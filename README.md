# Yunta

One keyboard and mouse for two computers on the same local network. Push the pointer through the
edge of the screen, or double-tap Right Ctrl, and it carries on onto the other machine.

A glow builds on the edge as you push against it, and a ripple shows where the pointer lands.
Drag the other computer diagonally in Arrangement and it is reached through a corner instead.

- Each direction has its own switch, and a full-screen app keeps the edge to itself.
- Double-tap or hold the shortcut key (Right Ctrl) to switch without moving the pointer.
- The other computer's pointer and scrolling speed are set here, Ctrl and ⌘/Windows can swap,
  and volume keys, media keys, mouse side buttons and Print Screen can stay on this computer.
- Media keys cross. Three and four finger touchpad swipes on Linux play Windows' own gestures.
- Switching to a sleeping computer wakes it (Wake-on-LAN, its address learnt when they connect).
- Name this computer in Connection; the other one shows the new name at once.
- Updates install in one click from About, and only when the release's signature checks out.
Text and images on the clipboard go along at every switch. It runs in the background with a tray
icon. A click on it opens the settings window; its menu pauses crossing, pairs a new computer,
starts at login and quits.

## Setup

1. Install: `sudo apt install ./yunta_<version>_amd64.deb` on Linux; on Windows, `yunta.exe` is
   the whole app, so put it anywhere.
2. Start it on both machines, from the menu or by double-clicking. The first time, it opens the
   settings window on Pairing: start pairing mode on both, and on one of them type the code the
   other shows next to its name. (`yunta` in a terminal pairs in the terminal instead.)
3. In the settings window, under Arrangement, drag the other computer to where it stands. The
   other machine follows, so this is done once, on either one.
4. Under Overview, turn on Start at login.

Both machines must run the same version of Yunta. A machine that hears a message it does not
know drops the link.

`yunta settings` opens the settings window, and `yunta pair` pairs again in the terminal.
Everything the window sets lives in `~/.config/yunta/yunta.conf` on Linux and
`%APPDATA%\yunta\yunta.conf` on Windows; edits there take effect while it runs.

On the PC, allow Yunta through Windows Firewall on private networks when asked. It uses TCP 24830
for input and TCP and UDP 24831 while pairing.

## Build

`./package.sh` builds `dist/yunta_<version>_amd64.deb`, and `dist/yunta.exe` as well once the
Windows toolchain is installed. The `.exe` is built with Microsoft's compiler through cargo-xwin,
which downloads the Windows SDK and C runtime and accepts Microsoft's license for them:

```
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-msvc
cargo install --locked cargo-xwin
```

`./release.sh` publishes a release: it tags the version in Cargo.toml, pushes, runs
`package.sh`, and uploads the signed files to GitHub, where the app's update check finds them.
It refuses a dirty tree, a tag whose code has since changed, and an unsigned build.

## Security

Pairing runs SPAKE2 with the code as the password, so the code never crosses the network and
cannot be guessed offline from what does. A wrong code costs a live attempt: three end pairing
mode, which also switches itself off after two minutes. Outside pairing mode nothing listens on
port 24831, TCP or UDP.

Updates come from this project's GitHub releases. Each release's SHA256SUMS is signed with an
Ed25519 key that never leaves the release machine; the app checks the signature against the
public key built into it, then the download against its checksum, before installing anything.
The check is one HTTPS request to GitHub when the settings window opens, and can be turned off.

The link is Noise_KK (X25519, ChaCha20-Poly1305, BLAKE2s). Each machine accepts only the one
public key it was given, every connection gets fresh keys, and replayed or altered traffic is
rejected. Only private, link-local and loopback addresses may connect, and a handshake has two
seconds to finish. Nothing leaves the local network.

## Known limits

- Files do not cross, only text up to 256KB and images up to 8MB as PNG.
- X11 only on Linux. Wayland has no way for an app to watch or take over global input.
- The settings window adds about 8MB to the Linux binary and 6MB to the Windows one. The
  background process never runs that code (about 8MB of memory on Linux).
- Windows ignores input sent into administrator windows (Task Manager, installers, UAC) unless
  yunta itself runs as administrator.

## License

MIT. See [LICENSE](LICENSE). The Inter typeface in `assets/` is under the SIL Open Font License
(`assets/Inter-LICENSE.txt`).
