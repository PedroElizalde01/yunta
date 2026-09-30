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
use std::sync::mpsc;
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

enum Step {
    Found(String, IpAddr),
    Typed(String),
    Paired(Paired),
    WrongCode(IpAddr),
}

pub fn run(public: &[u8]) -> io::Result<Paired> {
    let code = new_code();
    let name = machine_name();
    let id = format!("{:016x}", random_u64());
    let (tx, rx) = mpsc::channel();

    let host = TcpListener::bind(("0.0.0.0", PORT)).map_err(|e| io::Error::new(e.kind(), format!("pairing port {PORT}: {e}")))?;
    let beacons = UdpSocket::bind(("0.0.0.0", PORT)).map_err(|e| io::Error::new(e.kind(), format!("pairing port {PORT}/udp: {e}")))?;
    println!("Pairing mode is on for two minutes. This machine is {name}, and its code is  {code}\n");
    println!("On the other machine, run `yunta pair` too. Then, on one of the two, type the number");
    println!("of the other machine and the code it shows, like `1 {code}`. An address works as well.\n");

    let beacon = format!("{MAGIC}\n{id}\n{name}");
    thread::spawn(move || announce(&beacon));
    let found = tx.clone();
    thread::spawn(move || listen(&beacons, &id, &found));
    let (hosted, key, host_name, host_code) = (tx.clone(), public.to_vec(), name.clone(), code.clone());
    thread::spawn(move || {
        for stream in host.incoming().flatten() {
            let Ok(addr) = stream.peer_addr().map(|a| a.ip()) else { continue };
            if !link::is_lan(addr) {
                continue;
            }
            let step = match serve(stream, &host_code, &key, &host_name) {
                Ok((peer_key, name)) => Step::Paired(Paired { peer_key, name, addr, dialer: false }),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Step::WrongCode(addr),
                Err(_) => continue,
            };
            if hosted.send(step).is_err() {
                return;
            }
        }
    });
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(Step::Typed(line)).is_err() {
                return;
            }
        }
    });

    let deadline = Instant::now() + LIFETIME;
    let mut machines: Vec<(String, IpAddr)> = vec![];
    let mut wrong = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let step = rx.recv_timeout(left).map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "pairing mode timed out"))?;
        match step {
            Step::Found(name, addr) if !machines.iter().any(|m| m.1 == addr) => {
                machines.push((name, addr));
                println!("  [{}] {} ({addr})", machines.len(), machines[machines.len() - 1].0);
            }
            Step::Found(..) => {}
            Step::Typed(line) => {
                let mut words = line.split_whitespace();
                let (Some(target), Some(typed), None) = (words.next(), words.next(), words.next()) else {
                    println!("Type the machine's number (or address) and its code, like `1 {code}`.");
                    continue;
                };
                let addr = match target.parse::<usize>() {
                    Ok(n) if (1..=machines.len()).contains(&n) => machines[n - 1].1,
                    _ => match target.parse::<IpAddr>() {
                        Ok(addr) => addr,
                        Err(_) => {
                            println!("No machine {target} in the list.");
                            continue;
                        }
                    },
                };
                match join(SocketAddr::new(addr, PORT), typed, public, &name) {
                    Ok((peer_key, name)) => return Ok(Paired { peer_key, name, addr, dialer: true }),
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => println!("That code is not the one {addr} shows."),
                    Err(e) => println!("Could not pair with {addr}: {e}"),
                }
            }
            Step::Paired(paired) => return Ok(paired),
            Step::WrongCode(addr) => {
                wrong += 1;
                println!("{addr} tried a wrong code ({wrong} of {ATTEMPTS}).");
                if wrong >= ATTEMPTS {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "too many wrong codes, pairing mode is off"));
                }
            }
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

fn announce(beacon: &str) {
    let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else { return };
    let _ = socket.set_broadcast(true);
    // ponytail: the limited broadcast plus a /24 guess; typing an address covers other networks
    let mut targets: Vec<IpAddr> = vec![[255, 255, 255, 255].into()];
    if let Some(IpAddr::V4(ip)) = local_ip() {
        let [a, b, c, _] = ip.octets();
        targets.push([a, b, c, 255].into());
    }
    loop {
        for target in &targets {
            let _ = socket.send_to(beacon.as_bytes(), (*target, PORT));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn listen(socket: &UdpSocket, own_id: &str, tx: &mpsc::Sender<Step>) {
    let mut buf = [0u8; 256];
    while let Ok((n, from)) = socket.recv_from(&mut buf) {
        let text = String::from_utf8_lossy(&buf[..n]);
        let mut lines = text.lines();
        let (Some(MAGIC), Some(id), Some(name)) = (lines.next(), lines.next(), lines.next()) else { continue };
        if id == own_id || !link::is_lan(from.ip()) {
            continue;
        }
        let name = name.chars().filter(|c| !c.is_control()).take(64).collect();
        if tx.send(Step::Found(name, from.ip())).is_err() {
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
    fn codes_are_six_digits() {
        assert!((0..100).map(|_| new_code()).all(|c| c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())));
    }
}
