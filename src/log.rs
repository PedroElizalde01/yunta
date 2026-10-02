//! Yunta's log: every line goes to stderr and to yunta.log next to yunta.conf, so what happened
//! on a machine started from a menu or at login, where stderr goes nowhere, can be read after.
//! The app and the settings window share the file, each line saying which wrote it.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Past this the log starts over, keeping the previous one as yunta.log.1.
const MAX: u64 = 1024 * 1024;

static FILE: Mutex<Option<(File, &'static str)>> = Mutex::new(None);

pub fn path(config: &Path) -> PathBuf {
    config.with_file_name("yunta.log")
}

/// Opens the log for `who` ("app" or "window"), and records crashes in it too: the release build
/// aborts on a panic, and would otherwise vanish without a word.
pub fn start(config: &Path, who: &'static str) {
    let path = path(config);
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX) {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => *FILE.lock().unwrap() = Some((file, who)),
        Err(e) => eprintln!("log: {}: {e}", path.display()),
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write(format_args!("crashed: {info}"));
        default(info);
    }));
    write(format_args!("Yunta {} started on {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS));
}

pub fn write(args: std::fmt::Arguments) {
    let line = format!("{} {args}", clock());
    eprintln!("{line}");
    if let Ok(mut file) = FILE.lock()
        && let Some((file, who)) = file.as_mut()
    {
        let _ = writeln!(file, "{line} [{who}]");
    }
}

/// The time of day in UTC, to the millisecond: the standard library knows no time zones.
fn clock() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let day = ms % 86_400_000;
    format!("{:02}:{:02}:{:02}.{:03}Z", day / 3_600_000, day / 60_000 % 60, day / 1000 % 60, day % 1000)
}

/// The last `n` lines of the log, for copying out of the settings window.
pub fn tail(config: &Path, n: usize) -> String {
    let text = fs::read_to_string(path(config)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
