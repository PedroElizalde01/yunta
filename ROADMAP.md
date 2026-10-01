# Yunta roadmap (A8): settings window, arrangement, pairing UI, Beamer parity

Hand-off brief for the next agent. Read it top to bottom before touching code. Every task below
has the files to touch, the design already chosen, and how to tell it is done.

## 1. What Yunta is

One keyboard and mouse for two computers on a LAN (a lightweight rewrite of Beamer's KVM
features). Push the pointer through a screen edge, or double-tap Right Ctrl, and input moves to
the other machine. The clipboard (text, PNG) goes along on every switch. Runs in the background
with a tray icon.

- Repo: `~/projects/yunta`. Reference app: `~/projects/beamer` (Python/PySide6, Mac + Windows +
  a Linux port). Beamer is the feature reference only. Yunta is **not** wire-compatible with it.
- Rust, edition 2024, single binary, std threads, no async runtime.
- Platforms: Linux X11 and Windows 10/11. Mac comes later (A7). Wayland is out of scope.
- Owner's hardware: one Linux X11 laptop (eDP 1920x1080 at +0+0, HDMI 1920x1080 at +1920+0)
  and one Windows 10 PC. The Linux machine listens (no `peer` in its config), the PC dials.

## 2. Conventions (follow exactly)

- **Minimal dependencies.** Prefer std and the existing crates. Every new crate needs a reason.
  The release profile is `lto`, `codegen-units = 1`, `strip`, `panic = "abort"`.
- **`ponytail:` comments** mark deliberate simplifications. Each one names the limit and the
  upgrade path, e.g. `// ponytail: displays are polled, subscribe to RandR if a 2s lag matters`.
- Match the existing style: short doc comments in plain English, no comment noise, `rustfmt.toml`
  in the repo (max width is wide, run `cargo fmt`).
- Non-trivial logic gets one small `#[cfg(test)]` test next to it. Pure logic lives in
  `crossing.rs`/`msg.rs`/`config.rs` so it can be tested without an OS.
- Before you finish any task, all of these must pass:
  ```
  cargo test --release
  cargo clippy --release                                  # no warnings
  cargo check --release --target x86_64-pc-windows-gnu   # Windows side compiles
  ```
- Windows code cannot be run here. It is only cross-compiled (`./package.sh` builds
  `dist/yunta.exe` with mingw-w64). The owner tests it on the PC. Say clearly which parts were
  only compiled, never claim them tested.
- Git: commit as the configured user only. No `Co-Authored-By`, no "Generated with" lines.
  Imperative, technical commit messages. Do not commit or push unless the owner asks.
- Security is a feature, not optional: keep the LAN-only checks, the Noise_KK pinning, and the
  private config file (0600 on Linux).

## 3. Code map

| File | Role |
|---|---|
| `src/main.rs` | Entry point (`yunta`, `yunta init`, `yunta pair`), the `Core` state machine (`Local`/`Driving`/`Driven`), link thread, held-key release |
| `src/crossing.rs` | Pure geometry: `Edge`, `Rect`, push-through resistance (`Crossing`), `pos_along`, `entry_point`, `step_within`, `DoubleTap` |
| `src/msg.rs` | Wire messages and their encoding (`Enter`, `Leave`, `Move`, `Key`, `Button`, `Scroll`, `ClipText`, `ClipPng`, `Ping`) |
| `src/link.rs` | Noise_KK_25519_ChaChaPoly_BLAKE2s link (`snow`), LAN-only check `is_lan`, 2s handshake deadline |
| `src/pair.rs` | Pairing mode: UDP beacon on 24831, discovery, SPAKE2 with a 6-digit code (3 attempts, 120s), console-driven today |
| `src/config.rs` | `yunta.conf` (`key = value`), `init`, `load`, `set` (rewrites one line in place) |
| `src/clip.rs` | Clipboard via `arboard` |
| `src/autostart.rs` | Start at login (XDG autostart / HKCU Run) |
| `src/icon.rs` | Tray icon pixels per `Look` (Waiting, Linked, Paused) |
| `src/os/linux.rs` | X11: XInput2 raw events, grab on a second connection with a blank cursor, XTest injection, RandR displays, `ksni` tray |
| `src/os/windows.rs` | Win32: low-level hooks + Raw Input, `SendInput`, `EnumDisplayMonitors`, notification-area icon and menu |
| `src/settings.rs` | The settings window (`yunta settings`, egui): pages, arrangement canvas, pairing page |
| `src/widgets.rs` | The window's look: palettes, fonts, cards, rows, switches, icons, the two-computer drawing |
| `src/fx.rs` | Crossing animations: software-drawn glow and ripple, played on their own thread |
| `package.sh` | Builds the `.deb` and the `.exe` |

Config paths: `~/.config/yunta/yunta.conf` (Linux), `%APPDATA%\yunta\yunta.conf` (Windows).
Next to it: `status` (written by the running app for the window), `yunta.lock` and
`settings.lock`.
Ports: TCP 24830 link, TCP+UDP 24831 pairing.

## 4. Status (2026-10-01)

T1 to T6 are done, uncommitted, in the working tree. "Tested here" means on this Linux machine,
with two instances linked over loopback (separate `YUNTA_CONFIG`s, `port = 24900`) and the window
driven with xdotool. Nothing has run on the real PC yet.

| Task | What it does | Where | Tested |
|---|---|---|---|
| Edge bug | The edge setting no longer has to be right on both machines: T3's layout sync keeps them in step | `main.rs` | Here; the original stopgap was replaced by T3 |
| T1 | Blank system cursors on Windows while driving, restored on return, quit and next start | `os/windows.rs` `hide_cursors` | Compiled only |
| T2 | `yunta.lock`: a second copy started from a menu opens the settings window, from a terminal says "already running" | `config::lock`, `main.rs` `run` | Here |
| T3 | `Msg::Displays` and `Msg::Layout`; crossing only along the touching stretch; newer layout wins, tie goes to the dialer; a hand edit to the config is stamped and sent | `crossing.rs` `Layout`, `msg.rs`, `main.rs` | Here (unit tests, two instances) |
| T4 | `yunta settings`: Overview, Arrangement, Pairing, Connection. Status file out, config file in, reloaded live. Tray: "Settings…", "Pair a new computer…", left click opens settings | `settings.rs`, `main.rs` `write_status`/`reload` | Here on Linux; Windows compiled only |
| T5 | Drag-and-drop arrangement, snaps to the nearest edge, zooms out while dragging | `settings.rs` `canvas`, `snap`, `place` | Here |
| T6 | `pair::Mode` engine shared by console and window; pairing mode closes its ports when it ends; daemon restarts itself when the key, peer or port changes; first run without a terminal opens the Pairing page | `pair.rs`, `settings.rs` `pump`/`pairing`, `main.rs` | Here: ports close, wrong-address check, restart. A full pairing between two machines is not tested |

| Effects | Crossing animation: edge glow while pushing (eased to the push), a flash on crossing, a ripple on landing. Drawn in software into a click-through overlay (X11: 32-bit override-redirect window with an empty input shape, needs a compositor; Windows: layered `WS_EX_TRANSPARENT` window). `effects = yes/no`, toggle and Preview on Overview | `fx.rs`, `os/*.rs` `Overlay`, `main.rs` `push`/`arrive` | Here, recorded from the screen; Windows overlay compiled only |
| Design | Inter (Latin subset, `assets/`), dark and light palettes, Appearance setting (`theme = system/light/dark`), sidebar icons, two-computer hero on Overview with a live link, code tiles and countdown in Pairing, dot-grid arrangement canvas, page fade | `widgets.rs`, `settings.rs` | Here, both themes |

| Parity (2026-10-01) | Corner crossing (drag diagonally; `corner = yes`, diagonal push into an 8 px box), full-screen pause, send/receive switches (`Msg::Hello` carries name and receive), pointer and scroll speed, Ctrl/Super swap, kept keys (`keep = volume, media, side_buttons, print_screen`; X11 lets go of the grab for an instant to play them here), media keys (HID 0xF0–0xF3 private usages, Windows virtual keys), Linux XI 2.4 swipes → Windows shortcuts (`Msg::Gesture`), Wake-on-LAN (`wake.rs`, MAC from ARP), hold trigger, device rename, signed one-click updates (`update.rs`, `package.sh` signs with `~/.config/yunta-release/signing.pem`), states Not running / Not paired / Waiting (pairing) / Offline / Waking / Connected / Paused | many | Unit tests, two instances over loopback, a headless egui drag test. Not tested: gestures, kept keys, media keys, wake, full screen and updates on real hardware; anything Windows |

Sizes: Linux binary 3.0MB → 11.6MB, Windows `.exe` 0.9MB → 6.8MB (eframe, plus 2.3MB of
accesskit for screen readers, kept on purpose). The background process uses about 8MB of memory.
The `.deb` now depends on the OpenGL and X11 libraries the window opens at run time.

## 5. Decisions already made

- **D1 Settings window: `eframe`/egui with the `glow` backend, in its own process.** The tray
  menu's "Settings…" item runs `yunta settings` (the same binary). The background process stays
  as small as it is now, and only the open window pays for the GUI. Measure the binary size
  before and after and report it (budget: about +6MB). Use `default-features = false` and only
  the features needed (`glow`, `default_fonts`, `x11` on Linux). Do not enable `wgpu` or Wayland.
  Native Win32 plus GTK was rejected because every page, including the drag-and-drop canvas,
  would have to be written twice.
- **D2 Window ⇄ background process talk through files in the config folder.** No sockets, no
  new crates.
  - Window → daemon: the window writes settings with `config::set`. The daemon stats
    `yunta.conf` on each loop tick (it already wakes every 250ms) and reloads on a new mtime.
  - Daemon → window: the daemon writes `status` next to `yunta.conf` (same `key = value`
    format, written to a temp file then renamed, only when something changed) with
    `linked`, `input`, `paused`, `displays`, `peer_displays` (`rtt_ms` comes with T7). The window polls
    it about four times a second.
  - The config folder is private to the user, so this needs no authentication. Mark it
    `// ponytail: file IPC, switch to a local socket if latency or races matter`.
- **D3 Arrangement model: two matching intervals, in fractions.** The layout is our `edge`
  plus `[a0, a1]`, the part of our edge span (0..1) that touches the peer, facing `[b0, b1]` of
  the peer's span. The default `[0,1]→[0,1]` is today's behaviour. Fractions make it
  independent of resolution and DPI, and the peer's view is the mirror image: `edge.opposite()`
  with the two intervals swapped.
- **D4 Pairing discovery lives in the settings window.** The tray gets a "Pair a new
  machine…" item that opens the window on the Pairing page. The page lists machines currently
  in pairing mode (from the beacon), shows this machine's code, and takes the other machine's
  code. A tray menu cannot take text input, so the code entry cannot live there. The owner
  agreed either place is fine.
- **D5 Hide the idle cursor on Windows with `SetSystemCursor`.** Linux already blanks its
  cursor while driving (the grab uses `blank_cursor`). Windows parks a visible cursor in the
  middle of the primary screen (`os/windows.rs`, `grab`).

## 6. Tasks, in order

### T1 to T6

Done, see section 4. Owner checks on the real machines, in this order: both directions cross;
the PC's cursor disappears while Linux has input and comes back on return, on quit, and on the
next start after killing yunta mid-drive; dragging in Arrangement on either machine moves the
other's; pairing the two from the window.

### T7 Beamer parity, high value

Reference implementations in `~/projects/beamer/win_app` are named for each item.

- Pause crossing while a full-screen app has focus (`desktop_win.py`). X11: `_NET_WM_STATE_FULLSCREEN`
  on the active window. Windows: foreground window rect equals its monitor rect.
- A separate on/off for each direction (this machine may drive / may be driven).
- Pointer and scroll speed for the peer's input on this machine (Keyboard page).
- Round trip: add `Pong` echoing `Ping`, show it on Overview.
- Windows Firewall check with a one-click fix on the Connection page (`firewall_win.py`).
- Hold the trigger key as an alternative to double-tap (`capture_win.py`, `style == "hold"`).

### T8 Beamer parity, medium and later

- Keys and buttons that stay on this machine (`ignored.py`).
- Wake-on-LAN when switching to a sleeping machine, MAC read from the ARP table (`wol.py`).
- Media keys across.
- Later: crossing effects and edge glow (`edge_glow.py`, `effects.py`), update check, Mac (A7).

## 7. Risks

- **R1 Size.** The GUI makes the binary bigger. It is acceptable only because it runs in its own
  process. Report the measured sizes in the commit or hand-off.
- **R2 Blank cursor left behind on Windows.** `SetSystemCursor` is session-wide. T1's restore
  on startup covers a crash. Test it.
- **R3 Protocol break.** T3 adds messages, so old and new builds cannot link. Update both
  machines together.
- **R4 Config write races.** The daemon (learned edge, layout sync) and the window both call
  `config::set`. Writes are rare and whole-line, but if they show up, route all writes through
  the daemon.
- **R5 Windows is only cross-compiled here.** Every Windows change needs the owner's test on
  the PC before it counts as done.

## 8. Testing on the real machines

1. `./package.sh`, install the `.deb` on Linux, copy `dist/yunta.exe` to the PC.
2. Start both. Run Linux from a terminal to read the log (`yunta`).
3. Check both directions by edge and by double-tapping Right Ctrl, clipboard both ways, held
   keys released on switch, and that the idle machine's cursor is hidden.
