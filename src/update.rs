//! Updates from the project's GitHub releases. A release carries SHA256SUMS, signed with the
//! project's Ed25519 key; nothing is installed unless that signature checks out against the key
//! built in here and the download matches its checksum.
//!
//! The downloads go through the system's curl (in Windows since 10 1803), so the app carries no
//! TLS code of its own.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub const REPO: &str = "PedroElizalde01/yunta";
/// The public half of the release signing key. The private half never leaves the release machine.
const PUBLIC_KEY: [u8; 32] = [
    0x7f, 0x1f, 0x1c, 0x8c, 0x7c, 0x37, 0xe7, 0x6b, 0xc6, 0xd0, 0xb6, 0xa8, 0x04, 0xf2, 0x2b, 0x91, 0xba, 0xce, 0x60, 0x84, 0x91, 0xd1,
    0xb4, 0x44, 0x03, 0xe8, 0x7f, 0xcf, 0xb6, 0x90, 0x02, 0x52,
];

pub struct Release {
    pub version: String,
    /// Each file's name and download address.
    assets: Vec<(String, String)>,
}

impl Release {
    fn asset(&self, name: &str) -> io::Result<&str> {
        self.assets.iter().find(|a| a.0 == name).map(|a| a.1.as_str()).ok_or_else(|| io::Error::other(format!("the release has no {name}")))
    }

    /// The file this computer installs.
    fn package(&self) -> String {
        if cfg!(windows) { "yunta.exe".into() } else { format!("yunta_{}_amd64.deb", self.version) }
    }
}

/// The newest release, if it is newer than this build.
pub fn check() -> io::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let json = curl(&[&url, "-H", "Accept: application/vnd.github+json"], JSON_MAX)?;
    let json = String::from_utf8_lossy(&json);
    let version = strings(&json, "tag_name").into_iter().next().ok_or_else(|| io::Error::other("no release found"))?;
    let version = version.trim_start_matches('v').to_string();
    let urls = strings(&json, "browser_download_url");
    let assets = urls.into_iter().map(|u| (u.rsplit('/').next().unwrap_or_default().to_string(), u)).collect();
    Ok(newer(&version, env!("CARGO_PKG_VERSION")).then_some(Release { version, assets }))
}

/// Downloads, checks and installs `release`. On Linux that asks for an administrator's password;
/// on Windows the running copy is moved aside and the new one put in its place. Either way the
/// app must be started again afterwards.
pub fn install(release: &Release) -> io::Result<()> {
    let sums = curl(&[release.asset("SHA256SUMS")?], SUMS_MAX)?;
    let signature = curl(&[release.asset("SHA256SUMS.sig")?], SIG_MAX)?;
    verify(&sums, &signature)?;
    let name = release.package();
    let package = curl(&[release.asset(&name)?], PACKAGE_MAX)?;
    let want = String::from_utf8_lossy(&sums)
        .lines()
        .find_map(|l| l.split_once("  ").filter(|(_, file)| file.trim() == name).map(|(sum, _)| sum.to_string()))
        .ok_or_else(|| io::Error::other(format!("SHA256SUMS does not list {name}")))?;
    if crate::config::hex(&Sha256::digest(&package)) != want {
        return Err(io::Error::other(format!("{name} does not match its checksum")));
    }
    // A folder of our own that nobody else can enter, under a name nobody can guess, made
    // fresh: on Linux the package is opened again by apt-get as root, and anyone able to swap it
    // in between could install what they like.
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    let dir = private_dir(&base.join(format!("yunta-update-{}", crate::config::hex(&random_bytes()))))?;
    let file = dir.join(&name);
    let result = write_private(&file, &package).and_then(|()| apply(&file));
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Most a download may be: past this it is not what it claims to be, and stops.
const JSON_MAX: usize = 1024 * 1024;
const SUMS_MAX: usize = 64 * 1024;
const SIG_MAX: usize = 1024;
const PACKAGE_MAX: usize = 64 * 1024 * 1024;

fn random_bytes() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system RNG");
    bytes
}

/// Makes `dir`, which must not exist yet, readable by this user alone.
fn private_dir(dir: &Path) -> io::Result<PathBuf> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    Ok(dir.to_path_buf())
}

/// Writes a new file, never an existing one, readable by this user alone.
fn write_private(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut f = options.open(file)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()
}

#[cfg(target_os = "linux")]
fn apply(deb: &Path) -> io::Result<()> {
    // pkexec asks for the password in a dialog of the desktop's own.
    let status = Command::new("pkexec").args(["apt-get", "install", "-y", "--allow-downgrades"]).arg(deb).status()?;
    if !status.success() {
        return Err(io::Error::other("the package was not installed (cancelled, or apt failed)"));
    }
    Ok(())
}

#[cfg(windows)]
fn apply(exe: &Path) -> io::Result<()> {
    // A running program cannot be overwritten, but it can be renamed.
    let current = std::env::current_exe()?;
    // A name of its own each time: an earlier one may still be running and cannot go yet.
    let old = current.with_extension(format!("old-{}.exe", crate::now_ms()));
    std::fs::rename(&current, &old)?;
    if let Err(e) = std::fs::copy(exe, &current) {
        let _ = std::fs::rename(&old, &current);
        return Err(e);
    }
    Ok(())
}

/// Removes the copies Windows updates moved aside (yunta.old-*.exe), once nothing runs from
/// them; one still running stays until next time.
pub fn tidy() {
    if !cfg!(windows) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let (Some(dir), Some(stem)) = (exe.parent(), exe.file_stem().and_then(|s| s.to_str())) else { return };
    let prefix = format!("{stem}.old");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn verify(sums: &[u8], signature: &[u8]) -> io::Result<()> {
    let bad = || io::Error::other("the release's signature is not the project's: not installing it");
    let key = VerifyingKey::from_bytes(&PUBLIC_KEY).map_err(|_| bad())?;
    let signature = Signature::from_slice(signature).map_err(|_| bad())?;
    key.verify_strict(sums, &signature).map_err(|_| bad())
}

/// Downloads through curl, keeping at most `max` bytes: more than that and it stops.
fn curl(args: &[&str], max: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut cmd = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args(["-fsSL", "--max-time", "60", "--proto", "=https", "--max-filesize", &max.to_string()]).args(args);
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let mut child = cmd.spawn().map_err(|e| io::Error::new(e.kind(), format!("curl: {e}")))?;
    let mut body = Vec::new();
    child.stdout.take().expect("piped").take(max as u64 + 1).read_to_end(&mut body)?;
    if body.len() > max {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("the download is larger than it should be"));
    }
    let mut why = String::new();
    child.stderr.take().expect("piped").read_to_string(&mut why)?;
    if !child.wait()?.success() {
        let why = why.trim().to_string();
        return Err(io::Error::other(if why.contains("404") { "no release published yet".to_string() } else { why }));
    }
    Ok(body)
}

/// Every string value of `key` in `json`. Enough for GitHub's release JSON, whose values here
/// are plain names and addresses.
fn strings(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    json.match_indices(&needle)
        .filter_map(|(i, _)| {
            let rest = json[i + needle.len()..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_string())
        })
        .collect()
}

/// True when dotted version `a` is later than `b`.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) > parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_releases_and_versions() {
        let json = r#"{"tag_name": "v0.2.0", "assets": [{"name":"x","browser_download_url":"https://github.com/a/b/releases/download/v0.2.0/yunta.exe"},
            {"browser_download_url" : "https://github.com/a/b/releases/download/v0.2.0/SHA256SUMS"}]}"#;
        assert_eq!(strings(json, "tag_name"), vec!["v0.2.0"]);
        assert_eq!(strings(json, "browser_download_url").len(), 2);
        assert!(newer("0.2.0", "0.1.9") && newer("0.10.0", "0.9.0") && !newer("0.1.0", "0.1.0") && !newer("0.1.0", "0.2.0"));
    }

    /// Asks GitHub, for real: run by hand with `cargo test --release -- --ignored asks_github`.
    #[test]
    #[ignore]
    fn asks_github() {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let json = curl(&[&url], JSON_MAX).unwrap();
        assert!(!strings(&String::from_utf8_lossy(&json), "tag_name").is_empty());
        assert!(curl(&[&url], 10).is_err()); // over the limit
    }

    #[test]
    fn update_files_are_never_reused() {
        let base = std::env::temp_dir().join(format!("yunta-private-{}", std::process::id()));
        let dir = private_dir(&base).unwrap();
        // A folder someone made first is refused, never written into.
        assert!(private_dir(&base).is_err());
        let file = dir.join("yunta.deb");
        write_private(&file, b"one").unwrap();
        assert!(write_private(&file, b"two").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_the_project_key_signs() {
        // Made with `openssl pkeyutl -sign -rawin`, as package.sh signs releases.
        let signed = "5ff34cddf67185b8a9804075eb8feb6d5b82636a4ff81a5c7badffd87a6351f427a9be71ea139a33bb02059116142233a117afa93fa4e17cdec648c0c5543708";
        let signature: Vec<u8> = (0..128).step_by(2).map(|i| u8::from_str_radix(&signed[i..i + 2], 16).unwrap()).collect();
        assert!(verify(b"yunta test vector", &signature).is_ok());
        assert!(verify(b"yunta test vectoR", &signature).is_err());
        assert!(verify(b"sums", &[0; 64]).is_err());
        assert!(verify(b"sums", b"short").is_err());
    }
}
