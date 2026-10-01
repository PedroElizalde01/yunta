//! Crossing animations, drawn in software into a small see-through window that clicks pass
//! through: a glow on the edge while the pointer pushes against it, a flash as it crosses, and a
//! ripple where it lands. A thread of its own plays them, so the core never waits on a frame.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::crossing::Edge;
use crate::os;

/// Yunta green, as in the tray icon.
pub const COLOR: [f32; 3] = [0x3f as f32 / 255.0, 0xb9 as f32 / 255.0, 0x50 as f32 / 255.0];
const FRAME: Duration = Duration::from_millis(16);
const RIPPLE: Duration = Duration::from_millis(520);
const FLASH: Duration = Duration::from_millis(240);
/// How fast a glow follows the push, per frame: eased rather than jumping.
const FOLLOW: f32 = 0.35;
/// The glow's length along the edge and its depth into the screen, in pixels.
const GLOW_LEN: u32 = 420;
const GLOW_DEPTH: u32 = 36;
const RIPPLE_SIZE: u32 = 260;

enum Cmd {
    /// Pushing against `edge` at (x, y), this far to crossing. 0 lets the glow fade.
    Push(Edge, i32, i32, f32),
    /// Crossed out through `edge` at (x, y).
    Leave(Edge, i32, i32),
    /// The pointer arrived at (x, y).
    Arrive(i32, i32),
}

/// What is on screen now.
enum Playing {
    Idle,
    Glow { edge: Edge, x: i32, y: i32, shown: f32, target: f32 },
    Flash { edge: Edge, x: i32, y: i32, start: Instant },
    Ripple { x: i32, y: i32, start: Instant },
}

#[derive(Clone)]
pub struct Fx(mpsc::Sender<Cmd>);

impl Fx {
    /// None where see-through windows are not possible, as on X11 without a compositor.
    pub fn start() -> Option<Fx> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        thread::spawn(move || {
            // Made on this thread: on Windows a window belongs to the thread that made it.
            let overlay = os::Overlay::new();
            let ok = overlay.is_some();
            let _ = ready_tx.send(ok);
            if let Some(overlay) = overlay {
                play(overlay, &rx);
            }
        });
        ready.recv().ok().filter(|ok| *ok).map(|_| Fx(tx))
    }

    pub fn push(&self, edge: Edge, x: i32, y: i32, strength: f32) {
        let _ = self.0.send(Cmd::Push(edge, x, y, strength));
    }

    pub fn leave(&self, edge: Edge, x: i32, y: i32) {
        let _ = self.0.send(Cmd::Leave(edge, x, y));
    }

    pub fn arrive(&self, x: i32, y: i32) {
        let _ = self.0.send(Cmd::Arrive(x, y));
    }
}

fn play(mut overlay: os::Overlay, rx: &mpsc::Receiver<Cmd>) {
    let mut playing = Playing::Idle;
    loop {
        // Idle, nothing to draw: sleep until told. Otherwise wake for the next frame.
        let cmd = match playing {
            Playing::Idle => rx.recv().ok(),
            _ => match rx.recv_timeout(FRAME) {
                Ok(cmd) => Some(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
        };
        if let Some(cmd) = cmd {
            playing = apply(cmd, playing);
            // Everything else waiting too, before drawing, so a fast mouse never builds a backlog.
            for cmd in rx.try_iter() {
                playing = apply(cmd, playing);
            }
        }
        playing = frame(&mut overlay, playing);
        overlay.pump();
    }
}

fn apply(cmd: Cmd, playing: Playing) -> Playing {
    match (cmd, playing) {
        (Cmd::Push(edge, x, y, target), Playing::Glow { shown, .. }) => Playing::Glow { edge, x, y, shown, target },
        (Cmd::Push(_, _, _, 0.0), Playing::Idle) => Playing::Idle,
        (Cmd::Push(edge, x, y, target), Playing::Idle) => Playing::Glow { edge, x, y, shown: 0.0, target },
        // A ripple or flash plays out; a push meanwhile waits for the next one.
        (Cmd::Push(..), other) => other,
        (Cmd::Leave(edge, x, y), _) => Playing::Flash { edge, x, y, start: Instant::now() },
        (Cmd::Arrive(x, y), _) => Playing::Ripple { x, y, start: Instant::now() },
    }
}

/// Draws one frame of `playing`, and says what plays next.
fn frame(overlay: &mut os::Overlay, playing: Playing) -> Playing {
    match playing {
        Playing::Idle => Playing::Idle,
        Playing::Glow { edge, x, y, shown, target } => {
            let shown = shown + (target - shown) * FOLLOW;
            if target == 0.0 && shown < 0.02 {
                overlay.hide();
                return Playing::Idle;
            }
            show_glow(overlay, edge, x, y, shown);
            Playing::Glow { edge, x, y, shown, target }
        }
        Playing::Flash { edge, x, y, start } => {
            let t = start.elapsed().as_secs_f32() / FLASH.as_secs_f32();
            if t >= 1.0 {
                overlay.hide();
                return Playing::Idle;
            }
            show_glow(overlay, edge, x, y, 1.0 + 0.6 * (1.0 - t) - 1.6 * t * t);
            Playing::Flash { edge, x, y, start }
        }
        Playing::Ripple { x, y, start } => {
            let t = start.elapsed().as_secs_f32() / RIPPLE.as_secs_f32();
            if t >= 1.0 {
                overlay.hide();
                return Playing::Idle;
            }
            let half = (RIPPLE_SIZE / 2) as i32;
            overlay.show(x - half, y - half, RIPPLE_SIZE, RIPPLE_SIZE, &ripple(RIPPLE_SIZE, t));
            Playing::Ripple { x, y, start }
        }
    }
}

fn show_glow(overlay: &mut os::Overlay, edge: Edge, x: i32, y: i32, strength: f32) {
    let vertical = matches!(edge, Edge::Left | Edge::Right);
    let (w, h) = if vertical { (GLOW_DEPTH, GLOW_LEN) } else { (GLOW_LEN, GLOW_DEPTH) };
    // The window sits against the edge, centred on the pointer, inside the screen.
    let (half, depth) = ((GLOW_LEN / 2) as i32, GLOW_DEPTH as i32);
    let (left, top) = match edge {
        Edge::Left => (x, y - half),
        Edge::Right => (x - depth + 1, y - half),
        Edge::Top => (x - half, y),
        Edge::Bottom => (x - half, y - depth + 1),
    };
    overlay.show(left, top, w, h, &glow(edge, w, h, strength.clamp(0.0, 1.6)));
}

/// The edge glow, `w` x `h` premultiplied BGRA: brightest in a thin line on the edge, fading
/// into the screen and towards both ends.
pub fn glow(edge: Edge, w: u32, h: u32, strength: f32) -> Vec<u8> {
    let (len, depth) = if matches!(edge, Edge::Left | Edge::Right) { (h, w) } else { (w, h) };
    let mut px = vec![0u8; (w * h * 4) as usize];
    for py in 0..h {
        for pxl in 0..w {
            // `d` is the distance from the edge, `a` the position along it from the middle.
            let (a, d) = match edge {
                Edge::Left => (py, pxl),
                Edge::Right => (py, w - 1 - pxl),
                Edge::Top => (pxl, py),
                Edge::Bottom => (pxl, h - 1 - py),
            };
            let along = (a as f32 - len as f32 / 2.0) / (len as f32 / 2.0);
            let ends = (1.0 - along * along).max(0.0).powf(1.5);
            let d = d as f32 / depth as f32;
            let core = (-d * 28.0).exp();
            let haze = (-d * 4.5).exp() * 0.55;
            let alpha = (strength * ends * (core + haze)).min(1.0);
            // A white-hot core, the colour around it.
            let white = (core * strength - 0.6).clamp(0.0, 1.0) * 0.6;
            put(&mut px, (py * w + pxl) as usize, alpha, white);
        }
    }
    px
}

/// The landing ripple at time `t` from 0 to 1: a soft burst at the centre and two rings
/// spreading out, the second just behind the first.
pub fn ripple(size: u32, t: f32) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let c = size as f32 / 2.0;
    let ease = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
    let reach = c - 6.0;
    let rings = [(ease(t), 1.0 - t), (ease((t - 0.18) / 0.82), (1.0 - t) * 0.7)];
    let burst = (1.0 - t * 2.2).max(0.0);
    for y in 0..size {
        for x in 0..size {
            let r = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let mut alpha = burst * (-(r / 22.0).powi(2)).exp() * 0.9;
            for (spread, fade) in rings {
                if fade <= 0.0 {
                    continue;
                }
                let radius = 8.0 + spread * (reach - 8.0);
                let width = 2.5 + 5.0 * (1.0 - spread);
                alpha += fade.powf(1.4) * (-((r - radius) / width).powi(2)).exp();
            }
            let white = (burst * (-(r / 10.0).powi(2)).exp()).min(1.0) * 0.7;
            put(&mut px, (y * size + x) as usize, alpha.min(1.0), white);
        }
    }
    px
}

/// One premultiplied BGRA pixel: the colour mixed towards white by `white`, at `alpha`.
fn put(px: &mut [u8], i: usize, alpha: f32, white: f32) {
    if alpha <= 0.002 {
        return;
    }
    let channel = |c: f32| ((c + (1.0 - c) * white) * alpha * 255.0).round() as u8;
    px[i * 4..i * 4 + 4].copy_from_slice(&[channel(COLOR[2]), channel(COLOR[1]), channel(COLOR[0]), (alpha * 255.0).round() as u8]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(px: &[u8], w: u32, x: u32, y: u32) -> u8 {
        px[((y * w + x) * 4 + 3) as usize]
    }

    #[test]
    fn glow_hugs_the_edge_and_fades_inwards() {
        let px = glow(Edge::Right, 36, 420, 1.0);
        let (edge, inside) = (alpha(&px, 36, 35, 210), alpha(&px, 36, 0, 210));
        assert!(edge > 200 && inside < 60, "{edge} {inside}");
        assert!(alpha(&px, 36, 35, 2) < 20); // the ends fade out
        assert!(glow(Edge::Right, 36, 420, 0.0).iter().all(|&b| b == 0));
        // Premultiplied: no channel brighter than its alpha.
        assert!(px.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    }

    #[test]
    fn ripple_spreads_and_fades() {
        let early = ripple(260, 0.05);
        let late = ripple(260, 0.9);
        assert!(alpha(&early, 260, 130, 130) > 150);
        assert!(alpha(&late, 260, 130, 130) < 10);
        // The ring has moved out by the end, and is fainter.
        let ring_late = (0..130).map(|x| alpha(&late, 260, x, 130)).max().unwrap();
        assert!(ring_late > 0 && ring_late < 120);
        assert_eq!(alpha(&ripple(260, 1.0), 260, 130, 130), 0);
    }
}
