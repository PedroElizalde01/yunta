//! Settings and keys: `key = value` lines in yunta.conf, `#` starts a comment.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::crossing::Edge;
use crate::link;

pub const DEFAULT_PORT: u16 = 24830;

pub struct Config {
    pub path: PathBuf,
    pub key: Vec<u8>,
    pub public: Vec<u8>,
    /// The other machine's public key. Pairing fills it in (A5); by hand until then.
    pub peer_key: Option<Vec<u8>>,
    /// The other machine's address. The machine that has it dials; the other one listens.
    pub peer: Option<String>,
    pub port: u16,
    /// The edge of this machine's screens that leads to the other machine.
    pub edge: Edge,
    pub resistance: i32,
    /// HID usage of the key that switches on a double-tap. Right Ctrl to start with.
    pub hotkey: u16,
}

/// `$YUNTA_CONFIG`, else yunta/yunta.conf in the platform's per-user config folder.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("YUNTA_CONFIG") {
        return p.into();
    }
    let var = |k| std::env::var_os(k).map(PathBuf::from);
    let dir = if cfg!(windows) { var("APPDATA") } else { var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config"))) };
    dir.unwrap_or_default().join("yunta").join("yunta.conf")
}

/// Writes a config with a fresh keypair unless one exists, then loads it.
pub fn init() -> io::Result<Config> {
    let path = path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let kp = link::keypair();
        let text = format!(
            "# yunta settings. Keep this file private: it holds this machine's key.\n\
             key = {}\npublic = {}\n\
             # The other machine's public key: what `yunta init` prints there.\npeer_key =\n\
             # On one machine only, the other one's address. The machine without it listens.\n# peer = 192.168.1.20\n\
             # The edge of this machine's screens that leads to the other one: left, right, top or bottom.\nedge = right\n\
             resistance = 120\n",
            hex(&kp.private),
            hex(&kp.public)
        );
        let mut file = OpenOptions::new();
        file.write(true).create_new(true);
        // On Windows, %APPDATA% is already readable by this user alone.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
        file.open(&path)?.write_all(text.as_bytes())?;
    }
    load()
}

pub fn load() -> io::Result<Config> {
    let path = path();
    let text = fs::read_to_string(&path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e} (run `yunta init`)", path.display())))?;
    parse(path, &text)
}

fn parse(path: PathBuf, text: &str) -> io::Result<Config> {
    let mut cfg = Config {
        path,
        key: vec![],
        public: vec![],
        peer_key: None,
        peer: None,
        port: DEFAULT_PORT,
        edge: Edge::Right,
        resistance: 120,
        hotkey: 0xE4,
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let err = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}:{}: {what}", cfg.path.display(), n + 1));
        let (k, v) = line.split_once('=').ok_or_else(|| err("expected `key = value`"))?;
        let (k, v) = (k.trim(), v.trim());
        if v.is_empty() {
            continue;
        }
        let key32 = || unhex(v).filter(|k| k.len() == 32).ok_or_else(|| err("expected 64 hex digits"));
        match k {
            "key" => cfg.key = key32()?,
            "public" => cfg.public = key32()?,
            "peer_key" => cfg.peer_key = Some(key32()?),
            "peer" => cfg.peer = Some(v.to_string()),
            "port" => cfg.port = v.parse().map_err(|_| err("expected a port number"))?,
            "edge" => {
                cfg.edge = match v {
                    "left" => Edge::Left,
                    "right" => Edge::Right,
                    "top" => Edge::Top,
                    "bottom" => Edge::Bottom,
                    _ => return Err(err("expected left, right, top or bottom")),
                }
            }
            "resistance" => cfg.resistance = v.parse().ok().filter(|r| *r >= 0).ok_or_else(|| err("expected a number, 0 or more"))?,
            "hotkey" => cfg.hotkey = u16::from_str_radix(v.trim_start_matches("0x"), 16).map_err(|_| err("expected a HID usage in hex"))?,
            _ => return Err(err("unknown setting")),
        }
    }
    if cfg.key.is_empty() || cfg.public.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: no key (run `yunta init`)", cfg.path.display())));
    }
    Ok(cfg)
}

/// Rewrites one setting in the file, keeping everything else. `Some` replaces the setting, or its
/// commented-out example, or adds it at the end; `None` removes it.
pub fn set(path: &Path, key: &str, value: Option<&str>) -> io::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut out = String::new();
    let mut written = value.is_none();
    for line in text.lines() {
        let live = !line.trim_start().starts_with('#');
        let name = line.trim_start_matches(['#', ' ']).split('=').next().unwrap_or("").trim();
        if name == key && line.contains('=') && (live || !written) {
            if let (Some(v), false) = (value, written) {
                out.push_str(&format!("{key} = {v}\n"));
                written = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !written && let Some(v) = value {
        out.push_str(&format!("{key} = {v}\n"));
    }
    // Written in place, so the file keeps its owner-only permissions.
    fs::write(path, out)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects() {
        let k = "ab".repeat(32);
        let text =
            format!("# hi\nkey = {k}\npublic = {k}\npeer_key =\npeer = 10.0.0.2 # the PC\nedge = left\nresistance = 0\nhotkey = 0xe6\n");
        let cfg = parse("t".into(), &text).unwrap();
        assert_eq!(cfg.key, vec![0xAB; 32]);
        assert_eq!(cfg.peer_key, None);
        assert_eq!(cfg.peer.as_deref(), Some("10.0.0.2"));
        assert_eq!((cfg.edge, cfg.resistance, cfg.hotkey, cfg.port), (Edge::Left, 0, 0xE6, DEFAULT_PORT));
        for bad in ["key = abc", "edge = up", "nonsense = 1", "resistance = -1", "just words"] {
            assert!(parse("t".into(), &format!("{text}{bad}\n")).is_err(), "{bad}");
        }
        assert!(parse("t".into(), "edge = left\n").is_err()); // no key
    }

    #[test]
    fn set_rewrites_in_place() {
        let path = std::env::temp_dir().join(format!("yunta-set-{}.conf", std::process::id()));
        fs::write(&path, "# note = keep me\npeer_key =\n# peer = 1.2.3.4\nedge = left\n").unwrap();
        set(&path, "peer_key", Some("ab")).unwrap();
        set(&path, "peer", Some("10.0.0.9")).unwrap();
        set(&path, "resistance", Some("0")).unwrap();
        set(&path, "edge", None).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(text, "# note = keep me\npeer_key = ab\npeer = 10.0.0.9\nresistance = 0\n");
    }
}
