//! Encrypted link: Noise_KK over TCP.
//!
//! Each machine holds its own static key and pinned the peer's public key at pairing, so a
//! handshake only completes between the two paired machines. Every connection mixes in fresh
//! ephemeral keys, so a recording stays unreadable even if a static key leaks later. Noise's
//! per-direction keys and counting nonces reject replayed, reordered, dropped or reflected frames.
//!
//! Wire: frames of u16 BE length + Noise message. A message is u32 BE length + bytes, sealed
//! across as many frames as it needs (clipboard images run to megabytes, a frame to 64KB).

use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use snow::{Builder, HandshakeState, StatelessTransportState};

pub use snow::Keypair;

const PARAMS: &str = "Noise_KK_25519_ChaChaPoly_BLAKE2s";
// Bumping this makes old and new builds fail the handshake instead of misreading each other.
const PROLOGUE: &[u8] = b"yunta/1";
const FRAME_MAX: usize = 65535;
const CHUNK: usize = FRAME_MAX - 16;
// Both KK handshake messages are 48 bytes. Anything bigger before authentication is not us.
const HANDSHAKE_FRAME_MAX: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
pub const MAX_MSG: usize = 16 * 1024 * 1024;
/// The peer sends at least one message a second, so this much silence means the link is dead.
pub const READ_TIMEOUT: Duration = Duration::from_millis(2500);

pub fn keypair() -> Keypair {
    Builder::new(PARAMS.parse().unwrap()).generate_keypair().expect("system RNG")
}

pub struct Sender {
    stream: TcpStream,
    noise: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
}

pub struct Receiver {
    stream: TcpStream,
    noise: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
}

pub fn connect(addr: impl ToSocketAddrs, key: &[u8], peer: &[u8]) -> io::Result<(Sender, Receiver)> {
    let addr = addr.to_socket_addrs()?.next().ok_or(io::ErrorKind::NotFound)?;
    let stream = TcpStream::connect_timeout(&addr, HANDSHAKE_TIMEOUT)?;
    handshake(stream, builder(key, peer)?.build_initiator().map_err(bad)?)
}

/// Runs the responder side on a stream from `TcpListener::accept`. Refuses anything off the LAN.
pub fn accept(stream: TcpStream, key: &[u8], peer: &[u8]) -> io::Result<(Sender, Receiver)> {
    if !is_lan(stream.peer_addr()?.ip()) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "peer is not on the local network"));
    }
    handshake(stream, builder(key, peer)?.build_responder().map_err(bad)?)
}

pub fn is_lan(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local() || ip.is_loopback(),
    }
}

fn builder<'a>(key: &'a [u8], peer: &'a [u8]) -> io::Result<Builder<'a>> {
    Builder::new(PARAMS.parse().unwrap())
        .local_private_key(key)
        .and_then(|b| b.remote_public_key(peer))
        .and_then(|b| b.prologue(PROLOGUE))
        .map_err(bad)
}

fn handshake(mut stream: TcpStream, mut hs: HandshakeState) -> io::Result<(Sender, Receiver)> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    // A hard deadline, not a per-read timeout, so a peer trickling bytes cannot hold us here.
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut buf = vec![0u8; FRAME_MAX];
    while !hs.is_handshake_finished() {
        if hs.is_my_turn() {
            let n = hs.write_message(&[], &mut buf).map_err(bad)?;
            write_frame(&mut stream, &buf[..n])?;
        } else {
            let frame = read_frame(&mut stream, HANDSHAKE_FRAME_MAX, Some(deadline))?;
            hs.read_message(&frame, &mut buf).map_err(bad)?;
        }
    }
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let noise = Arc::new(hs.into_stateless_transport_mode().map_err(bad)?);
    let sender = Sender { stream: stream.try_clone()?, noise: noise.clone(), nonce: 0, buf: vec![0; FRAME_MAX] };
    Ok((sender, Receiver { stream, noise, nonce: 0, buf }))
}

impl Sender {
    pub fn send(&mut self, msg: &[u8]) -> io::Result<()> {
        if msg.len() > MAX_MSG {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too large"));
        }
        let mut plain = Vec::with_capacity(4 + msg.len());
        plain.extend_from_slice(&(msg.len() as u32).to_be_bytes());
        plain.extend_from_slice(msg);
        let mut out = Vec::with_capacity(plain.len() + plain.len() / CHUNK * 18 + 18);
        for chunk in plain.chunks(CHUNK) {
            let n = self.noise.write_message(self.nonce, chunk, &mut self.buf).map_err(bad)?;
            self.nonce += 1;
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out.extend_from_slice(&self.buf[..n]);
        }
        self.stream.write_all(&out)
    }
}

impl Receiver {
    pub fn recv(&mut self) -> io::Result<Vec<u8>> {
        let n = self.open()?;
        let head: [u8; 4] = self.buf.get(..4).and_then(|h| h.try_into().ok()).filter(|_| n >= 4).ok_or_else(|| bad("short message"))?;
        let len = u32::from_be_bytes(head) as usize;
        if len > MAX_MSG {
            return Err(bad("message too large"));
        }
        let mut plain = Vec::with_capacity(len);
        plain.extend_from_slice(&self.buf[4..n]);
        while plain.len() < len {
            let n = self.open()?;
            plain.extend_from_slice(&self.buf[..n]);
        }
        if plain.len() != len {
            return Err(bad("message overran its length"));
        }
        Ok(plain)
    }

    fn open(&mut self) -> io::Result<usize> {
        let frame = read_frame(&mut self.stream, FRAME_MAX, None)?;
        let n = self.noise.read_message(self.nonce, &frame, &mut self.buf).map_err(bad)?;
        self.nonce += 1;
        Ok(n)
    }
}

fn write_frame(stream: &mut TcpStream, frame: &[u8]) -> io::Result<()> {
    let mut out = Vec::with_capacity(2 + frame.len());
    out.extend_from_slice(&(frame.len() as u16).to_be_bytes());
    out.extend_from_slice(frame);
    stream.write_all(&out)
}

fn read_frame(stream: &mut TcpStream, max: usize, deadline: Option<Instant>) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    read_full(stream, &mut len, deadline)?;
    let len = u16::from_be_bytes(len) as usize;
    if len > max {
        return Err(bad("frame too large"));
    }
    let mut frame = vec![0u8; len];
    read_full(stream, &mut frame, deadline)?;
    Ok(frame)
}

/// `read_exact`, but with an optional overall deadline instead of the stream's per-read timeout.
fn read_full(stream: &mut TcpStream, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            stream.set_read_timeout(Some(left))?;
        }
        match stream.read(&mut buf[got..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn bad(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn paired_peers_talk_and_strangers_do_not() {
        let (a, b, stranger) = (keypair(), keypair(), keypair());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (b_key, a_pub) = (b.private.clone(), a.public.clone());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (mut tx, mut rx) = accept(stream, &b_key, &a_pub).unwrap();
            let msg = rx.recv().unwrap();
            tx.send(&msg).unwrap();
            let (stream, _) = listener.accept().unwrap();
            assert!(accept(stream, &b_key, &a_pub).is_err());
        });

        let (mut tx, mut rx) = connect(addr, &a.private, &b.public).unwrap();
        // Spans several frames, so chunking and reassembly are exercised.
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        tx.send(&big).unwrap();
        assert_eq!(rx.recv().unwrap(), big);

        assert!(connect(addr, &stranger.private, &b.public).is_err());
        server.join().unwrap();
    }

    #[test]
    fn lan_only() {
        for ip in ["192.168.1.20", "10.0.0.1", "172.16.5.5", "169.254.1.1", "127.0.0.1", "fd00::1", "fe80::1", "::ffff:10.0.0.1"] {
            assert!(is_lan(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_lan(ip.parse().unwrap()), "{ip}");
        }
    }
}
