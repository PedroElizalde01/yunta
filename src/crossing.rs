//! When the pointer crosses, and where it lands. Pure logic: the OS backends feed it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn from_u8(v: u8) -> Option<Edge> {
        [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom].get(v as usize).copied()
    }

    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    /// One pixel step outwards through this edge.
    fn step(self) -> (i32, i32) {
        match self {
            Edge::Left => (-1, 0),
            Edge::Right => (1, 0),
            Edge::Top => (0, -1),
            Edge::Bottom => (0, 1),
        }
    }

    /// Left and Right are vertical lines, so a position along them is a y.
    fn vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// One display in desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Debug, PartialEq)]
pub enum Push {
    None,
    /// Pushing against the edge, this fraction of the way to crossing.
    Building(f32),
    /// Crossed, `pos` of the way along the edge.
    Cross(u16),
}

/// Push-through resistance: the pointer has to keep pushing into the edge for `resistance`
/// pixels of raw motion before it crosses, so overshooting a close button does not switch.
pub struct Crossing {
    pub edge: Edge,
    pub resistance: i32,
    pressure: i32,
}

impl Crossing {
    pub fn new(edge: Edge, resistance: i32) -> Crossing {
        Crossing { edge, resistance, pressure: 0 }
    }

    /// Feed every pointer motion: where the pointer is now, and the raw motion that moved it.
    pub fn motion(&mut self, displays: &[Rect], x: i32, y: i32, dx: i32, dy: i32, dragging: bool) -> Push {
        let (sx, sy) = self.edge.step();
        let push = dx * sx + dy * sy;
        // ponytail: pressure only resets on backing off, add a time decay if a slow creep crosses by surprise
        if dragging || push < 0 || !at_outer_edge(displays, self.edge, x, y) {
            self.pressure = 0;
            return Push::None;
        }
        self.pressure += push;
        if self.pressure < self.resistance {
            return Push::Building(self.pressure as f32 / self.resistance.max(1) as f32);
        }
        self.pressure = 0;
        Push::Cross(pos_along(displays, self.edge, x, y))
    }
}

/// True where no display lies beyond `edge`, so an edge between two monitors never counts.
pub fn at_outer_edge(displays: &[Rect], edge: Edge, x: i32, y: i32) -> bool {
    let (sx, sy) = edge.step();
    displays.iter().any(|d| d.contains(x, y)) && !displays.iter().any(|d| d.contains(x + sx, y + sy))
}

/// How far along `edge` the point is, across the span of all displays, as 0 to 65535.
pub fn pos_along(displays: &[Rect], edge: Edge, x: i32, y: i32) -> u16 {
    let Some((start, len)) = span(displays, edge) else { return 0 };
    let v = if edge.vertical() { y } else { x };
    ((v - start).clamp(0, len - 1) as i64 * 65535 / (len - 1).max(1) as i64) as u16
}

/// Where the pointer appears when it comes in through `edge` at `pos`: on the outermost display
/// at that height (or width), or the nearest display when monitors are staggered and none is.
pub fn entry_point(displays: &[Rect], edge: Edge, pos: u16) -> Option<(i32, i32)> {
    let (start, len) = span(displays, edge)?;
    let v = start + ((pos as i64 * (len - 1) as i64 + 32767) / 65535) as i32;
    let (sx, sy) = edge.step();
    let d = displays.iter().min_by_key(|d| {
        let (lo, n, depth) = if edge.vertical() { (d.y, d.h, d.x * sx + (d.w - 1) * sx.max(0)) } else { (d.x, d.w, d.y * sy + (d.h - 1) * sy.max(0)) };
        let gap = if v < lo { lo - v } else { (v - (lo + n - 1)).max(0) };
        (gap, -depth)
    })?;
    Some(match edge {
        Edge::Left => (d.x, v.clamp(d.y, d.y + d.h - 1)),
        Edge::Right => (d.x + d.w - 1, v.clamp(d.y, d.y + d.h - 1)),
        Edge::Top => (v.clamp(d.x, d.x + d.w - 1), d.y),
        Edge::Bottom => (v.clamp(d.x, d.x + d.w - 1), d.y + d.h - 1),
    })
}

/// Start and length of all displays along `edge`.
fn span(displays: &[Rect], edge: Edge) -> Option<(i32, i32)> {
    let lo = displays.iter().map(|d| if edge.vertical() { d.y } else { d.x }).min()?;
    let hi = displays.iter().map(|d| if edge.vertical() { d.y + d.h } else { d.x + d.w }).max()?;
    Some((lo, hi - lo))
}

/// Double-tap a key to switch. Fires on the second release within `window_ms` of the first,
/// and only for clean taps, so Ctrl+C never counts as a tap of Ctrl.
pub struct DoubleTap {
    pub hid: u16,
    pub window_ms: u64,
    held: bool,
    dirty: bool,
    last_tap: Option<u64>,
}

impl DoubleTap {
    pub fn new(hid: u16, window_ms: u64) -> DoubleTap {
        DoubleTap { hid, window_ms, held: false, dirty: false, last_tap: None }
    }

    /// Feed every key event. Returns true when the switch should happen.
    pub fn key(&mut self, hid: u16, down: bool, now_ms: u64) -> bool {
        if hid != self.hid {
            self.dirty |= self.held;
            self.last_tap = None;
            return false;
        }
        if down {
            // Auto-repeat sends more downs while held. They change nothing.
            if !self.held {
                self.held = true;
                self.dirty = false;
            }
            return false;
        }
        self.held = false;
        if self.dirty {
            self.last_tap = None;
            return false;
        }
        match self.last_tap.take() {
            Some(t) if now_ms.saturating_sub(t) <= self.window_ms => true,
            _ => {
                self.last_tap = Some(now_ms);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
    const B: Rect = Rect { x: 1920, y: 300, w: 1280, h: 720 };

    #[test]
    fn edges_between_displays_do_not_count() {
        assert!(!at_outer_edge(&[A, B], Edge::Right, 1919, 500));
        assert!(at_outer_edge(&[A, B], Edge::Right, 1919, 100)); // above B, nothing beyond
        assert!(at_outer_edge(&[A, B], Edge::Right, 3199, 500));
        assert!(!at_outer_edge(&[A, B], Edge::Right, 3000, 500)); // not at an edge at all
    }

    #[test]
    fn push_builds_then_crosses() {
        let mut c = Crossing::new(Edge::Right, 120);
        assert_eq!(c.motion(&[A], 1919, 540, 60, 0, false), Push::Building(0.5));
        assert_eq!(c.motion(&[A], 1919, 540, 0, 5, false), Push::Building(0.5)); // sliding along keeps it
        assert_eq!(c.motion(&[A], 1919, 540, -1, 0, false), Push::None); // backing off resets
        assert_eq!(c.motion(&[A], 1919, 540, 60, 0, false), Push::Building(0.5));
        assert_eq!(c.motion(&[A], 1919, 540, 60, 0, false), Push::Cross(pos_along(&[A], Edge::Right, 0, 540)));
        assert_eq!(c.motion(&[A], 1919, 540, 500, 0, true), Push::None); // a drag never crosses
    }

    #[test]
    fn lands_at_the_same_fraction() {
        let pos = pos_along(&[A], Edge::Right, 1919, 540);
        let big = Rect { x: 0, y: 0, w: 2560, h: 1440 };
        let (x, y) = entry_point(&[big], Edge::Left, pos).unwrap();
        assert_eq!(x, 0);
        assert!((719..=721).contains(&y), "{y}");
        assert_eq!(entry_point(&[A], Edge::Left, 0), Some((0, 0)));
        assert_eq!(entry_point(&[A], Edge::Left, 65535), Some((0, 1079)));
        // Entering the right edge high up lands on A, the only display at that height.
        assert_eq!(entry_point(&[A, B], Edge::Right, 0), Some((1919, 0)));
        // Lower down B sticks out further, so it takes the pointer.
        assert_eq!(entry_point(&[A, B], Edge::Right, pos_along(&[A, B], Edge::Right, 0, 500)), Some((3199, 500)));
        assert_eq!(entry_point(&[A, B], Edge::Right, 65535), Some((1919, 1079)));
        assert_eq!(entry_point(&[], Edge::Left, 0), None);
    }

    #[test]
    fn double_tap() {
        let mut t = DoubleTap::new(0xE4, 300);
        assert!(!t.key(0xE4, true, 0) && !t.key(0xE4, false, 50));
        assert!(!t.key(0xE4, true, 100) && !t.key(0xE4, true, 130)); // repeat
        assert!(t.key(0xE4, false, 150));
        // Too slow.
        assert!(!t.key(0xE4, true, 1000) && !t.key(0xE4, false, 1050));
        assert!(!t.key(0xE4, true, 1500) && !t.key(0xE4, false, 1550));
        // Ctrl+C in between is a shortcut, not a tap.
        let mut t = DoubleTap::new(0xE4, 300);
        assert!(!t.key(0xE4, true, 0) && !t.key(0x06, true, 20) && !t.key(0x06, false, 30) && !t.key(0xE4, false, 40));
        assert!(!t.key(0xE4, true, 100) && !t.key(0xE4, false, 150));
    }
}
