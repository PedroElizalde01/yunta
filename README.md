<p align="center">
  <img src="assets/yunta.svg" width="96" alt="Yunta logo">
</p>

<h1 align="center">Yunta</h1>

<p align="center">
  One keyboard and mouse for two computers on the same local network.
</p>

<p align="center">
  <a href="https://github.com/PedroElizalde01/yunta/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/PedroElizalde01/yunta?sort=semver"></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-2f855a"></a>
  <img alt="Linux X11" src="https://img.shields.io/badge/Linux-X11-3f7f4c">
  <img alt="Windows 10 and 11" src="https://img.shields.io/badge/Windows-10%20%7C%2011-2563eb">
  <img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-b7410e">
</p>

Yunta lets two nearby computers behave like one continuous workspace. Push the pointer through a configured screen edge, or use the keyboard shortcut, and the same keyboard, mouse, and clipboard continue on the other machine. There is no account and no relay service: input traffic travels directly between the paired computers.

> **Project status:** Yunta is a `0.x` project for Linux/X11 and Windows 10/11 on x86-64. Wayland and macOS are not supported. Both computers should run the same Yunta version.

## What it does

| Capability | Behavior |
|---|---|
| Seamless switching | Cross through a shared edge or corner, double-tap Right Ctrl, or hold Right Ctrl when hold mode is selected. |
| Screen arrangement | Drag the peer around the local display layout. Yunta maps the touching edge across different resolutions and monitor arrangements. |
| Input forwarding | Keyboard, pointer, wheel, media keys, mouse side buttons, and supported Linux touchpad gestures can cross the link. |
| Clipboard handoff | Text up to 256 KiB and PNG images up to 8 MiB follow input when control changes sides. |
| Local controls | Configure send/receive permissions, pointer and scroll speed, modifier swapping, edge resistance, per-device behavior, and keys that stay local. |
| Full-screen protection | A full-screen game or video can keep its screen edge while the keyboard shortcut remains available. |
| Wake-on-LAN | Yunta learns the peer's hardware address after a connection and can wake it when switching. |
| Background operation | A tray icon reports state, opens settings, pauses edge crossing, controls autostart, starts pairing, and quits. |
| Signed updates | Release checksums are signed with Ed25519 and verified before an update is installed. |

## Platform support

| Platform | Status | Distribution |
|---|---|---|
| Linux x86-64 with X11 | Supported | Debian/Ubuntu `.deb` |
| Windows x86-64 | Supported on Windows 10 and 11 | Portable `yunta.exe` |
| Linux with Wayland | Unsupported | Global input capture and injection are not available to this application model. |
| macOS | Not implemented | None |

Windows does not allow a normal desktop process to inject input into administrator windows, installers, Task Manager running elevated, or UAC prompts. Run Yunta at the same integrity level as the application being controlled.

## Install

Download the current artifacts from the [latest release](https://github.com/PedroElizalde01/yunta/releases/latest).

### Linux

Install the downloaded package with `sudo apt install ./yunta_<version>_amd64.deb`, then start **Yunta** from the application menu.

Yunta requires an X11 session. If the desktop is using Wayland, log out and choose an Xorg/X11 session from the login screen.

### Windows

Download `yunta.exe`, place it in a stable location, and run it. The executable is portable and includes its C runtime. When Windows Firewall asks, allow Yunta on **private networks**.

## Quick start

1. Install and start the same Yunta version on both computers.
2. Open **Pairing** on both. On either computer, enter the six-digit code displayed by the other one. Pairing mode closes after two minutes or three wrong attempts.
3. Open **Arrangement** and drag the other computer to its physical position. Drop it diagonally to use corner crossing.
4. In **Overview**, enable **Start at login** if Yunta should remain available after sign-in.

Push the pointer through the configured edge to move control. By default, double-tapping **Right Ctrl** switches without moving the pointer. Moving the controlled computer's own mouse takes control back locally.

## Runtime model

The background process owns input capture, the encrypted link, clipboard handoff, and the tray icon. The settings window runs as a separate process, so the GUI is not active during normal background use.

`Physical input → OS backend → Core state machine → Noise-encrypted TCP → peer OS backend → native input injection`

| Component | Responsibility |
|---|---|
| `src/main.rs` | Process lifecycle, link supervision, configuration reloads, and the `Local` / `Driving` / `Driven` state machine |
| `src/crossing.rs` | Display geometry, edge resistance, corner crossing, pointer mapping, and shortcut timing |
| `src/link.rs` | Framed Noise transport, peer-key pinning, timeouts, and LAN source checks |
| `src/pair.rs` | Discovery, six-digit SPAKE2 pairing, attempt limits, and key exchange |
| `src/os/linux.rs` | XInput2 capture, X11 grabs, XTest injection, RandR displays, overlays, and tray integration |
| `src/os/windows.rs` | Low-level hooks, Raw Input, SendInput, display discovery, overlays, and notification-area integration |
| `src/settings.rs` / `src/widgets.rs` | Native settings application and reusable interface components |
| `src/config.rs` | Configuration parsing, device records, file locking, and atomic file replacement |
| `src/update.rs` | GitHub release discovery, signature and checksum verification, and installation |

The implementation uses standard threads and channels rather than an async runtime. Pure protocol and geometry logic is kept outside the OS backends and covered by unit tests.

## Security and privacy

### Pairing

Pairing uses SPAKE2 with the displayed code as the password. The code is not sent over the network and captured pairing traffic does not provide an offline password test. Each wrong code requires a live attempt; three wrong attempts end pairing mode.

Pairing discovery and code entry use TCP and UDP port `24831` only while pairing mode is active.

### Session encryption

The control link uses `Noise_KK_25519_ChaChaPoly_BLAKE2s` over TCP. Each computer pins the public key learned during pairing. Noise provides authenticated encryption, fresh session keys, ordered nonces, and tamper detection.

The long-lived link uses TCP port `24830`. Dialing and incoming links are restricted to private, link-local, or loopback addresses and must complete the handshake within two seconds.

### Local secrets

The configuration contains the machine's private key. A newly created Linux configuration is mode `0600`; `%APPDATA%` supplies the per-user boundary on Windows. Treat the configuration and its backups as credentials.

### Updates and external traffic

Input, clipboard, and pairing discovery are designed for the local network. When entering a pairing address manually, use only a private or link-local address; the current enforcement gap is documented in the engineering review. If update checks are enabled, opening the settings window sends an HTTPS request to the project's GitHub Releases API. Installation proceeds only after the signed `SHA256SUMS` file and the selected artifact checksum both verify.

| Purpose | Protocol | Port | Lifetime |
|---|---|---:|---|
| Encrypted input link | TCP | 24830 | While Yunta is running and paired |
| Pairing key exchange | TCP | 24831 | Pairing mode only |
| Pairing discovery | UDP broadcast | 24831 | Pairing mode only |
| Update check/download | HTTPS | 443 | When requested or when settings opens with checks enabled |

For current security and reliability findings, see [ENGINEERING_REVIEW.md](ENGINEERING_REVIEW.md).

## Configuration and operational files

Yunta creates its configuration on first start and reloads supported changes while running.

| Platform | Configuration directory |
|---|---|
| Linux | `~/.config/yunta/` or `$XDG_CONFIG_HOME/yunta/` |
| Windows | `%APPDATA%\yunta\` |

The main files are:

| File | Purpose |
|---|---|
| `yunta.conf` | Keys, active peer, remembered devices, layout, and user settings |
| `status` | Current link/input state consumed by the settings process |
| `yunta.log` | Runtime and settings log, rotated at approximately 1 MiB |
| `yunta.lock` | Single-instance lock for the background process |
| `settings.lock` | Single-instance lock for the settings window |

`YUNTA_CONFIG=/path/to/yunta.conf` overrides the configuration path for development and isolated test instances.

Common settings include:

| Setting | Default | Meaning |
|---|---|---|
| `edge` | `right` | Local edge leading to the peer: `left`, `right`, `top`, or `bottom` |
| `resistance` | `120` | Raw pointer distance required to cross |
| `hotkey` | `0xe4` | USB HID usage for the switching key; `0xe4` is Right Ctrl |
| `trigger` | `double` | `double` for double-tap or `hold` for hold-to-switch |
| `send` / `receive` | `yes` | Whether input may leave or enter this computer |
| `fullscreen` | `yes` | Keep an edge closed where a full-screen application occupies the peer display |
| `pointer_speed` / `scroll_speed` | `1.0` | Scale peer input applied on this computer |
| `keep` | empty | Comma-separated groups: `volume`, `media`, `side_buttons`, `print_screen` |
| `effects` | `yes` | Edge glow, crossing flash, and landing ripple |
| `wake` | `yes` | Send Wake-on-LAN when switching to an offline peer with a learned address |
| `updates` | `yes` | Check GitHub for a newer release when the settings application opens |

Prefer the settings application over manual edits. The device keys, layout fractions, timestamps, and restart marker are internal fields and may change between releases.

## Command line

| Command | Purpose |
|---|---|
| `yunta` | Start the background process; pair in the terminal on first run when a terminal is attached |
| `yunta settings` | Open the settings application |
| `yunta settings pairing` | Open settings directly on Pairing |
| `yunta pair` | Run pairing in a terminal |
| `yunta init` | Create the configuration and print this machine's public key |

## Troubleshooting

### The computers stay offline

- Confirm both run the same Yunta version and are on the same private network.
- Allow TCP `24830` through the Windows firewall on private networks.
- Confirm only one side has a `peer` address. Pairing sets the dialing/listening roles automatically.
- Open **About → Copy log** on both machines and compare the connection and handshake messages.

### Edge crossing does not start

- Confirm **Use this keyboard and mouse on…** is enabled.
- Confirm edge crossing is not paused.
- Check **Arrangement** for the active edge and touching span.
- A full-screen application may reserve that edge; the keyboard shortcut remains available.

### Linux reports that input capture is unavailable

Run an X11 session and confirm the X server exposes XInput2 and XTest. Wayland sessions are unsupported.

### No tray icon appears on Linux

The desktop needs a StatusNotifierItem/AppIndicator-compatible tray host. Yunta continues running without the icon; use `yunta settings` to open configuration directly.

## Build and validate

Yunta uses Rust 2024 and a committed `Cargo.lock`. Build the local platform with `cargo build --release`.

Before submitting a change, run:

- `cargo fmt --check`
- `cargo test --release`
- `cargo clippy --release --all-targets -- -D warnings`
- `cargo check --release --target x86_64-pc-windows-gnu`

Add the validation target once with `rustup target add x86_64-pc-windows-gnu` if it is not installed.

`./package.sh` rebuilds `dist/`, creates the Linux `.deb`, and also creates `dist/yunta.exe` when the Windows cross-build tools are installed.

For the Windows MSVC build from Linux, install `mingw-w64`, add the Rust target with `rustup target add x86_64-pc-windows-msvc`, and install cargo-xwin with `cargo install --locked cargo-xwin`. The script uses cargo-xwin for the executable and MinGW `windres` for Windows resources. cargo-xwin downloads Microsoft SDK/runtime files and requires acceptance of Microsoft's license.

The maintainer release flow is `./release.sh`. It validates the tree, runs release tests, builds and verifies the signed artifacts, then creates and pushes the version tag before publishing to GitHub Releases. With the documented Linux and Windows toolchains installed, the release contains both platform artifacts.

## Known limits

- Exactly one peer is active at a time, although previously paired computers are remembered.
- Clipboard transfer supports plain text and PNG images, not files or rich clipboard formats.
- Linux support is X11-only and the distributed package currently targets Debian/Ubuntu x86-64.
- Pairing discovery uses IPv4 broadcast; a peer can be entered by address when discovery does not cross a subnet.
- Windows cannot inject into applications running at a higher privilege level.
- Protocol compatibility is strict. Upgrade both computers together.

## Engineering status

The detailed code review, open questions, confirmed defects, operational risks, and prioritized next steps are maintained in [ENGINEERING_REVIEW.md](ENGINEERING_REVIEW.md).

## License

Yunta is available under the [MIT License](LICENSE). The bundled Inter typeface is licensed under the SIL Open Font License; see [`assets/Inter-LICENSE.txt`](assets/Inter-LICENSE.txt).
