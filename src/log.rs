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
    // Only the app starts the log over; the window and the app rotating it at once could split it.
    if who == "app" && fs::metadata(&path).is_ok_and(|m| m.len() > MAX) {
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
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let line = format!("{} {args}", stamp(ms));
    eprintln!("{line}");
    if let Ok(mut file) = FILE.lock()
        && let Some((file, who)) = file.as_mut()
    {
        let _ = writeln!(file, "{line} [{who} {}]", std::process::id());
    }
}

/// `ms` since 1970 as an ISO 8601 date and time in UTC, to the millisecond: the standard library
/// knows no time zones. The date is the civil calendar's, by Howard Hinnant's days_from_civil.
fn stamp(ms: u64) -> String {
    let (days, day) = ((ms / 86_400_000) as i64, ms % 86_400_000);
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", day / 3_600_000, day / 60_000 % 60, day / 1000 % 60, day % 1000)
}

/// The last `n` lines of the log, for copying out of the settings window.
pub fn tail(config: &Path, n: usize) -> String {
    let text = fs::read_to_string(path(config)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamps_dates() {
        assert_eq!(super::stamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::stamp(1_790_953_553_980), "2026-10-02T15:05:53.980Z");
        assert_eq!(super::stamp(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
