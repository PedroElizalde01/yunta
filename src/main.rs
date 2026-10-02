// No console window when started from the Start menu or at login; subcommands open their own.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod clip;
mod config;
mod crossing;
mod fx;
mod icon;
mod keymap;
mod link;
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
            eprintln!("usage: yunta [init | pair | settings]");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("yunta: {e}");
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
        Err(e) => eprintln!("settings: {e}"),
    }
}

/// Pairing mode, then the other machine's key (and, on the dialing side, its address) saved.
fn pair() -> io::Result<()> {
    let cfg = config::init()?;
    let paired = pair::run(&cfg.public, &cfg.display_name())?;
    config::use_device(&cfg.path, &cfg, paired.device())?;
    println!("\nPaired with {} ({}). Start `yunta` on both machines.", paired.name, paired.addr);
    Ok(())
}

enum Role {
    Listen(TcpListener),
    Dial(String),
}

fn run() -> io::Result<()> {
    let mut cfg = config::init()?;
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
        // First run: pair before anything else. Started from a menu or at login, that happens
        // in the settings window, which starts us again once paired.
        if !io::stdin().is_terminal() {
            open_settings(Some("pairing"));
            return Ok(());
        }
        in_console(pair)?;
        cfg = config::load()?;
    }
    let peer_key = cfg.peer_key.clone().expect("paired");
    let role = match &cfg.peer {
        Some(host) => Role::Dial(host.clone()),
        // ponytail: IPv4 only, bind [::] as well if a network ever needs IPv6
        None => Role::Listen(TcpListener::bind(("0.0.0.0", cfg.port))?),
    };
    let (tx, rx) = mpsc::channel();
    let os = os::Os::start(tx.clone())?;
    let tray = os::Tray::start(tx.clone());
    let (key, port) = (cfg.key.clone(), cfg.port);
    thread::spawn(move || link_thread(role, port, key, peer_key, tx));
    eprintln!("yunta running, config {}", cfg.path.display());
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
                eprintln!("link down: {why}");
                if tx.send(Input::Down).is_err() {
                    return;
                }
            }
            Err(e) => {
                eprintln!("link: {e}");
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
    fullscreen: (Instant, bool),
    /// The parts of a pixel the peer's moves add up to, under a pointer speed that is not whole.
    rest: (f32, f32),
    /// When we last sent a wake-up.
    woke_at: Option<Instant>,
    /// While driven, where we put the pointer. Kept here rather than read back: on Windows a
    /// move is applied later, and reading the position at once can give the old one.
    pointer: (i32, i32),
    /// An app fills the peer's screen, so its edge is not crossed into.
    peer_busy: bool,
    /// What we last told the peer about our own full-screen app.
    busy: bool,
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
            fullscreen: (now - Duration::from_secs(1), false),
            rest: (0.0, 0.0),
            woke_at: None,
            pointer: (0, 0),
            peer_busy: false,
            busy: false,
            cfg,
        };
        core.apply_keep();
        core
    }

    fn run(mut self, rx: mpsc::Receiver<Input>) {
        self.show_status();
        self.write_status();
        loop {
            // A held trigger key switches at its deadline, so wake up for that too.
            let wait = self.tap.deadline().map_or(TICK, |d| Duration::from_millis(d.saturating_sub(self.now())).min(TICK));
            match rx.recv_timeout(wait) {
                Ok(Input::Quit) => {
                    self.shut_down();
                    if let Err(e) = std::fs::write(config::quit_path(&self.cfg.path), "") {
                        eprintln!("quit: {e}");
                    }
                    return;
                }
                Ok(input) => self.handle(input),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if self.tap.tick(self.now()) {
                self.switch();
            }
            // While an app fills our screen, the peer is told not to cross into it.
            let busy = self.link.is_some() && self.cfg.fullscreen && self.fullscreen();
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
            let at = modified(&self.cfg.path);
            if at != self.cfg_at {
                self.cfg_at = at;
                self.reload();
            }
            self.write_status();
        }
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
            Err(e) => return eprintln!("config: {e}"),
        };
        // Paired again, or asked to after an update: the simplest is to start over.
        if (&cfg.peer_key, &cfg.peer, cfg.port, cfg.restart) != (&self.cfg.peer_key, &self.cfg.peer, self.cfg.port, self.cfg.restart) {
            eprintln!("restarting");
            self.shut_down();
            if let Err(e) = exe().and_then(|exe| Command::new(exe).env(RESTART, "1").spawn()) {
                eprintln!("restart: {e}");
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

    /// Updates the entry for the computer in use in the list of paired ones, and saves it.
    fn note_device(&mut self, change: impl FnOnce(&mut config::Device)) {
        let Some(key) = self.cfg.peer_key.clone() else { return };
        let Some(device) = self.cfg.devices.iter_mut().find(|d| d.key == key) else { return };
        change(device);
        if let Err(e) = config::save_devices(&self.cfg.path, &self.cfg.devices) {
            eprintln!("devices: {e}");
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
            yes(self.peer_busy),
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
            eprintln!("status: {e}");
        }
        self.status = status;
        // The tray says the same, and a wake-up runs out without a message.
        self.show_status();
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Up(link, addr) => {
                eprintln!("linked");
                self.woke_at = None;
                self.link = Some(link);
                self.send(Msg::Displays(self.displays.clone()));
                self.send(Msg::Layout(self.cfg.layout));
                self.hello();
                self.busy = self.cfg.fullscreen && self.fullscreen();
                self.send(Msg::Busy(self.busy));
                // Fresh in the ARP table now, so this is the time to learn it for waking later.
                if let Some(mac) = addr.and_then(wake::mac_of)
                    && self.cfg.peer_mac != Some(mac)
                {
                    self.cfg.peer_mac = Some(mac);
                    if let Err(e) = config::set(&self.cfg.path, "peer_mac", Some(&config::mac_text(&mac))) {
                        eprintln!("peer_mac: {e}");
                    }
                }
                let mac = self.cfg.peer_mac;
                self.note_device(|d| {
                    d.seen = now_ms();
                    d.mac = mac.or(d.mac);
                });
                self.show_status();
            }
            Input::Down => {
                self.link = None;
                self.peer_receives = true;
                self.peer_busy = false;
                self.fall_back();
                self.show_status();
            }
            Input::Pause(paused) => {
                self.cfg.paused = paused;
                if let Err(e) = config::set(&self.cfg.path, "paused", Some(if paused { "yes" } else { "no" })) {
                    eprintln!("pause: {e}");
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
            Event::Motion { x, y, dx, dy, dragging } => match self.state {
                State::Driving => self.send(Msg::Move { dx: clamp16(dx), dy: clamp16(dy) }),
                // The edge stays shut while an app fills the peer's screen: no glow, and the
                // pointer stays here. The shortcut still switches.
                State::Local if !self.cfg.paused && self.can_drive() && !(self.link.is_some() && self.peer_busy) => {
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
            eprintln!("could not take the keyboard and mouse back after a kept key");
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
            Ok(()) => eprintln!("waking {}", config::mac_text(&mac)),
            Err(e) => eprintln!("wake: {e}"),
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
                    eprintln!("refused: this computer does not let the other one in");
                    return self.send(Msg::Leave { pos: self.cfg.layout.to_peer(pos) });
                }
                self.back = Crossing::new(edge, self.cfg.resistance);
                self.rest = (0.0, 0.0);
                self.pointer = crossing::entry_point(&self.displays, edge, pos).unwrap_or_else(|| self.os.cursor());
                self.os.move_to(self.pointer.0, self.pointer.1);
                self.arrive(self.pointer.0, self.pointer.1);
                self.state = State::Driven;
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
                    if let Err(e) = config::set(&self.cfg.path, "peer_name", Some(&name)) {
                        eprintln!("peer_name: {e}");
                    }
                    self.note_device(|d| d.name = name.clone());
                    self.cfg.peer_name = Some(name);
                }
                self.show_status();
            }
            Msg::Busy(busy) => self.peer_busy = busy,
            Msg::Displays(displays) => self.peer_displays = displays,
            Msg::Layout(theirs) => {
                // The newer arrangement wins. A tie, as when neither was ever set, goes to the
                // dialing machine's.
                let ours = self.cfg.layout;
                let dialer = self.cfg.peer.is_some();
                if theirs.mirror() != ours && (theirs.stamp > ours.stamp || (theirs.stamp == ours.stamp && !dialer)) {
                    eprintln!("layout: the other machine is on the {} side", theirs.mirror().edge.name());
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
            eprintln!("could not take the keyboard and mouse: another app is holding them");
            return;
        }
        self.state = State::Driving;
        self.send_clipboard();
        self.send(Msg::Enter { edge: self.cfg.layout.edge.opposite(), pos });
    }

    /// Uses a new arrangement, and saves it with `save`. Not sent: the caller decides whether the
    /// peer needs it.
    fn set_layout(&mut self, layout: crossing::Layout, save: bool) {
        self.cfg.layout = layout;
        self.out = Crossing::new(layout.edge, self.cfg.resistance);
        if save && let Err(e) = config::save_layout(&self.cfg.path, &layout) {
            eprintln!("layout: {e}");
        }
    }

    /// Stops driving. With `pos` the pointer comes back through our edge there, otherwise to
    /// where it left.
    fn come_home(&mut self, pos: Option<u16>) {
        for up in release(&mut self.held_there) {
            self.send(up);
        }
        self.os.grab(false);
        let (x, y) = pos.and_then(|p| crossing::entry_point(&self.displays, self.cfg.layout.edge, p)).unwrap_or(self.exit);
        self.os.move_to(x, y);
        self.arrive(x, y);
        self.state = State::Local;
    }

    /// Whether an app fills the screen here, asked at most a few times a second.
    fn fullscreen(&mut self) -> bool {
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
        let blocked =
            (layout.corner && !layout.corner_push(&self.displays, x, y, dx, dy)) || (!back && self.cfg.fullscreen && self.fullscreen());
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
            eprintln!("link: {e}");
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
