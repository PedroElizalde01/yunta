//! Pairing mode: two machines learn each other's public keys, vouched for by a six-digit code
//! read off one screen and typed on the other.
//!
//! Every machine in pairing mode broadcasts a UDP beacon each second and lists the others it
//! hears. The machine where the code is typed connects over TCP and the two run SPAKE2 with the
//! code as the password: only a side that knows the code arrives at the same key, and nothing on
//! the wire lets anyone test guesses offline. That key then seals the exchange of public keys,
//! which confirms it in both directions. Each wrong code costs a live attempt; three end pairing
//! mode, and it ends by itself after two minutes.

use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::link;

pub const PORT: u16 = 24831;
const MAGIC: &str = "yunta-pair/1";
const LIFETIME: Duration = Duration::from_secs(120);
const ATTEMPTS: u32 = 3;
const TIMEOUT: Duration = Duration::from_secs(5);
const MSG_MAX: usize = 512;
/// How often the pairing threads look up to see whether pairing mode is over.
const POLL: Duration = Duration::from_millis(200);
const ID_JOIN: &[u8] = b"yunta-join";
const ID_HOST: &[u8] = b"yunta-host";

/// The other machine's public key and name.
type Peer = (Vec<u8>, String);

pub struct Paired {
    pub peer_key: Vec<u8>,
    pub name: String,
    pub addr: IpAddr,
    /// True on the machine where the code was typed. It dials; the other listens.
    pub dialer: bool,
}

/// What happens in pairing mode, as `Mode::next` reports it.
pub enum Event {
    /// Another machine in pairing mode.
    Found(String, IpAddr),
    /// Paired: the other machine's key and name, ready to save.
    Paired(Paired),
    /// Someone typed a wrong code for ours: `n` of the attempts allowed. The host thread sends
    /// 0, and `next` counts.
    WrongCode(IpAddr, u32),
    /// The code we typed is not the one that machine shows.
    Refused(IpAddr),
    /// Trying a code for that machine failed for another reason.
    Failed(IpAddr, String),
}

/// Pairing mode while it lasts: the beacon, the list it builds and the port where a code can
/// be tried. Dropping it ends pairing mode and closes both ports.
pub struct Mode {
    pub code: String,
    pub name: String,
    public: Vec<u8>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    deadline: Instant,
    wrong: u32,
    stop: Arc<AtomicBool>,
}

impl Mode {
    pub fn start(public: &[u8], name: &str) -> io::Result<Mode> {
        let host = TcpListener::bind(("0.0.0.0", PORT)).map_err(|e| io::Error::new(e.kind(), format!("pairing port {PORT}: {e}")))?;
        let beacons = UdpSocket::bind(("0.0.0.0", PORT)).map_err(|e| io::Error::new(e.kind(), format!("pairing port {PORT}/udp: {e}")))?;
        // Both wake up now and then to see whether pairing mode is over.
        host.set_nonblocking(true)?;
        beacons.set_read_timeout(Some(POLL))?;
        let (tx, rx) = mpsc::channel();
        let mode = Mode {
            code: new_code(),
            name: name.to_string(),
            public: public.to_vec(),
            tx,
            rx,
            deadline: Instant::now() + LIFETIME,
            wrong: 0,
            stop: Arc::default(),
        };
        let id = format!("{:016x}", random_u64());
        let beacon = format!("{MAGIC}\n{id}\n{}", mode.name);
        let stop = mode.stop.clone();
        thread::spawn(move || announce(&beacon, &stop));
        let (found, stop) = (mode.tx.clone(), mode.stop.clone());
        thread::spawn(move || listen(&beacons, &id, &found, &stop));
        let (hosted, key, name, code, stop) =
            (mode.tx.clone(), mode.public.clone(), mode.name.clone(), mode.code.clone(), mode.stop.clone());
        thread::spawn(move || host_codes(&host, &code, &key, &name, &hosted, &stop));
        Ok(mode)
    }

    /// How long pairing mode has left.
    pub fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Tries `code` as the one `addr` shows, on a thread of its own. The outcome comes from `next`.
    pub fn join(&self, addr: IpAddr, code: &str) {
        let (tx, public, name, code) = (self.tx.clone(), self.public.clone(), self.name.clone(), code.to_string());
        thread::spawn(move || {
            let event = match join(SocketAddr::new(addr, PORT), &code, &public, &name) {
                Ok((peer_key, name)) => Event::Paired(Paired { peer_key, name, addr, dialer: true }),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Event::Refused(addr),
                Err(e) => Event::Failed(addr, e.to_string()),
            };
            let _ = tx.send(event);
        });
    }

    /// The next thing that happened, waiting up to `wait` for it. An error once pairing mode
    /// is over: timed out, or too many wrong codes.
    pub fn next(&mut self, wait: Duration) -> io::Result<Option<Event>> {
        if self.left().is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "pairing mode timed out"));
        }
        let Ok(event) = self.rx.recv_timeout(wait.min(self.left())) else { return Ok(None) };
        if let Event::WrongCode(addr, _) = event {
            self.wrong += 1;
            if self.wrong >= ATTEMPTS {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "too many wrong codes, pairing mode is off"));
            }
            return Ok(Some(Event::WrongCode(addr, self.wrong)));
        }
        Ok(Some(event))
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Pairing in the terminal: `yunta pair`.
pub fn run(public: &[u8], name: &str) -> io::Result<Paired> {
    let mut mode = Mode::start(public, name)?;
    let code = mode.code.clone();
    println!("Pairing mode is on for two minutes. This machine is {}, and its code is  {code}\n", mode.name);
    println!("On the other machine, run `yunta pair` too. Then, on one of the two, type the number");
    println!("of the other machine and the code it shows, like `1 {code}`. An address works as well.\n");
    let (typed_tx, typed) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if typed_tx.send(line).is_err() {
                return;
            }
        }
    });
    let mut machines: Vec<(String, IpAddr)> = vec![];
    loop {
        for line in typed.try_iter() {
            let mut words = line.split_whitespace();
            let (Some(target), Some(typed), None) = (words.next(), words.next(), words.next()) else {
                println!("Type the machine's number (or address) and its code, like `1 {code}`.");
                continue;
            };
            match target.parse::<usize>() {
                Ok(n) if (1..=machines.len()).contains(&n) => mode.join(machines[n - 1].1, typed),
                _ => match target.parse::<IpAddr>() {
                    Ok(addr) => mode.join(addr, typed),
                    Err(_) => println!("No machine {target} in the list."),
                },
            }
        }
        match mode.next(POLL)? {
            Some(Event::Found(name, addr)) if !machines.iter().any(|m| m.1 == addr) => {
                println!("  [{}] {name} ({addr})", machines.len() + 1);
                machines.push((name, addr));
            }
            Some(Event::Paired(paired)) => return Ok(paired),
            Some(Event::WrongCode(addr, n)) => println!("{addr} tried a wrong code ({n} of {ATTEMPTS})."),
            Some(Event::Refused(addr)) => println!("That code is not the one {addr} shows."),
            Some(Event::Failed(addr, e)) => println!("Could not pair with {addr}: {e}"),
            Some(Event::Found(..)) | None => {}
        }
    }
}

/// Takes codes typed on other machines for ours, until pairing mode is over.
fn host_codes(host: &TcpListener, code: &str, public: &[u8], name: &str, tx: &mpsc::Sender<Event>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let stream = match host.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL);
                continue;
            }
            Err(_) => continue,
        };
        let Ok(addr) = stream.peer_addr().map(|a| a.ip()) else { continue };
        // ponytail: one attempt at a time, so a stranger can hold the port for up to TIMEOUT
        if !link::is_lan(addr) || stream.set_nonblocking(false).is_err() {
            continue;
        }
        let event = match serve(stream, code, public, name) {
            Ok((peer_key, name)) => Event::Paired(Paired { peer_key, name, addr, dialer: false }),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Event::WrongCode(addr, 0),
            Err(_) => continue,
        };
        if tx.send(event).is_err() {
            return;
        }
    }
}

/// The side showing the code (SPAKE2's B).
fn serve(mut stream: TcpStream, code: &str, public: &[u8], name: &str) -> io::Result<Peer> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(code), &Identity::new(ID_JOIN), &Identity::new(ID_HOST));
    let theirs = read_msg(&mut stream)?;
    write_msg(&mut stream, &ours)?;
    let key = spake.finish(&theirs).map_err(|_| bad("bad SPAKE2 message"))?;
    let peer = open(&key, 1, &read_msg(&mut stream)?)?;
    write_msg(&mut stream, &seal(&key, 2, public, name))?;
    Ok(peer)
}

/// The side where the code is typed (SPAKE2's A).
fn join(addr: SocketAddr, code: &str, public: &[u8], name: &str) -> io::Result<Peer> {
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let (spake, ours) = Spake2::<Ed25519Group>::start_a(&Password::new(code), &Identity::new(ID_JOIN), &Identity::new(ID_HOST));
    write_msg(&mut stream, &ours)?;
    let key = spake.finish(&read_msg(&mut stream)?).map_err(|_| bad("bad SPAKE2 message"))?;
    write_msg(&mut stream, &seal(&key, 1, public, name))?;
    // A host that could not open what we sealed hangs up: the code was wrong.
    let reply = read_msg(&mut stream).map_err(|_| io::Error::from(io::ErrorKind::PermissionDenied))?;
    open(&key, 2, &reply)
}

/// Seals our public key and name. `dir` keeps the two directions' nonces apart.
fn seal(key: &[u8], dir: u8, public: &[u8], name: &str) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let msg = [public, name.as_bytes()].concat();
    cipher.encrypt(&nonce(dir), Payload { msg: &msg, aad: MAGIC.as_bytes() }).expect("sealing cannot fail")
}

fn open(key: &[u8], dir: u8, sealed: &[u8]) -> io::Result<Peer> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plain = cipher
        .decrypt(&nonce(dir), Payload { msg: sealed, aad: MAGIC.as_bytes() })
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "wrong code"))?;
    if plain.len() < 32 {
        return Err(bad("short key"));
    }
    let (peer_key, name) = plain.split_at(32);
    Ok((peer_key.to_vec(), String::from_utf8_lossy(name).chars().take(64).collect()))
}

fn nonce(dir: u8) -> Nonce {
    let mut n = [0u8; 12];
    n[11] = dir;
    Nonce::from(n)
}

fn write_msg(stream: &mut TcpStream, msg: &[u8]) -> io::Result<()> {
    stream.write_all(&[&(msg.len() as u16).to_be_bytes()[..], msg].concat())
}

fn read_msg(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let len = u16::from_be_bytes(len) as usize;
    if len > MSG_MAX {
        return Err(bad("message too large"));
    }
    let mut msg = vec![0u8; len];
    stream.read_exact(&mut msg)?;
    Ok(msg)
}

fn announce(beacon: &str, stop: &AtomicBool) {
    let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else { return };
    let _ = socket.set_broadcast(true);
    // ponytail: the limited broadcast plus a /24 guess; typing an address covers other networks
    let mut targets: Vec<IpAddr> = vec![[255, 255, 255, 255].into()];
    if let Some(IpAddr::V4(ip)) = local_ip() {
        let [a, b, c, _] = ip.octets();
        targets.push([a, b, c, 255].into());
    }
    while !stop.load(Ordering::Relaxed) {
        for target in &targets {
            let _ = socket.send_to(beacon.as_bytes(), (*target, PORT));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn listen(socket: &UdpSocket, own_id: &str, tx: &mpsc::Sender<Event>, stop: &AtomicBool) {
    let mut buf = [0u8; 256];
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
        let text = String::from_utf8_lossy(&buf[..n]);
        let mut lines = text.lines();
        let (Some(MAGIC), Some(id), Some(name)) = (lines.next(), lines.next(), lines.next()) else { continue };
        if id == own_id || !link::is_lan(from.ip()) {
            continue;
        }
        let name = name.chars().filter(|c| !c.is_control()).take(64).collect();
        if tx.send(Event::Found(name, from.ip())).is_err() {
            return;
        }
    }
}

/// The address other machines reach us on. Connecting a UDP socket sends nothing.
fn local_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

pub fn machine_name() -> String {
    let name = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        std::fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|s| s.trim().to_string())
    };
    name.filter(|n| !n.is_empty()).unwrap_or_else(|| "this machine".into())
}

fn new_code() -> String {
    // The modulo bias over a 64-bit draw is below one in 10^13.
    format!("{:06}", random_u64() % 1_000_000)
}

fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("system RNG");
    u64::from_le_bytes(bytes)
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one host on an ephemeral port and one join against it with `typed`.
    fn attempt(typed: &str) -> (io::Result<Peer>, io::Result<Peer>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = thread::spawn(move || serve(listener.accept().unwrap().0, "123456", &[1; 32], "host"));
        let joined = join(SocketAddr::from(([127, 0, 0, 1], port)), typed, &[2; 32], "joiner");
        (host.join().unwrap(), joined)
    }

    #[test]
    fn right_code_swaps_keys() {
        let (host, joined) = attempt("123456");
        assert_eq!(host.unwrap(), (vec![2; 32], "joiner".to_string()));
        assert_eq!(joined.unwrap(), (vec![1; 32], "host".to_string()));
    }

    #[test]
    fn wrong_code_is_refused_on_both_sides() {
        let (host, joined) = attempt("123457");
        assert_eq!(host.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(joined.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn ending_pairing_mode_closes_its_ports() {
        let mode = Mode::start(&[1; 32], "test").unwrap();
        assert!(TcpListener::bind(("0.0.0.0", PORT)).is_err());
        drop(mode);
        thread::sleep(POLL * 3);
        assert!(TcpListener::bind(("0.0.0.0", PORT)).is_ok());
        assert!(UdpSocket::bind(("0.0.0.0", PORT)).is_ok());
    }

    #[test]
    fn codes_are_six_digits() {
        assert!((0..100).map(|_| new_code()).all(|c| c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())));
    }
}
