# Yunta engineering review

- **Review date:** 2026-10-02
- **Reviewed revision:** `31eccb0` plus the documentation worktree
- **Scope:** Rust source, Linux and Windows backends, wire protocol, pairing, configuration, updater, packaging/release scripts, tests, and project documentation
- **Review type:** Static review and Linux validation. This was not a penetration test or native Windows runtime test.

## Executive summary

This second pass reviewed the fixes committed after the first report. The critical updater path issue, zero-resistance crossing defect, multi-field configuration races, outbound control-link LAN check, rectangle validation, configuration warnings, Windows tray recovery, log timestamps, and release ordering were addressed with focused tests.

No critical or high-severity issue remains in the reviewed revision. The largest remaining risks are synchronous work and an unbounded queue in the input-control path, manual pairing being able to dial a public address, unauthenticated handshake starvation on a shared LAN, a regression in “last connected” recording when MAC discovery fails, and release automation that can still publish without the Windows artifact.

### Current risk counts

| Severity | Count | Meaning |
|---|---:|---|
| Critical | 0 | No confirmed critical issue remains in this pass |
| High | 0 | No confirmed high-severity issue remains in this pass |
| Medium | 10 | Reliability, security-boundary, compatibility, or release-control work remains |
| Low | 8 | Limited-scope correctness, hardening, portability, or documentation work remains |

## Validation performed

| Check | Result |
|---|---|
| `cargo fmt --check` | Passed |
| `cargo test --release` | Passed: 40 tests, 0 failed, 2 ignored |
| `cargo clippy --release --all-targets -- -D warnings` | Passed |
| `cargo check --release --target x86_64-pc-windows-gnu` | Passed |
| `cargo test --release asks_github -- --ignored` | Passed against the live GitHub Releases API |
| `sh -n package.sh release.sh` | Passed |
| Native Windows execution | Not performed |
| Real two-machine input, gestures, full-screen behavior, Wake-on-LAN, and privileged update installation | Not performed during this review |
| Dependency vulnerability audit | Not performed; `cargo-audit` and `cargo-deny` were unavailable |
| Shell static analysis | Not performed; `shellcheck` was unavailable |

The other ignored test, `tests::drive_a_running_copy`, requires a separately running paired Yunta instance and explicit environment variables.

## Resolved since the first pass

| Code | Original issue | Current status | Evidence |
|---|---|---|---|
| R1 | Predictable updater directory could cross a root privilege boundary | Resolved by unpredictable `0700` directory, `create_new` package, `0600` mode, cleanup, and regression test | `src/update.rs:67-109`, `src/update.rs:225-246` |
| R2 | Update responses had no byte limits | Resolved with limits for JSON, sums, signatures, and packages, enforced by curl and a second reader-side cap | `src/update.rs:84-88`, `src/update.rs:160-184` |
| R3 | Zero resistance crossed on movement parallel to the edge | Resolved with positive initial outward-push requirement and focused test | `src/crossing.rs:171-190`, `src/crossing.rs:335-341` |
| R4 | Pair/use/forget/layout changes were multiple file transactions and could lose concurrent device updates | Resolved with `config::update`, in-lock reparse, one replacement, and a concurrency test | `src/config.rs:308-429`, `src/config.rs:572-601` |
| R5 | Outbound control links could dial public addresses | Resolved for the permanent encrypted link; all resolved candidates are filtered through `is_lan` | `src/link.rs:50-66`, `src/link.rs:208-214` |
| R6 | Full-screen and display rectangles could contain invalid dimensions or overflowing coordinates | Resolved with shared `Rect::sane` validation at protocol, file, and OS boundaries | `src/crossing.rs:66-80`, `src/msg.rs:161-193`, `src/os/linux.rs:112-122`, `src/os/windows.rs:134-148` |
| R7 | Unknown configuration fields and unsafe names were silent | Resolved with collected unknown-field warnings and canonical name/port/hotkey checks | `src/config.rs:60-65`, `src/config.rs:212-293`, `src/main.rs:169-175`, `src/settings.rs:228-239` |
| R8 | Existing private-key files and replacement files were not consistently protected/durable | Substantially resolved with mode repair, unique `0600` replacement, file sync, rename, and directory sync attempt | `src/config.rs:152-162`, `src/config.rs:376-399` |
| R9 | Explorer restart permanently removed the Windows tray icon | Resolved with `TaskbarCreated` registration and icon re-add | `src/os/windows.rs:70-76`, `src/os/windows.rs:452-489` |
| R10 | Logs had no date/PID and both processes could initiate rotation | Resolved for date/PID and rotation ownership | `src/log.rs:22-58` |
| R11 | Release tags were pushed before build/signature validation | Resolved: tests, package build, checksum, and signature verification now precede tag creation/push | `release.sh:24-40` |

R8 and R10 still have narrower residual concerns documented below. R5 applies to the permanent control link, not manual pairing entry.

## Current findings

### F1 — Medium — The core input path still performs blocking work behind an unbounded queue

**Evidence:** `src/main.rs:206`, `src/main.rs:367-420`, `src/main.rs:953-959`, `src/main.rs:981-990`, `src/link.rs:113-129`, `src/clip.rs:35-47`, `src/clip.rs:85-90`

OS capture threads send all events through an unbounded `std::sync::mpsc::channel`. The core thread synchronously hashes and PNG-encodes clipboard images and synchronously encrypts and writes each network message. A write may block until the transport timeout. Raw clipboard images are not rejected by dimensions or raw byte count before hashing and encoding.

**Impact:** A slow peer, large clipboard, or burst of high-rate mouse events can build a backlog. The five-second watchdog returns local input if the core stops, but it does not bound memory, discard stale motion, prevent stale hotkey/key processing after recovery, or reduce clipboard CPU/memory cost.

**Recommendation:** Keep the state machine single-threaded, but put clipboard encoding and socket writes behind a bounded worker queue. Coalesce consecutive motion only; never drop key/button transitions or state-control messages. Reject unreasonable raw image dimensions/bytes before encoding. Add a stalled-receiver test that proves bounded memory and prompt local recovery.

### F2 — Medium — Manual pairing can still dial a non-LAN address

**Evidence:** `src/pair.rs:123-133`, `src/pair.rs:184-188`, `src/pair.rs:245-256`, `src/settings.rs:961-977`

Discovery filters received beacons to LAN addresses, and the permanent link now filters all resolved addresses. The manual pairing paths accept any parsed `IpAddr` and call `TcpStream::connect_timeout` without `link::is_lan`.

**Impact:** A manually entered public address causes an outbound TCP connection to leave the local network. A real Yunta host will reject a public source on its side, but the local-only invariant should be enforced before the SYN is sent. This also makes the code and README security model inconsistent unless users are told to enter private addresses only.

**Recommendation:** Reject non-LAN addresses in `Mode::join` before spawning the worker and again in `pair::join` as defense in depth. Apply the same validation in terminal and GUI paths. Add a test equivalent to `link::tests::never_dials_off_the_lan`.

### F3 — Medium — “Last connected” is not updated when MAC discovery fails

**Evidence:** `src/main.rs:569-582`, `src/wake.rs:28-41`

On link-up, `seen` is updated only inside `if let Some(mac) = self.cfg.peer_mac`. If ARP lookup fails and there is no previously stored MAC, no device update occurs.

**Impact:** A successfully connected device may continue to show “Never connected.” ARP lookup can fail transiently, for loopback/development links, for unsupported neighbor-table formats, or when an address is unavailable. Connection history should not depend on Wake-on-LAN metadata.

**Recommendation:** Update `seen` on every authenticated `Input::Up`. Update `peer_mac` and the device MAC only when a MAC is available. One transaction can handle the optional setting and unconditional device timestamp. Add tests for link-up with and without a discovered/stored MAC.

### F4 — Medium — A LAN client can repeatedly starve link or pairing handshakes

**Evidence:** `src/main.rs:223-230`, `src/pair.rs:205-225`

The permanent listener and pairing listener each process one accepted connection at a time. An unauthenticated LAN client can occupy the link handshake for up to two seconds or pairing for up to five seconds, then reconnect.

**Impact:** This is an availability issue on shared office, school, guest, or conference networks. It does not expose keys, but it can delay a legitimate reconnect indefinitely or consume most of a two-minute pairing window.

**Recommendation:** Use a small fixed number of handshake workers and reject excess work. Add short per-source backoff after failures. This does not require an async runtime.

### F5 — Medium — The release script can publish an incomplete cross-platform release

**Evidence:** `package.sh:39-48`, `package.sh:50-57`, `release.sh:24-40`

`package.sh` treats missing cargo-xwin or `windres` as a warning and continues without `yunta.exe`. It signs whichever files exist. `release.sh` verifies `SHA256SUMS` and its signature but does not require both the `.deb` and `yunta.exe` before tagging and publishing.

**Impact:** A release can pass all current release checks and publish only the Linux artifact even though release notes and project support include Windows.

**Recommendation:** Make `release.sh` explicitly require exactly one versioned `.deb`, `yunta.exe`, `SHA256SUMS`, and `SHA256SUMS.sig`. Keep `package.sh` permissive for local Linux-only builds if useful, but make release mode strict.

### F6 — Medium — There is still no automated CI or native Windows release gate

**Evidence:** no tracked `.github/workflows`, equivalent CI configuration, dependency-policy file, or native Windows test job

The local checks pass, and `release.sh` now runs release tests before tagging. It does not run formatting, Clippy, dependency advisories, script analysis, or a native Windows test. The Windows backend was cross-compiled only in this review.

**Impact:** OS-specific input defects, packaging regressions, dependency advisories, and behavior differences can reach a release through a manual process.

**Recommendation:** Add a minimal protected CI matrix for Linux format/test/Clippy, Windows native check/test, MSVC artifact build, advisory scanning, script analysis, package smoke tests, and release signature verification. Keep a short real-hardware acceptance checklist for features a VM cannot validate.

### F7 — Medium — Windows input ownership and delivery failures remain mostly silent

**Evidence:** `src/os/windows.rs:148-194`, `src/os/windows.rs:209-268`, `src/os/windows.rs:306-346`, `src/os/windows.rs:432-434`, `src/os/windows.rs:603-617`

Important Win32 return values remain unchecked, including `GetCursorPos`, `SetCursorPos`, `SendInput`, notification icon operations, and layered-window updates. `SendInput` can fail because of integrity levels, desktop transitions, or OS state, but the core records the message as handled.

**Impact:** Input or cursor restoration can fail without a useful log or degraded state. This is especially important because the backend owns global hooks and contains most of the project's unsafe code.

**Recommendation:** Check operations that affect input capture, cursor placement, and input injection. Centralize those unsafe calls in small wrappers with explicit preconditions and rate-limited error reporting. Paint-only failures can remain best-effort.

### F8 — Medium — Protocol compatibility is still discovered by disconnecting

**Evidence:** `src/link.rs:20-23`, `src/main.rs:236-250`, `src/msg.rs:8-68`

The Noise prologue remains `yunta/1`, with no application version or capability exchange. A receiver discovers incompatibility only when `Msg::decode` rejects an unknown tag, after which the link drops with a generic reason.

**Impact:** Rolling upgrades are disruptive and hard to diagnose. Both computers must be upgraded together, and additive features cannot be negotiated independently.

**Recommendation:** Exchange a protocol major/minor and capability bitmap as the first authenticated application message. Reject incompatible majors with a status visible in settings. Gate additive message types on negotiated capabilities.

### F9 — Medium — Clipboard transfer has no independent policy control

**Evidence:** `src/main.rs:808-813`, `src/main.rs:953-959`, `src/clip.rs:1-70`

Supported clipboard content automatically follows input. There is no disabled mode, text-only mode, direction-specific policy, or receiver-side refusal independent of keyboard/mouse control.

**Impact:** Passwords, tokens, customer data, or screenshots may be copied to a differently managed peer. Noise protects the network path, not either endpoint.

**Recommendation:** Add one policy setting with `off`, `text`, and `text_and_images`, enforced by both sender and receiver. Show the policy during setup. Decide whether the default is convenience-first or privacy-first.

### F10 — Medium — Private-key configuration validation still has failure paths that continue silently

**Evidence:** `src/config.rs:119-162`, `src/config.rs:299-304`, `src/config.rs:376-399`

The second pass added mode repair and private durable replacements. Remaining gaps are:

- `set_permissions` failure is ignored, so a broadly readable existing private-key file can still be loaded.
- File type and owner are not checked before use; metadata follows symlinks.
- The stored public key is length-checked but not verified to correspond to the stored private key.
- Directory sync failure after replacement is ignored despite the durability guarantee in the comment.

**Impact:** Misconfigured permissions can remain undetected, and a mismatched keypair allows pairing to complete while every subsequent Noise link fails with little guidance.

**Recommendation:** Fail closed or display a blocking warning when permissions cannot be repaired. Require a regular file owned by the current user on Unix. Verify keypair correspondence during load. Return directory-sync errors on filesystems where durability is claimed.

### F11 — Low — Update metadata and version parsing remain ad hoc

**Evidence:** `src/update.rs:39-50`, `src/update.rs:187-201`

GitHub JSON is parsed by substring search rather than JSON rules. Escaped strings are not decoded and field context is not verified. Version comparison splits on dots/hyphens and maps non-numeric segments to zero.

The current GitHub `latest` endpoint and controlled release naming reduce exposure, but malformed or prerelease tags can still compare incorrectly.

**Recommendation:** Either use a small direct JSON dependency and strict semantic-version parser, or reject any tag outside a documented `MAJOR.MINOR.PATCH` grammar. Never coerce invalid components to zero.

### F12 — Low — Updater process I/O and runtime-directory trust can be hardened further

**Evidence:** `src/update.rs:67-80`, `src/update.rs:160-184`

The updater checks that `XDG_RUNTIME_DIR` is a directory but not that it is owned by the user and inaccessible to others. That variable should satisfy those properties by specification, but the code relies on the environment being correct. Curl stdout is drained before stderr; a child producing enough stderr while stdout remains open could deadlock on pipe capacity.

**Recommendation:** Validate owner/mode before using `XDG_RUNTIME_DIR`, otherwise fall back to the sticky system temp directory. Drain stdout and stderr concurrently, or inherit/cap stderr separately.

### F13 — Low — Discovery and listening remain IPv4/topology-limited

**Evidence:** `src/main.rs:210-214`, `src/pair.rs:89-90`, `src/pair.rs:299-314`

The permanent listener binds only `0.0.0.0`. Pairing advertises by limited broadcast and one guessed `/24` based on one local address. Multi-interface hosts, non-/24 networks, VLANs, IPv6-only networks, and broadcast-suppressed Wi-Fi may not discover each other.

Manual address entry helps routed IPv4 setups but does not provide an IPv6 listener.

**Recommendation:** Decide whether broader topology is supported before expanding it. If yes, add dual-stack listening and enumerate interfaces for directed broadcasts. Keep manual private-address entry as fallback.

### F14 — Low — Side files collide for custom configurations in one directory

**Evidence:** `src/config.rs:367`, `src/config.rs:439-457`, `src/main.rs:185`, `src/settings.rs:79-85`

`status`, `quit`, `yunta.conf.lock`, `yunta.lock`, and `settings.lock` use fixed sibling names. Two `YUNTA_CONFIG` files in one directory share locks and runtime state.

**Impact:** This primarily affects development, tests, and attempted multi-profile use.

**Recommendation:** Either derive side-file names from the configuration stem or document that isolated configurations require isolated directories.

### F15 — Low — Log rotation can still split output across files during daemon restart

**Evidence:** `src/log.rs:22-35`

Only the daemon initiates rotation now, which removes the simultaneous-rotation race. However, if the settings process already has `yunta.log` open when a restarted daemon renames it, that settings handle continues writing to `yunta.log.1` while the daemon writes the new file.

Logs also default to normal umask permissions and can contain machine names, addresses, and detailed input-flow diagnostics.

**Recommendation:** Coordinate rotation with the settings process, reopen on inode change, or let one process own log writes. Document log data/permissions and retention.

### F16 — Low — Distribution and artifact trust remain narrow

**Evidence:** `package.sh:8-58`, `build.rs:1-84`

The project publishes a Debian-family x86-64 package and portable Windows x86-64 executable. The Windows binary has embedded metadata but no OS-native Authenticode signature. There is no SBOM, build provenance, ARM package, RPM, installer, or automated uninstall cleanup.

**Recommendation:** Keep the support scope narrow until demand justifies more formats. Prioritize Windows signing and artifact provenance over adding package formats.

### F17 — Low — Wire limits are global rather than message-specific on receive

**Evidence:** `src/link.rs:25-28`, `src/msg.rs:159-160`, `src/clip.rs:11-14`

The sender caps clipboard text at 256 KiB and PNG at 8 MiB, but the receiver accepts either message up to the link-wide 16 MiB maximum. `Gesture` validates direction but not that the finger count is three or four.

A paired peer is already trusted to inject input, so this is protocol robustness rather than a strong security boundary.

**Recommendation:** Apply type-specific receive limits and validate all enumerated fields. Add maximum-boundary tests.

### F18 — Low — `ROADMAP.md` is historical and contradicts the current tree

**Evidence:** `ROADMAP.md:69-72`, `ROADMAP.md:131-155`

The roadmap says T1–T6 are uncommitted and later presents already implemented parity work as future T7/T8 tasks. It is a useful hand-off record but no longer a reliable roadmap.

**Recommendation:** Archive it as a dated implementation hand-off or replace it with a current roadmap that references this review's remaining work.

## Test coverage gaps

### T1 — Core state transitions

Pure helpers are tested well, but `Core` remains coupled to concrete OS, clipboard, tray, and network types. There is no deterministic integration test for `Local → Driving → Driven → Local` under simultaneous crossing, refusal, disconnect, watchdog release, send failure, held keys, and stale queued events.

### T2 — Pairing policy and availability

SPAKE2 success/failure and port closure are covered. Missing cases include rejecting public manual addresses, handshake-worker saturation, per-source retry behavior, and multiple network interfaces.

### T3 — Update installation

Signature verification, size enforcement through the live ignored test, and private-file creation are covered. Missing cases include a real privileged `.deb` update, interrupted package replacement, Windows rollback, invalid runtime-directory ownership, and curl stderr saturation.

### T4 — Configuration security

Sequential operations, concurrent layout/device writes, and parser validation are covered. Missing cases include permission-repair failure, symlink/non-regular files, keypair mismatch, directory-sync failure, and two custom configs sharing a directory.

### T5 — Windows behavior

Cross-compilation cannot validate low-level hook timeouts, cursor hiding, Raw Input, SendInput failures, mixed-DPI monitors, Explorer restart, elevated windows, full-screen detection, or update replacement.

### T6 — Packaging and release completeness

There is no clean-container `.deb` install/uninstall test, clean Windows VM startup test, or assertion that every release contains all advertised artifacts.

## Open questions

### Q1 — Deployment posture

Is Yunta an experimental personal utility, public beta, or intended for managed business deployment? The required bar for signing, support, clipboard policy, and operational controls differs materially.

### Q2 — Threat model

Should Yunta defend only against network outsiders, or also against hostile devices on the same LAN, malicious local users, and compromised paired peers?

### Q3 — Clipboard default

Should clipboard transfer remain automatic, become opt-in, or default to text-only? Can either endpoint refuse clipboard independently of input?

### Q4 — Compatibility policy

Must adjacent minor releases interoperate, or is same-version lockstep permanent? If lockstep remains, should settings identify a version mismatch explicitly?

### Q5 — Network contract

Does “same local network” include routed RFC1918 networks, VPNs, VLANs, IPv6, and multi-interface machines, or only one IPv4 broadcast domain?

### Q6 — Multiple remembered peers

Is the paired-device list only a convenience for replacing the active peer, or should switching among remembered peers become a runtime feature?

### Q7 — Recovery target

What is the maximum acceptable period during which local input may be unavailable? The watchdog uses five seconds, but this should be an explicit product requirement.

### Q8 — Release artifact contract

Is a release allowed to contain only one platform, or must every public version include Linux and Windows artifacts? Current user-facing material implies both.

### Q9 — Windows privilege model

Is running Yunta elevated an accepted workaround for controlling elevated applications? If so, autostart, updates, hooks, and the larger impact of a defect need a defined policy.

### Q10 — Update consent and cadence

Should update checks contact GitHub on every settings launch, only after explicit consent, or on a persisted daily schedule? How should managed proxies/offline environments behave?

### Q11 — Release-key ownership

Where is the release key stored, who can publish, and how are key rotation, revocation, and emergency rollback handled?

### Q12 — Log privacy

What may logs contain, how long should they remain, and what diagnostic bundle should a user provide for support?

## Prioritized next steps

### P0 — Close the remaining small correctness/security boundary defects

1. Reject non-LAN manual pairing addresses before connecting.
2. Record device `seen` on every authenticated link-up, independent of MAC discovery.
3. Require both platform artifacts in release mode.
4. Add one focused regression test for each.

**Exit criteria:** Manual pairing cannot emit public traffic, every successful connection updates history, and a release cannot publish without both advertised binaries.

### P1 — Bound the input path

1. Move clipboard encoding and socket writes to a bounded worker.
2. Coalesce only motion events under pressure.
3. Preserve all key/button/control ordering.
4. Add raw image limits before hashing/encoding.
5. Test a stalled peer and oversized raw clipboard.

**Exit criteria:** Queue memory is bounded and local input returns within the defined recovery target under a stalled receiver.

### P2 — Improve protocol resilience

1. Add authenticated version/capability negotiation.
2. Apply type-specific receive limits.
3. Add a small fixed handshake worker pool and per-source failure backoff.
4. Report version mismatch and refusal reasons in status/settings.

**Exit criteria:** Compatible versions negotiate additive features, incompatible versions fail clearly, and one LAN client cannot monopolize reconnects.

### P3 — Establish release gates

1. Add Linux format/test/Clippy CI.
2. Add native Windows build/test and MSVC artifact jobs.
3. Add advisory scanning and shell analysis.
4. Smoke-test `.deb` installation and portable Windows startup.
5. Verify artifact set, checksums, and signature before publication.

**Exit criteria:** A protected tag cannot become a release unless all code, package, and artifact checks pass for that exact commit.

### P4 — Make Windows failures observable

1. Wrap critical Win32 input/cursor calls.
2. Check and log failures with rate limiting.
3. Surface degraded injection/capture state in settings.
4. Execute the real-hardware checklist for crossing, held keys, privilege boundaries, Explorer restart, and updates.

**Exit criteria:** A Windows support log identifies input-delivery and ownership failures without source-level inference.

### P5 — Define privacy and support policy

1. Write a short threat model.
2. Decide clipboard and update-check defaults.
3. Add `SECURITY.md` with supported versions and reporting instructions.
4. Document release-key rotation and log privacy.
5. Add Windows signing and artifact provenance when moving beyond beta.

**Exit criteria:** README claims, implementation, and support policy describe the same trust boundaries.

### P6 — Reconcile documentation

1. Replace or archive `ROADMAP.md`.
2. Keep this review current as findings are fixed.
3. Add a changelog only when releases need user-facing migration history.

## Release assessment

The previous critical updater issue is fixed by design and focused tests. The current revision is reasonable for continued beta use within the documented Linux/X11 and Windows limits. Before managed business deployment, complete P1 through P5, with particular emphasis on bounded input delivery, native Windows validation, complete release artifacts, protocol diagnostics, and explicit clipboard/security policy.
