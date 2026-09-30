mod clip;
mod config;
mod crossing;
mod keymap;
mod link;
mod msg;

#[cfg(target_os = "linux")]
#[path = "os/linux.rs"]
mod os;
#[cfg(windows)]
#[path = "os/windows.rs"]
mod os;

use std::io;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossing::{Crossing, DoubleTap, Push, Rect};
use msg::Msg;

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
}

pub enum Input {
    Local(Event),
    Peer(Msg),
    Up(link::Sender),
    Down,
}

fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        None => run(),
        Some("init") => config::init().map(|cfg| {
            println!("config: {}\nthis machine's public key: {}", cfg.path.display(), config::hex(&cfg.public));
        }),
        Some(_) => {
            eprintln!("usage: yunta [init]");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("yunta: {e}");
        std::process::exit(1);
    }
}

enum Role {
    Listen(TcpListener),
    Dial(String),
}

fn run() -> io::Result<()> {
    let cfg = config::load()?;
    let Some(peer_key) = cfg.peer_key.clone() else {
        return Err(io::Error::other(format!("set peer_key in {} to the other machine's public key", cfg.path.display())));
    };
    let role = match &cfg.peer {
        Some(host) => Role::Dial(host.clone()),
        // ponytail: IPv4 only, bind [::] as well if a network ever needs IPv6
        None => Role::Listen(TcpListener::bind(("0.0.0.0", cfg.port))?),
    };
    let (tx, rx) = mpsc::channel();
    let os = os::Os::start(tx.clone())?;
    let (key, port) = (cfg.key.clone(), cfg.port);
    thread::spawn(move || link_thread(role, port, key, peer_key, tx));
    eprintln!("yunta running, config {}", cfg.path.display());
    Core::new(os, cfg).run(rx);
    Ok(())
}

/// Keeps one link up: dials the peer, or accepts it, and forwards what it says to the core.
fn link_thread(role: Role, port: u16, key: Vec<u8>, peer_key: Vec<u8>, tx: mpsc::Sender<Input>) {
    loop {
        // ponytail: handshakes run one at a time, so a stranger can stall a (re)connect by up to 2s
        let conn = match &role {
            Role::Listen(listener) => listener.accept().and_then(|(stream, _)| link::accept(stream, &key, &peer_key)),
            Role::Dial(host) => link::connect((host.as_str(), port), &key, &peer_key),
        };
        match conn {
            Ok((sender, mut receiver)) => {
                if tx.send(Input::Up(sender)).is_err() {
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
            Err(e) => eprintln!("link: {e}"),
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
    displays_at: Instant,
    last_send: Instant,
    start: Instant,
    /// Our edge towards the peer.
    out: Crossing,
    /// While driven, the edge the pointer came in by.
    back: Crossing,
    tap: DoubleTap,
    /// Where our pointer was when we started driving.
    exit: (i32, i32),
    /// Keys and buttons we pressed on the peer, and the peer pressed here, still down.
    /// Released on every switch and disconnect, so nothing stays stuck.
    held_there: Vec<Msg>,
    held_here: Vec<Msg>,
    clip: Option<clip::Clip>,
}

impl Core {
    fn new(os: os::Os, cfg: config::Config) -> Core {
        let now = Instant::now();
        Core {
            displays: os.displays(),
            os,
            link: None,
            state: State::Local,
            displays_at: now,
            last_send: now,
            start: now,
            out: Crossing::new(cfg.edge, cfg.resistance),
            back: Crossing::new(cfg.edge.opposite(), cfg.resistance),
            tap: DoubleTap::new(cfg.hotkey, 300),
            exit: (0, 0),
            held_there: vec![],
            held_here: vec![],
            clip: clip::Clip::new(),
            cfg,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Input>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(input) => self.handle(input),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if self.last_send.elapsed() >= PING_EVERY {
                self.send(Msg::Ping);
            }
            if self.displays_at.elapsed() >= DISPLAYS_EVERY {
                self.displays = self.os.displays();
                self.displays_at = Instant::now();
            }
        }
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Up(link) => {
                eprintln!("linked");
                self.link = Some(link);
            }
            Input::Down => {
                self.link = None;
                self.fall_back();
            }
            Input::Local(event) => self.local(event),
            Input::Peer(msg) => self.peer(msg),
        }
    }

    fn local(&mut self, event: Event) {
        match event {
            Event::Motion { x, y, dx, dy, dragging } => match self.state {
                State::Driving => self.send(Msg::Move { dx: clamp16(dx), dy: clamp16(dy) }),
                State::Local if self.link.is_some() => {
                    if let Push::Cross(pos) = self.out.motion(&self.displays, x, y, dx, dy, dragging) {
                        self.drive(pos);
                    }
                }
                _ => {}
            },
            // While driven, what arrives here includes the keys we inject for the peer.
            Event::Key { hid, down } if self.state != State::Driven => {
                if self.state == State::Driving {
                    self.forward(Msg::Key { hid, down });
                }
                if self.tap.key(hid, down, self.start.elapsed().as_millis() as u64) {
                    match self.state {
                        State::Driving => {
                            self.send(Msg::Leave { pos: 0 });
                            self.come_home(None);
                        }
                        State::Local if self.link.is_some() => {
                            let (x, y) = self.os.cursor();
                            self.drive(crossing::pos_along(&self.displays, self.cfg.edge, x, y));
                        }
                        _ => {}
                    }
                }
            }
            Event::Button { button, down } if self.state == State::Driving => self.forward(Msg::Button { button, down }),
            Event::Scroll { dx, dy } if self.state == State::Driving => self.send(Msg::Scroll { dx, dy }),
            _ => {}
        }
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
                self.back = Crossing::new(edge, self.cfg.resistance);
                if let Some((x, y)) = crossing::entry_point(&self.displays, edge, pos) {
                    self.os.move_to(x, y);
                }
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
                let (dx, dy) = (dx as i32, dy as i32);
                let (x, y) = self.os.cursor();
                let (x, y) = crossing::step_within(&self.displays, x, y, x + dx, y + dy);
                self.os.move_to(x, y);
                let dragging = self.held_here.iter().any(|m| matches!(m, Msg::Button { .. }));
                if let Push::Cross(pos) = self.back.motion(&self.displays, x, y, dx, dy, dragging) {
                    self.let_go();
                    self.send_clipboard();
                    self.send(Msg::Leave { pos });
                }
            }
            Msg::Key { .. } | Msg::Button { .. } | Msg::Scroll { .. } if self.state == State::Driven => {
                track(&mut self.held_here, &msg);
                self.os.inject(&msg);
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
        self.send(Msg::Enter { edge: self.cfg.edge.opposite(), pos });
    }

    /// Stops driving. With `pos` the pointer comes back through our edge there, otherwise to
    /// where it left.
    fn come_home(&mut self, pos: Option<u16>) {
        for up in release(&mut self.held_there) {
            self.send(up);
        }
        self.os.grab(false);
        let (x, y) = pos.and_then(|p| crossing::entry_point(&self.displays, self.cfg.edge, p)).unwrap_or(self.exit);
        self.os.move_to(x, y);
        self.state = State::Local;
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

fn clamp16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

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
