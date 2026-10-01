//! Settings and keys: `key = value` lines in yunta.conf, `#` starts a comment.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::crossing::{Edge, Layout};
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
    /// What the other machine calls itself, from pairing.
    pub peer_name: Option<String>,
    pub port: u16,
    /// Which edge of this machine's screens leads to the other machine, and which part of it.
    pub layout: Layout,
    pub resistance: i32,
    /// HID usage of the key that switches on a double-tap. Right Ctrl to start with.
    pub hotkey: u16,
    /// Crossing at the edge is off; the shortcut still switches.
    pub paused: bool,
    /// The crossing animations.
    pub effects: bool,
    /// The settings window's look: system, light or dark.
    pub theme: String,
    /// What this computer calls itself, when set; the system's name otherwise.
    pub name: Option<String>,
    /// This keyboard and mouse may go to the other computer.
    pub send: bool,
    /// The other computer's keyboard and mouse may come here.
    pub receive: bool,
    /// No crossing at the edge while an app fills the screen.
    pub fullscreen: bool,
    /// The trigger key switches when held, rather than double-tapped.
    pub hold: bool,
    /// How fast the other computer's pointer and scrolling go here.
    pub pointer_speed: f32,
    pub scroll_speed: f32,
    /// Ctrl and Super trade places on the way out, so a Mac keyboard's Cmd works as Ctrl.
    pub swap_modifiers: bool,
    /// Groups of keys and buttons that stay on this computer while it drives the other.
    pub keep: Vec<String>,
    /// Touchpad swipes go across as the other computer's gestures.
    pub gestures: bool,
    /// Switching to the other computer while it is asleep wakes it.
    pub wake: bool,
    /// The other computer's network card, for waking it. Learnt when they connect.
    pub peer_mac: Option<[u8; 6]>,
    /// Look for a new version once a day.
    pub updates: bool,
    /// A new value asks the running app to start again, as after an update.
    pub restart: u64,
}

/// The groups `keep` may name.
pub const KEEP: [&str; 4] = ["volume", "media", "side_buttons", "print_screen"];

impl Config {
    /// This computer's name: the one given in settings, or the system's.
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(crate::pair::machine_name)
    }
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
    load_from(&path())
}

pub fn load_from(path: &Path) -> io::Result<Config> {
    let path = path.to_path_buf();
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
        peer_name: None,
        port: DEFAULT_PORT,
        layout: Layout::default(),
        resistance: 120,
        hotkey: 0xE4,
        paused: false,
        effects: true,
        theme: "system".into(),
        name: None,
        send: true,
        receive: true,
        fullscreen: true,
        hold: false,
        pointer_speed: 1.0,
        scroll_speed: 1.0,
        swap_modifiers: false,
        keep: vec![],
        gestures: true,
        wake: true,
        peer_mac: None,
        updates: true,
        restart: 0,
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
            "peer_name" => cfg.peer_name = Some(v.to_string()),
            "paused" | "effects" | "send" | "receive" | "fullscreen" | "swap_modifiers" | "gestures" | "wake" | "updates" | "corner" => {
                let on = match v {
                    "yes" => true,
                    "no" => false,
                    _ => return Err(err("expected yes or no")),
                };
                match k {
                    "paused" => cfg.paused = on,
                    "effects" => cfg.effects = on,
                    "send" => cfg.send = on,
                    "receive" => cfg.receive = on,
                    "fullscreen" => cfg.fullscreen = on,
                    "swap_modifiers" => cfg.swap_modifiers = on,
                    "gestures" => cfg.gestures = on,
                    "wake" => cfg.wake = on,
                    "updates" => cfg.updates = on,
                    _ => cfg.layout.corner = on,
                }
            }
            "trigger" => {
                cfg.hold = match v {
                    "double" => false,
                    "hold" => true,
                    _ => return Err(err("expected double or hold")),
                }
            }
            "pointer_speed" | "scroll_speed" => {
                let speed = v.parse().ok().filter(|s| (0.1..=5.0).contains(s)).ok_or_else(|| err("expected a number from 0.1 to 5"))?;
                *(if k == "pointer_speed" { &mut cfg.pointer_speed } else { &mut cfg.scroll_speed }) = speed;
            }
            "keep" => {
                cfg.keep = v.split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
                if let Some(bad) = cfg.keep.iter().find(|g| !KEEP.contains(&g.as_str())) {
                    return Err(err(&format!("no group of keys called {bad}; there are {}", KEEP.join(", "))));
                }
            }
            "name" => cfg.name = Some(v.to_string()),
            "peer_mac" => cfg.peer_mac = Some(parse_mac(v).ok_or_else(|| err("expected a hardware address like aa:bb:cc:dd:ee:ff"))?),
            "restart" => cfg.restart = v.parse().map_err(|_| err("expected a number"))?,
            "theme" if ["system", "light", "dark"].contains(&v) => cfg.theme = v.to_string(),
            "theme" => return Err(err("expected system, light or dark")),
            "port" => cfg.port = v.parse().map_err(|_| err("expected a port number"))?,
            "edge" => {
                cfg.layout.edge = match v {
                    "left" => Edge::Left,
                    "right" => Edge::Right,
                    "top" => Edge::Top,
                    "bottom" => Edge::Bottom,
                    _ => return Err(err("expected left, right, top or bottom")),
                }
            }
            "along" => {
                let f: Vec<u16> = v
                    .split(',')
                    .map(|f| f.trim().parse::<f64>().ok().filter(|f| (0.0..=1.0).contains(f)))
                    .map(|f| f.map(|f| (f * 65535.0).round() as u16))
                    .collect::<Option<_>>()
                    .ok_or_else(|| err("expected four fractions from 0 to 1"))?;
                match f[..] {
                    [a0, a1, b0, b1] if a0 <= a1 && b0 <= b1 => (cfg.layout.ours, cfg.layout.theirs) = ((a0, a1), (b0, b1)),
                    _ => return Err(err("expected `along = from, to, peer from, peer to`, each from no bigger than its to")),
                }
            }
            "layout_at" => cfg.layout.stamp = v.parse().map_err(|_| err("expected milliseconds"))?,
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

/// The status file the running app keeps for the settings window, next to yunta.conf.
pub fn status_path(config: &Path) -> PathBuf {
    config.with_file_name("status")
}

/// Left by Quit in the tray, so the settings window closes with the app.
pub fn quit_path(config: &Path) -> PathBuf {
    config.with_file_name("quit")
}

/// Takes the lock file `name` next to yunta.conf. `None` when another process holds it. The lock
/// lasts as long as the file stays open.
pub fn lock(config: &Path, name: &str) -> io::Result<Option<File>> {
    // Not truncated: on Windows, cutting a file another process has locked fails.
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(config.with_file_name(name))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Writes the arrangement into the file.
pub fn save_layout(path: &Path, l: &Layout) -> io::Result<()> {
    let f = |v: u16| format!("{:.6}", f64::from(v) / 65535.0);
    set(path, "edge", Some(l.edge.name()))?;
    set(path, "along", Some(&format!("{}, {}, {}, {}", f(l.ours.0), f(l.ours.1), f(l.theirs.0), f(l.theirs.1))))?;
    set(path, "corner", Some(if l.corner { "yes" } else { "no" }))?;
    set(path, "layout_at", Some(&l.stamp.to_string()))
}

/// A name fit to save: no `#`, which starts a comment, no control characters, at most 48 long.
pub fn clean_name(name: &str) -> String {
    name.chars().filter(|c| *c != '#' && !c.is_control()).take(48).collect::<String>().trim().to_string()
}

pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = text.split([':', '-']).map(|b| u8::from_str_radix(b, 16).ok().filter(|_| b.len() == 2)).collect::<Option<_>>()?;
    bytes.try_into().ok()
}

pub fn mac_text(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
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
        let text = format!(
            "# hi\nkey = {k}\npublic = {k}\npeer_key =\npeer = 10.0.0.2 # the PC\nedge = left\nresistance = 0\nhotkey = 0xe6\nalong = 0, 0.5, 0, 1\nlayout_at = 42\npaused = yes\npeer_name = the PC\n"
        );
        let cfg = parse("t".into(), &text).unwrap();
        assert_eq!(cfg.key, vec![0xAB; 32]);
        assert_eq!(cfg.peer_key, None);
        assert_eq!(cfg.peer.as_deref(), Some("10.0.0.2"));
        assert_eq!((cfg.layout.edge, cfg.resistance, cfg.hotkey, cfg.port), (Edge::Left, 0, 0xE6, DEFAULT_PORT));
        assert_eq!((cfg.layout.ours, cfg.layout.theirs, cfg.layout.stamp), ((0, 32768), (0, 65535), 42));
        assert_eq!((cfg.paused, cfg.peer_name.as_deref()), (true, Some("the PC")));
        let more =
            format!("{text}trigger = hold\npointer_speed = 1.5\nkeep = volume, side_buttons\npeer_mac = AA-bb-cc-00-11-22\nsend = no\n");
        let cfg = parse("t".into(), &more).unwrap();
        assert!(cfg.hold && !cfg.send && cfg.receive);
        assert_eq!((cfg.pointer_speed, cfg.keep.len()), (1.5, 2));
        assert_eq!(cfg.peer_mac.map(|m| mac_text(&m)).as_deref(), Some("aa:bb:cc:00:11:22"));
        assert_eq!(clean_name("  My #PC\n "), "My PC");
        for bad in [
            "key = abc",
            "edge = up",
            "nonsense = 1",
            "resistance = -1",
            "just words",
            "along = 0, 1, 0",
            "along = 1, 0, 0, 1",
            "along = 0, 2, 0, 1",
            "paused = maybe",
            "trigger = triple",
            "pointer_speed = 9",
            "keep = volume, everything",
            "peer_mac = aa:bb:cc",
        ] {
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
        let l = Layout { edge: Edge::Top, ours: (0, 32768), theirs: (12345, 65535), stamp: 7, corner: false };
        fs::write(&path, format!("key = {k}\npublic = {k}\n", k = "ab".repeat(32))).unwrap();
        save_layout(&path, &l).unwrap();
        let back = parse(path.clone(), &fs::read_to_string(&path).unwrap()).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(back.layout, l);
    }
}
