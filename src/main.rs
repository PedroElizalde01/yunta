// No console window when started from the Start menu or at login; subcommands open their own.
#![cfg_attr(windows, windows_subsystem = "windows")]

/// A line in the log: stderr, and yunta.log next to yunta.conf (see log.rs).
macro_rules! log {
    ($($t:tt)*) => {
        $crate::log::write(format_args!($($t)*))
    };
}

mod autostart;
mod clip;
mod config;
mod crossing;
mod fx;
mod icon;
mod keymap;
mod link;
mod log;
mod msg;
mod pair;
mod settings;
mod update;
mod wake;
mod widgets;

#[cfg(target_os = "linux")]
#[path = "os/linux.rs"]
mod os;
#[cfg(windows)]
#[path = "os/windows.rs"]
mod os;

use std::io::{self, IsTerminal};
use std::net::{IpAddr, TcpListener};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossing::{Crossing, Push, Rect, Trigger};
use msg::Msg;

/// Set on the copy a restart starts.
pub const RESTART: &str = "YUNTA_RESTART";
/// How far, in pixels, our own mouse moves while driven before this computer takes itself back.
const TAKE_BACK: i32 = 6;
/// The core wakes at least this often, to ping, poll and pick up config changes.
const TICK: Duration = Duration::from_millis(250);
/// How long the tray and window say a wake-up is under way.
const WAKING: Duration = Duration::from_secs(60);
const PING_EVERY: Duration = Duration::from_millis(500);
// ponytail: displays are polled, subscribe to RandR / WM_DISPLAYCHANGE if a 2s lag after replugging matters
const DISPLAYS_EVERY: Duration = Duration::from_secs(2);

/// Local input, from the OS backend.
pub enum Event {
    /// Raw pointer motion, and where the pointer is. The position only counts while not grabbed.
    Motion {
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        dragging: bool,
        /// Measured under the grab, so x and y mean nothing: only counts while driving.
        grabbed: bool,
    },
    Key {
        hid: u16,
        down: bool,
    },
    Button {
        button: u8,
        down: bool,
    },
    Scroll {
        dx: i16,
        dy: i16,
    },
    /// A touchpad swipe: fingers, and 0 up, 1 down, 2 left, 3 right.
    Gesture {
        fingers: u8,
        direction: u8,
    },
}

pub enum Input {
    Local(Event),
    Peer(Msg),
    /// The link is up, with the peer at this address when known.
    Up(link::Sender, Option<IpAddr>),
    Down,
    /// From the tray. The settings window writes `paused` into the config instead.
    Pause(bool),
    Quit,
}

fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        None => {
            os::attach_console();
            run()
        }
        Some("init") => in_console(|| {
            let cfg = config::init()?;
            println!("config: {}\nthis machine's public key: {}", cfg.path.display(), config::hex(&cfg.public));
            Ok(())
        }),
        Some("pair") => in_console(pair),
        Some("settings") => settings::run(std::env::args().nth(2).as_deref()),
        Some(_) => {
            log!("usage: yunta [init | pair | settings]");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        log!("yunta: {e}");
        std::process::exit(1);
    }
}

/// Runs `f` in a console window of its own on Windows, left open until Enter so it can be read.
fn in_console(f: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    let opened = os::open_console();
    let result = f();
    if opened {
        if let Err(e) = &result {
            println!("yunta: {e}");
        }
        println!("\nPress Enter to close this window.");
        let _ = io::stdin().read_line(&mut String::new());
        os::close_console();
    }
    result
}

/// This program's file. After an update replaced it on Linux, the kernel calls the running one
/// "yunta (deleted)", and the new file is at the same path without that.
pub fn exe() -> io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.to_str().and_then(|p| p.strip_suffix(" (deleted)")).map(Into::into).unwrap_or(exe))
}

/// Opens the settings window, on `page` if given, as a process of its own.
pub fn open_settings(page: Option<&str>) {
    let child = exe().and_then(|exe| Command::new(exe).arg("settings").args(page).spawn());
    match child {
        // Waited for, so it does not linger as a zombie on Linux.
        Ok(mut child) => drop(thread::spawn(move || child.wait())),
        Err(e) => log!("settings: {e}"),
    }
}

/// Pairing mode, then the other machine's key (and, on the dialing side, its address) saved.
fn pair() -> io::Result<()> {
    let cfg = config::init()?;
    let paired = pair::run(&cfg.public, &cfg.display_name())?;
    config::use_device(&cfg.path, paired.device())?;
    println!("\nPaired with {} ({}). Start `yunta` on both machines.", paired.name, paired.addr);
    Ok(())
}

enum Role {
    Listen(TcpListener),
    Dial(String),
}

fn run() -> io::Result<()> {
    let mut cfg = config::init()?;
    log::start(&cfg.path, "app");
    if !cfg.unknown.is_empty() {
        log!("yunta.conf has settings this version does not know, and skips: {}", cfg.unknown.join(", "));
    }
    if wayland() {
        log!("this is a Wayland session: Yunta needs X11 to see and take over the keyboard and mouse, so it will not work here");
    }
    update::tidy();
    let _ = std::fs::remove_file(config::quit_path(&cfg.path));
    // Held until we exit: a second copy would open a second link, or fight over the input.
    // A copy restarting itself lets go of the lock as it exits, so the new one waits for that.
    let wait = if std::env::var_os(RESTART).is_some() { Duration::from_secs(3) } else { Duration::ZERO };
    let deadline = Instant::now() + wait;
    let _lock = loop {
        match config::lock(&cfg.path, "yunta.lock")? {
            Some(lock) => break lock,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            // Started again from a menu: what the user wants is the window.
            None if !io::stdin().is_terminal() => {
                open_settings(None);
                return Ok(());
            }
            None => return Err(io::Error::other("already running")),
        }
    };
    if cfg.peer_key.is_none() {
        // Not paired: in a terminal, pair there first. Started from a menu or at login, stay in
        // the tray as "Not paired" and open the window to pair; pairing there restarts us.
        if io::stdin().is_terminal() {
            in_console(pair)?;
            cfg = config::load()?;
        } else {
            open_settings(Some("pairing"));
        }
    }
    let (tx, rx) = mpsc::channel();
    let os = os::Os::start(tx.clone())?;
    let tray = os::Tray::start(tx.clone());
    if let Some(peer_key) = cfg.peer_key.clone() {
        let role = match &cfg.peer {
            Some(host) => Role::Dial(host.clone()),
            // ponytail: IPv4 only, bind [::] as well if a network ever needs IPv6
            None => Role::Listen(TcpListener::bind(("0.0.0.0", cfg.port))?),
        };
        let (key, port) = (cfg.key.clone(), cfg.port);
        thread::spawn(move || link_thread(role, port, key, peer_key, tx));
    }
    log!("yunta running, config {}", cfg.path.display());
    Core::new(os, tray, cfg).run(rx);
    Ok(())
}

/// Keeps one link up: dials the peer, or accepts it, and forwards what it says to the core.
fn link_thread(role: Role, port: u16, key: Vec<u8>, peer_key: Vec<u8>, tx: mpsc::Sender<Input>) {
    loop {
        // ponytail: handshakes run one at a time, so a stranger can stall a (re)connect by up to 2s
        let conn = match &role {
            Role::Listen(listener) => {
                listener.accept().and_then(|(stream, addr)| link::accept(stream, &key, &peer_key).map(|(s, r)| (s, r, Some(addr.ip()))))
            }
            Role::Dial(host) => link::connect((host.as_str(), port), &key, &peer_key).map(|(s, r)| (s, r, host.parse().ok())),
        };
        match conn {
            Ok((sender, mut receiver, addr)) => {
                if tx.send(Input::Up(sender, addr)).is_err() {
                    return;
                }
                let why = loop {
                    match receiver.recv().map(|bytes| Msg::decode(&bytes)) {
                        Ok(Some(msg)) => {
                            if tx.send(Input::Peer(msg)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break "the peer sent a message this version does not know".to_string(),
                        Err(e) => break e.to_string(),
                    }
                };
                log!("link down: {why}");
                if tx.send(Input::Down).is_err() {
                    return;
                }
            }
            Err(e) => {
                log!("link: {e}");
                if tx.send(Input::Down).is_err() {
                    return;
                }
            }
        }
        if let Role::Dial(_) = role {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

#[derive(Debug, PartialEq)]
enum State {
    Local,
    /// Our input goes to the peer.
    Driving,
    /// The peer's input comes to us.
    Driven,
}

struct Core {
    os: os::Os,
    cfg: config::Config,
    link: Option<link::Sender>,
    state: State,
    displays: Vec<Rect>,
    peer_displays: Vec<Rect>,
    displays_at: Instant,
    last_send: Instant,
    start: Instant,
    /// Our edge towards the peer.
    out: Crossing,
    /// While driven, the edge the pointer came in by.
    back: Crossing,
    tap: Trigger,
    /// Where our pointer was when we started driving.
    exit: (i32, i32),
    /// Keys and buttons we pressed on the peer, and the peer pressed here, still down.
    /// Released on every switch and disconnect, so nothing stays stuck.
    held_there: Vec<Msg>,
    held_here: Vec<Msg>,
    clip: Option<clip::Clip>,
    tray: Option<os::Tray>,
    /// When yunta.conf last changed: the settings window writes it, and we pick that up.
    cfg_at: Option<SystemTime>,
    /// What the status file says now, so it is only written when that changes.
    status: String,
    /// The crossing animations, when on and possible here.
    fx: Option<fx::Fx>,
    /// An edge glow is showing, and needs putting out when the push stops.
    glowing: bool,
    /// The peer lets our keyboard and mouse in. Assumed until its Hello says otherwise.
    peer_receives: bool,
    /// Whether an app is full screen here, and when that was last asked.
    fullscreen: (Instant, Option<Rect>),
    /// The parts of a pixel the peer's moves add up to, under a pointer speed that is not whole.
    rest: (f32, f32),
    /// When we last sent a wake-up.
    woke_at: Option<Instant>,
    /// While driven, where we put the pointer. Kept here rather than read back: on Windows a
    /// move is applied later, and reading the position at once can give the old one.
    pointer: (i32, i32),
    /// While driven: when our own mouse started moving, and how far it has gone since.
    moved: (Instant, i32),
    /// Moves sent while driving and taken in while driven, since the last line in the log.
    moves: u32,
    reported: Instant,
    /// An app fills the peer's screen, so its edge is not crossed into.
    peer_busy: Option<Rect>,
    /// What we last told the peer about our own full-screen app.
    busy: Option<Rect>,
}

impl Core {
    fn new(os: os::Os, tray: Option<os::Tray>, cfg: config::Config) -> Core {
        let now = Instant::now();
        let core = Core {
            displays: os.displays(),
            peer_displays: vec![],
            os,
            link: None,
            state: State::Local,
            displays_at: now,
            last_send: now,
            start: now,
            out: Crossing::new(cfg.layout.edge, cfg.resistance),
            back: Crossing::new(cfg.layout.edge.opposite(), cfg.resistance),
            tap: Trigger::new(cfg.hotkey, 300, cfg.hold),
            exit: (0, 0),
            held_there: vec![],
            held_here: vec![],
            clip: clip::Clip::new(),
            tray,
            cfg_at: modified(&cfg.path),
            status: String::new(),
            fx: if cfg.effects { fx::Fx::start() } else { None },
            glowing: false,
            peer_receives: true,
            fullscreen: (now - Duration::from_secs(1), None),
            rest: (0.0, 0.0),
            woke_at: None,
            pointer: (0, 0),
            moved: (now, 0),
            moves: 0,
            reported: now,
            peer_busy: None,
            busy: None,
            cfg,
        };
        core.apply_keep();
        core
    }

    fn run(mut self, rx: mpsc::Receiver<Input>) {
        beat();
        self.show_status();
        self.write_status();
        loop {
            // A held trigger key switches at its deadline, so wake up for that too.
            let wait = self.tap.deadline().map_or(TICK, |d| Duration::from_millis(d.saturating_sub(self.now())).min(TICK));
            match rx.recv_timeout(wait) {
                Ok(Input::Quit) => {
                    self.shut_down();
                    if let Err(e) = std::fs::write(config::quit_path(&self.cfg.path), "") {
                        log!("quit: {e}");
                    }
                    return;
                }
                Ok(input) => self.handle(input),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            beat();
            // The watchdog let go while we hung: input is back here, so we are too.
            if self.state == State::Driving && !self.os.is_grabbed() {
                log!("the keyboard and mouse were given back while driving: coming home");
                self.come_home(None);
            }
            if self.tap.tick(self.now()) {
                self.switch();
            }
            // While an app fills our screen, the peer is told not to cross into it.
            let busy = if self.link.is_some() && self.cfg.fullscreen { self.fullscreen() } else { None };
            if busy != self.busy {
                self.busy = busy;
                self.send(Msg::Busy(busy));
            }
            if self.last_send.elapsed() >= PING_EVERY {
                self.send(Msg::Ping);
            }
            if self.displays_at.elapsed() >= DISPLAYS_EVERY {
                let displays = self.os.displays();
                if displays != self.displays {
                    self.displays = displays;
                    self.send(Msg::Displays(self.displays.clone()));
                }
                self.displays_at = Instant::now();
            }
            self.report();
            let at = modified(&self.cfg.path);
            if at != self.cfg_at {
                self.cfg_at = at;
                self.reload();
            }
            self.write_status();
        }
    }

    /// Once a second while input crosses, a line on how it flows, to see where it stops.
    fn report(&mut self) {
        if self.state == State::Local || self.reported.elapsed() < Duration::from_secs(1) {
            return;
        }
        let (seen, drift) = self.os.hook_stats();
        match self.state {
            State::Driving => log!("driving: {} moves sent; the mouse hook swallowed {seen}, {drift} of them off the park", self.moves),
            _ => log!("driven: {} moves taken in, pointer at {:?}", self.moves, self.pointer),
        }
        (self.moves, self.reported) = (0, Instant::now());
    }

    /// Milliseconds since we started, the trigger key's clock.
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Gives input back, and takes the tray icon and status file away.
    fn shut_down(&mut self) {
        self.fall_back();
        if let Some(tray) = &self.tray {
            tray.remove();
        }
        let _ = std::fs::remove_file(config::status_path(&self.cfg.path));
    }

    /// Takes up what changed in yunta.conf.
    fn reload(&mut self) {
        let cfg = match config::load() {
            Ok(cfg) => cfg,
            Err(e) => return log!("config: {e}"),
        };
        // Paired again, or asked to after an update: the simplest is to start over.
        if (&cfg.peer_key, &cfg.peer, cfg.port, cfg.restart) != (&self.cfg.peer_key, &self.cfg.peer, self.cfg.port, self.cfg.restart) {
            log!("restarting");
            self.shut_down();
            if let Err(e) = exe().and_then(|exe| Command::new(exe).env(RESTART, "1").spawn()) {
                log!("restart: {e}");
            }
            std::process::exit(0);
        }
        let old = self.cfg.layout;
        let hello = (self.cfg.receive, self.cfg.display_name());
        let effects = self.cfg.effects;
        self.cfg = cfg;
        if self.cfg.effects != effects {
            self.fx = if self.cfg.effects { fx::Fx::start() } else { None };
        }
        self.back.resistance = self.cfg.resistance;
        self.tap = Trigger::new(self.cfg.hotkey, 300, self.cfg.hold);
        self.apply_keep();
        let mut layout = self.cfg.layout;
        self.set_layout(layout, false);
        if layout != old {
            // Edited by hand, with no new time on it: stamp it now, so it wins over the peer's.
            if layout.stamp <= old.stamp {
                layout.stamp = now_ms();
                self.set_layout(layout, true);
            }
            self.send(Msg::Layout(layout));
        }
        if hello != (self.cfg.receive, self.cfg.display_name()) {
            self.hello();
        }
        self.show_status();
    }

    /// Tells the OS backend which keys and buttons stay here while driving.
    fn apply_keep(&self) {
        let keys: Vec<u16> = self.cfg.keep.iter().flat_map(|g| keymap::kept_keys(g)).copied().collect();
        let buttons: Vec<u8> = self.cfg.keep.iter().flat_map(|g| keymap::kept_buttons(g)).copied().collect();
        self.os.keep(&keys, &buttons);
    }

    fn kept(&self, msg: &Msg) -> bool {
        self.cfg.keep.iter().any(|g| match *msg {
            Msg::Key { hid, .. } => keymap::kept_keys(g).contains(&hid),
            Msg::Button { button, .. } => keymap::kept_buttons(g).contains(&button),
            _ => false,
        })
    }

    /// Saves what we learnt about the computer in use: its entry in the list of paired ones, and
    /// `setting` (its hardware address or name) when there is one, in one write.
    fn note_peer(&mut self, setting: Option<(&str, &str)>, change: impl FnOnce(&mut config::Device)) {
        let Some(key) = self.cfg.peer_key.clone() else { return };
        let saved = config::update(&self.cfg.path, |doc| {
            if let Some((name, value)) = setting {
                doc.set(name, Some(value));
            }
            config::change_device(doc, &key, change)
        });
        if let Err(e) = saved {
            log!("saving what we know of the other computer: {e}");
        }
    }

    fn hello(&mut self) {
        self.send(Msg::Hello { receive: self.cfg.receive, name: self.cfg.display_name() });
    }

    fn waking(&self) -> bool {
        self.link.is_none() && self.woke_at.is_some_and(|t| t.elapsed() < WAKING)
    }

    /// Keeps the status file in step for the settings window.
    fn write_status(&mut self) {
        let input = match self.state {
            State::Local => "here",
            State::Driving => "there",
            State::Driven => "visiting",
        };
        let yes = |b: bool| if b { "yes" } else { "no" };
        let status = format!(
            "linked = {}\ninput = {input}\npaused = {}\nwaking = {}\npeer_receives = {}\npeer_busy = {}\ndisplays = {}\npeer_displays = {}\n",
            yes(self.link.is_some()),
            yes(self.cfg.paused),
            yes(self.waking()),
            yes(self.peer_receives),
            yes(self.peer_busy.is_some()),
            Rect::list_text(&self.displays),
            Rect::list_text(&self.peer_displays)
        );
        if status == self.status {
            return;
        }
        // Written aside and renamed, so the window never reads half a file.
        let path = config::status_path(&self.cfg.path);
        let aside = path.with_extension("new");
        if let Err(e) = std::fs::write(&aside, &status).and_then(|()| std::fs::rename(&aside, &path)) {
            log!("status: {e}");
        }
        self.status = status;
        // The tray says the same, and a wake-up runs out without a message.
        self.show_status();
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Up(link, addr) => {
                log!("linked");
                self.woke_at = None;
                self.link = Some(link);
                self.send(Msg::Displays(self.displays.clone()));
                self.send(Msg::Layout(self.cfg.layout));
                self.hello();
                self.busy = if self.cfg.fullscreen { self.fullscreen() } else { None };
                self.send(Msg::Busy(self.busy));
                // Fresh in the ARP table now, so this is the time to learn it for waking later.
                if let Some(mac) = addr.and_then(wake::mac_of) {
                    self.cfg.peer_mac = Some(mac);
                }
                // Every connection counts as seen; the hardware address only when it was found.
                let mac = self.cfg.peer_mac;
                let mac_text = mac.map(|m| config::mac_text(&m));
                self.note_peer(mac_text.as_deref().map(|m| ("peer_mac", m)), |d| {
                    d.seen = now_ms();
                    d.mac = mac.or(d.mac);
                });
                self.show_status();
            }
            Input::Down => {
                self.link = None;
                self.peer_receives = true;
                self.peer_busy = None;
                self.fall_back();
                self.show_status();
            }
            Input::Pause(paused) => {
                self.cfg.paused = paused;
                if let Err(e) = config::set(&self.cfg.path, "paused", Some(if paused { "yes" } else { "no" })) {
                    log!("pause: {e}");
                }
                self.show_status();
            }
            Input::Quit => {}
            Input::Local(event) => self.local(event),
            Input::Peer(msg) => self.peer(msg),
        }
    }

    /// Our keyboard and mouse may go over now: allowed here and there, and the link up, or a
    /// sleeping peer we can wake.
    fn can_drive(&self) -> bool {
        self.cfg.send && if self.link.is_some() { self.peer_receives } else { self.cfg.wake && self.cfg.peer_mac.is_some() }
    }

    fn local(&mut self, event: Event) {
        match event {
            Event::Motion { x, y, dx, dy, dragging, grabbed } => match self.state {
                State::Driving => {
                    self.moves += 1;
                    self.send(Msg::Move { dx: clamp16(dx), dy: clamp16(dy) })
                }
                // Still queued from the grab after coming home: its (0, 0) is no place to push from.
                _ if grabbed => {}
                // Our moves while driven are warps (X11) or marked as ours (Windows), so motion
                // seen here is this computer's own mouse.
                State::Driven => self.take_back(dx, dy),
                // The edge stays shut while an app fills the peer's screen: no glow, and the
                // pointer stays here. The shortcut still switches.
                State::Local if !self.cfg.paused && self.can_drive() && !self.lands_in_busy(x, y) => {
                    if let Some(pos) = self.push(false, x, y, dx, dy, dragging) {
                        if self.link.is_some() {
                            self.drive(self.cfg.layout.to_peer(pos));
                        } else {
                            self.wake();
                        }
                    }
                }
                // Shut while a glow was building: let it fade rather than hang there.
                State::Local if self.glowing => {
                    self.glowing = false;
                    self.out.reset();
                    if let Some(fx) = &self.fx {
                        fx.push(self.out.edge, x, y, 0.0);
                    }
                }
                _ => {}
            },
            // While driven, what arrives here includes the keys we inject for the peer.
            Event::Key { hid, down } if self.state != State::Driven => {
                if self.state == State::Driving {
                    let key = Msg::Key { hid: if self.cfg.swap_modifiers { keymap::swap_modifiers(hid) } else { hid }, down };
                    if self.kept(&Msg::Key { hid, down }) {
                        self.play_local(Msg::Key { hid, down });
                    } else {
                        self.forward(key);
                    }
                }
                if self.tap.key(hid, down, self.now()) {
                    self.switch();
                }
            }
            Event::Button { button, down } if self.state == State::Driving => {
                let msg = Msg::Button { button, down };
                if self.kept(&msg) { self.play_local(msg) } else { self.forward(msg) }
            }
            Event::Scroll { dx, dy } if self.state == State::Driving => self.send(Msg::Scroll { dx, dy }),
            Event::Gesture { fingers, direction } if self.state == State::Driving && self.cfg.gestures => {
                self.send(Msg::Gesture { fingers, direction })
            }
            _ => {}
        }
    }

    /// A kept key or button, played here. If the grab could not be taken back, input comes home.
    fn play_local(&mut self, msg: Msg) {
        if !self.os.play_local(&msg) {
            log!("could not take the keyboard and mouse back after a kept key");
            self.send(Msg::Leave { pos: 0 });
            self.come_home(None);
        }
    }

    /// The trigger key: input goes to the other side, or comes home.
    fn switch(&mut self) {
        match self.state {
            State::Driving => {
                self.send(Msg::Leave { pos: 0 });
                self.come_home(None);
            }
            State::Local if self.can_drive() => {
                if self.link.is_none() {
                    return self.wake();
                }
                let (x, y) = self.os.cursor();
                let pos = crossing::pos_along(&self.displays, self.cfg.layout.edge, x, y);
                self.drive(self.cfg.layout.to_peer(pos));
            }
            _ => {}
        }
    }

    /// Sends the peer a wake-up, at most every few seconds.
    fn wake(&mut self) {
        let Some(mac) = self.cfg.peer_mac else { return };
        if self.woke_at.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.woke_at = Some(Instant::now());
        match wake::send(&mac) {
            Ok(()) => log!("waking {}", config::mac_text(&mac)),
            Err(e) => log!("wake: {e}"),
        }
        self.show_status();
    }

    fn peer(&mut self, msg: Msg) {
        match msg {
            Msg::Enter { edge, pos } => {
                if self.state == State::Driving {
                    // Both crossed at once. The dialing machine gives way; the listener ignores
                    // this Enter, because the dialer is about to take its own back.
                    if self.cfg.peer.is_none() {
                        return;
                    }
                    self.come_home(None);
                }
                // Not allowed in: back it goes, to where it came from.
                if !self.cfg.receive {
                    log!("refused: this computer does not let the other one in");
                    return self.send(Msg::Leave { pos: self.cfg.layout.to_peer(pos) });
                }
                self.back = Crossing::new(edge, self.cfg.resistance);
                self.rest = (0.0, 0.0);
                self.pointer = crossing::entry_point(&self.displays, edge, pos).unwrap_or_else(|| self.os.cursor());
                self.os.place(self.pointer.0, self.pointer.1);
                self.arrive(self.pointer.0, self.pointer.1);
                self.state = State::Driven;
                (self.moves, self.reported) = (0, Instant::now());
                log!("the other computer came in through the {} edge, pointer to {:?}", edge.name(), self.pointer);
            }
            Msg::Leave { pos } => match self.state {
                State::Driving => self.come_home(Some(pos)),
                State::Driven => {
                    self.let_go();
                    self.send_clipboard();
                }
                State::Local => {}
            },
            Msg::Move { dx, dy } if self.state == State::Driven => {
                self.moves += 1;
                // Scaled by this machine's pointer speed for the peer, keeping the fractions.
                let speed = self.cfg.pointer_speed;
                let (fx, fy) = (f32::from(dx) * speed + self.rest.0, f32::from(dy) * speed + self.rest.1);
                let (dx, dy) = (fx.trunc(), fy.trunc());
                self.rest = (fx - dx, fy - dy);
                let (dx, dy) = (dx as i32, dy as i32);
                let (x, y) = self.pointer;
                let (x, y) = crossing::step_within(&self.displays, x, y, x + dx, y + dy);
                self.pointer = (x, y);
                self.os.move_to(x, y);
                let dragging = self.held_here.iter().any(|m| matches!(m, Msg::Button { .. }));
                if let Some(pos) = self.push(true, x, y, dx, dy, dragging) {
                    log!("the pointer went back out through the edge");
                    self.let_go();
                    self.send_clipboard();
                    self.send(Msg::Leave { pos: self.cfg.layout.to_peer(pos) });
                }
            }
            Msg::Scroll { dx, dy } if self.state == State::Driven => {
                let scale = |v: i16| clamp16((f32::from(v) * self.cfg.scroll_speed).round() as i32);
                self.os.inject(&Msg::Scroll { dx: scale(dx), dy: scale(dy) });
            }
            Msg::Key { .. } | Msg::Button { .. } if self.state == State::Driven => {
                track(&mut self.held_here, &msg);
                self.os.inject(&msg);
            }
            Msg::Gesture { fingers, direction } if self.state == State::Driven && cfg!(windows) => {
                let keys = keymap::gesture_keys(fingers, direction);
                for &hid in keys.iter().chain(keys.iter().rev()) {
                    let down = !self.held_here.contains(&Msg::Key { hid, down: true });
                    let key = Msg::Key { hid, down };
                    track(&mut self.held_here, &key);
                    self.os.inject(&key);
                }
            }
            Msg::Hello { receive, name } => {
                self.peer_receives = receive;
                if self.cfg.peer_name.as_deref() != Some(name.as_str()) && !name.is_empty() {
                    let name = config::clean_name(&name);
                    self.note_peer(Some(("peer_name", &name)), |d| d.name = name.clone());
                    self.cfg.peer_name = Some(name);
                }
                self.show_status();
            }
            Msg::Busy(busy) => {
                log!("the other computer's full-screen app: {busy:?}");
                self.peer_busy = busy
            }
            Msg::TakeBack if self.state == State::Driving => {
                log!("the other computer's own mouse moved: coming home");
                self.come_home_to(None, false)
            }
            Msg::Displays(displays) => self.peer_displays = displays,
            Msg::Layout(theirs) => {
                // The newer arrangement wins. A tie, as when neither was ever set, goes to the
                // dialing machine's.
                let ours = self.cfg.layout;
                let dialer = self.cfg.peer.is_some();
                if theirs.mirror() != ours && (theirs.stamp > ours.stamp || (theirs.stamp == ours.stamp && !dialer)) {
                    log!("layout: the other machine is on the {} side", theirs.mirror().edge.name());
                    self.set_layout(theirs.mirror(), true);
                }
            }
            Msg::ClipText(_) | Msg::ClipPng(_) => {
                if let Some(clip) = &mut self.clip {
                    clip.put(&msg);
                }
            }
            _ => {}
        }
    }

    fn drive(&mut self, pos: u16) {
        // Before the grab, which parks the cursor on Windows.
        self.exit = self.os.cursor();
        if !self.os.grab(true) {
            log!("could not take the keyboard and mouse: another app is holding them");
            return;
        }
        self.state = State::Driving;
        (self.moves, self.reported) = (0, Instant::now());
        log!("driving the other computer, in at {pos} along its edge; our pointer left from {:?}", self.exit);
        self.send_clipboard();
        self.send(Msg::Enter { edge: self.cfg.layout.edge.opposite(), pos });
    }

    /// Uses a new arrangement, and saves it with `save`. Not sent: the caller decides whether the
    /// peer needs it.
    fn set_layout(&mut self, layout: crossing::Layout, save: bool) {
        self.cfg.layout = layout;
        self.out = Crossing::new(layout.edge, self.cfg.resistance);
        if save && let Err(e) = config::save_layout(&self.cfg.path, &layout) {
            log!("layout: {e}");
        }
    }

    /// Stops driving. With `pos` the pointer comes back through our edge there, otherwise to
    /// where it left.
    fn come_home(&mut self, pos: Option<u16>) {
        self.come_home_to(pos, true);
    }

    /// As `come_home`, with the landing ripple only when `show`.
    fn come_home_to(&mut self, pos: Option<u16>, show: bool) {
        for up in release(&mut self.held_there) {
            self.send(up);
        }
        let (x, y) = pos.and_then(|p| crossing::entry_point(&self.displays, self.cfg.layout.edge, p)).unwrap_or(self.exit);
        log!("home, pointer at {:?}", (x, y));
        // Placed before letting go, so the pointer never shows where it was parked.
        self.os.place(x, y);
        self.os.grab(false);
        if show {
            self.arrive(x, y);
        }
        self.state = State::Local;
    }

    /// While driven, our own mouse moved: whoever sits here wants this computer back. A few
    /// pixels within a moment count, so a nudged desk does not.
    fn take_back(&mut self, dx: i32, dy: i32) {
        if self.moved.0.elapsed() > Duration::from_millis(300) {
            self.moved = (Instant::now(), 0);
        }
        self.moved.1 += dx.abs() + dy.abs();
        if self.moved.1 >= TAKE_BACK {
            log!("this computer's own mouse moved: taking it back");
            self.moved.1 = 0;
            self.let_go();
            self.send(Msg::TakeBack);
        }
    }

    /// True when crossing at (x, y) would land inside a full-screen app on the peer. With the
    /// peer's screens not known yet, any full-screen app there counts.
    fn lands_in_busy(&self, x: i32, y: i32) -> bool {
        let Some(busy) = self.peer_busy.filter(|_| self.link.is_some()) else { return false };
        let layout = self.cfg.layout;
        let pos = layout.to_peer(crossing::pos_along(&self.displays, layout.edge, x, y));
        crossing::entry_point(&self.peer_displays, layout.edge.opposite(), pos).is_none_or(|(px, py)| busy.contains(px, py))
    }

    /// Where an app fills the screen here, asked at most a few times a second.
    fn fullscreen(&mut self) -> Option<Rect> {
        if self.fullscreen.0.elapsed() >= Duration::from_millis(300) {
            self.fullscreen = (Instant::now(), self.os.fullscreen());
        }
        self.fullscreen.1
    }

    /// Feeds a move to our edge towards the peer (`back`: the way home while driven) and keeps
    /// the glow on it. Where along the edge the pointer crossed, if it did.
    fn push(&mut self, back: bool, x: i32, y: i32, dx: i32, dy: i32, dragging: bool) -> Option<u16> {
        let layout = self.cfg.layout;
        // A corner takes a push into the corner itself; a full-screen app keeps the edge for itself.
        let blocked = (layout.corner && !layout.corner_push(&self.displays, x, y, dx, dy))
            || (!back && self.cfg.fullscreen && self.fullscreen().is_some_and(|r| r.contains(x, y)));
        let crossing = if back { &mut self.back } else { &mut self.out };
        let edge = crossing.edge;
        let push = if blocked {
            crossing.reset();
            Push::None
        } else {
            crossing.motion(&self.displays, x, y, dx, dy, dragging)
        };
        // Only the stretch of edge that touches the peer glows, as only it crosses.
        let pos = crossing::pos_along(&self.displays, edge, x, y);
        let touching = layout.covers(pos);
        let was_glowing = std::mem::replace(&mut self.glowing, false);
        match (push, &self.fx) {
            (Push::Cross(pos), fx) if touching => {
                if let Some(fx) = fx {
                    fx.leave(edge, x, y);
                }
                return Some(pos);
            }
            (Push::Building(strength), Some(fx)) if touching => {
                fx.push(edge, x, y, strength);
                self.glowing = true;
            }
            (_, Some(fx)) if was_glowing => fx.push(edge, x, y, 0.0),
            _ => {}
        }
        None
    }

    fn arrive(&self, x: i32, y: i32) {
        if let Some(fx) = &self.fx {
            fx.arrive(x, y);
        }
    }

    fn show_status(&self) {
        let Some(tray) = &self.tray else { return };
        let peer = self.cfg.peer_name.clone().unwrap_or_else(|| "the other computer".into());
        // Offline whenever the other computer is not there; amber only while a wake-up is under way.
        let look = icon::Look::for_link(self.link.is_some(), self.waking(), self.cfg.paused);
        let status = match look {
            icon::Look::Offline if self.cfg.peer_key.is_none() => "Not paired: click to pair".to_string(),
            icon::Look::Waiting => format!("Waking {peer}"),
            icon::Look::Offline => "Offline".to_string(),
            icon::Look::Linked => format!("Connected to {peer}"),
            icon::Look::Paused => "Connected, crossing paused".to_string(),
        };
        tray.show(look, &status);
    }

    /// Whichever machine gives input away sends its clipboard, ahead of the switch itself.
    fn send_clipboard(&mut self) {
        if let Some(msg) = self.clip.as_mut().and_then(clip::Clip::take) {
            self.send(msg);
        }
    }

    /// Stops being driven.
    fn let_go(&mut self) {
        for up in release(&mut self.held_here) {
            self.os.inject(&up);
        }
        self.state = State::Local;
    }

    /// The link is gone: input goes back to where it physically is.
    fn fall_back(&mut self) {
        match self.state {
            State::Driving => self.come_home(None),
            State::Driven => self.let_go(),
            State::Local => {}
        }
    }

    fn forward(&mut self, msg: Msg) {
        track(&mut self.held_there, &msg);
        self.send(msg);
    }

    fn send(&mut self, msg: Msg) {
        self.last_send = Instant::now();
        let Some(link) = &mut self.link else { return };
        if let Err(e) = link.send(&msg.encode()) {
            log!("link: {e}");
            self.link = None;
            self.fall_back();
        }
    }
}

/// Records a key or button going down or up in `held`.
fn track(held: &mut Vec<Msg>, msg: &Msg) {
    let (pressed, down) = match *msg {
        Msg::Key { hid, down } => (Msg::Key { hid, down: true }, down),
        Msg::Button { button, down } => (Msg::Button { button, down: true }, down),
        _ => return,
    };
    held.retain(|m| *m != pressed);
    if down {
        held.push(pressed);
    }
}

/// Empties `held`, returning the releases for everything in it.
fn release(held: &mut Vec<Msg>) -> Vec<Msg> {
    held.drain(..)
        .map(|m| match m {
            Msg::Key { hid, .. } => Msg::Key { hid, down: false },
            Msg::Button { button, .. } => Msg::Button { button, down: false },
            m => m,
        })
        .collect()
}

/// When the core last went round its loop, in milliseconds since `CLOCK` started.
static BEAT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// A core silent this long while driving is taken to be stuck, and the OS layer gives this
/// computer's keyboard and mouse back on its own, rather than leave them held.
pub const WATCHDOG: Duration = Duration::from_secs(5);

fn clock_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The core is alive.
fn beat() {
    BEAT.store(clock_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// How long since the core last went round its loop.
pub fn core_silent() -> Duration {
    Duration::from_millis(clock_ms().saturating_sub(BEAT.load(std::sync::atomic::Ordering::Relaxed)))
}

/// A Linux desktop on Wayland, where no app may watch or take over global input.
pub fn wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn clamp16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_connection_reports_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || link_thread(Role::Dial("127.0.0.1".into()), port, vec![], vec![], tx));
        assert!(matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Input::Down));
        drop(rx);
        worker.join().unwrap();
    }

    /// Plays the other computer driving a copy of Yunta that is already running: comes in, then
    /// moves the pointer. Run by hand, with YUNTA_PEER_CONFIG set to the config of the computer
    /// to play and YUNTA_DRIVE to the running copy's address:
    /// `cargo test --release -- --ignored drive_a_running_copy`.
    #[test]
    #[ignore]
    fn drive_a_running_copy() {
        let me = config::load_from(Path::new(&std::env::var("YUNTA_PEER_CONFIG").unwrap())).unwrap();
        let (mut tx, _rx) = link::connect(std::env::var("YUNTA_DRIVE").unwrap().as_str(), &me.key, me.peer_key.as_ref().unwrap()).unwrap();
        let mut send = |m: Msg| tx.send(&m.encode()).unwrap();
        send(Msg::Enter { edge: me.layout.edge.opposite(), pos: 32768 });
        for _ in 0..40 {
            send(Msg::Move { dx: 0, dy: -5 });
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(1500));
        send(Msg::Leave { pos: 32768 });
        thread::sleep(Duration::from_millis(200));
    }

    #[test]
    fn held_keys_are_released() {
        let mut held = vec![];
        for m in [
            Msg::Key { hid: 0xE0, down: true },
            Msg::Key { hid: 0x06, down: true },
            Msg::Key { hid: 0xE0, down: true },
            Msg::Key { hid: 0x06, down: false },
            Msg::Button { button: 1, down: true },
        ] {
            track(&mut held, &m);
        }
        assert_eq!(release(&mut held), vec![Msg::Key { hid: 0xE0, down: false }, Msg::Button { button: 1, down: false }]);
        assert!(held.is_empty());
    }
}
