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

    /// As written in yunta.conf.
    pub fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        }
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
    /// `x y w h` per display, `;` between them, as in the status file.
    pub fn list_text(rects: &[Rect]) -> String {
        rects.iter().map(|r| format!("{} {} {} {}", r.x, r.y, r.w, r.h)).collect::<Vec<_>>().join("; ")
    }

    pub fn parse_list(text: &str) -> Vec<Rect> {
        text.split(';')
            .filter_map(|r| match r.split_whitespace().map(str::parse).collect::<Result<Vec<i32>, _>>().ok()?[..] {
                [x, y, w, h] if w > 0 && h > 0 => Some(Rect { x, y, w, h }),
                _ => None,
            })
            .collect()
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// How the two machines sit side by side: our `edge` leads to the peer, and the stretch `ours`
/// of it (in 0..65535 of our span along that edge) faces the stretch `theirs` of the peer's.
/// Fractions, so screen sizes and scaling on either side do not matter. `stamp` is when it was
/// last changed, in Unix milliseconds; the newer layout wins when the two machines meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub edge: Edge,
    pub ours: (u16, u16),
    pub theirs: (u16, u16),
    pub stamp: u64,
    /// The two touch at a corner only: `ours` and `theirs` are each one end of the edge, and
    /// crossing takes a diagonal push into that corner.
    pub corner: bool,
}

/// How near the corner the pointer must be to cross there, in pixels.
pub const CORNER_BOX: i32 = 8;

impl Default for Layout {
    fn default() -> Layout {
        Layout { edge: Edge::Right, ours: (0, u16::MAX), theirs: (0, u16::MAX), stamp: 0, corner: false }
    }
}

impl Layout {
    /// The same arrangement seen from the peer.
    pub fn mirror(self) -> Layout {
        Layout { edge: self.edge.opposite(), ours: self.theirs, theirs: self.ours, ..self }
    }

    /// True where our edge faces the peer. At a corner, that is only the corner itself, and
    /// `corner_push` decides.
    pub fn covers(self, pos: u16) -> bool {
        self.corner || (self.ours.0..=self.ours.1).contains(&pos)
    }

    /// For a corner: the pointer at (x, y) is in the corner and the move (dx, dy) heads into it
    /// along the edge as well as out through it.
    pub fn corner_push(self, displays: &[Rect], x: i32, y: i32, dx: i32, dy: i32) -> bool {
        let Some((start, len)) = span(displays, self.edge) else { return false };
        let (v, dv) = if self.edge.vertical() { (y, dy) } else { (x, dx) };
        let at_start = self.ours.0 < u16::MAX / 2;
        let target = if at_start { start } else { start + len - 1 };
        (v - target).abs() <= CORNER_BOX && if at_start { dv < 0 } else { dv > 0 }
    }

    /// A position along our edge as the matching position along the peer's, which is what
    /// `Enter` and `Leave` carry. Outside the facing stretch it goes to the nearest end.
    pub fn to_peer(self, pos: u16) -> u16 {
        let ((a0, a1), (b0, b1)) = (self.ours, self.theirs);
        let along = pos.clamp(a0, a1) - a0;
        let span = u32::from(a1 - a0).max(1);
        (u32::from(b0) + (u32::from(along) * u32::from(b1 - b0) + span / 2) / span) as u16
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

    /// Lets go of any push built up so far.
    pub fn reset(&mut self) {
        self.pressure = 0;
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
        let (lo, n, depth) =
            if edge.vertical() { (d.y, d.h, d.x * sx + (d.w - 1) * sx.max(0)) } else { (d.x, d.w, d.y * sy + (d.h - 1) * sy.max(0)) };
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

/// Moves the pointer from (x, y) to (nx, ny), or as near as the displays allow.
pub fn step_within(displays: &[Rect], x: i32, y: i32, nx: i32, ny: i32) -> (i32, i32) {
    if displays.iter().any(|d| d.contains(nx, ny)) {
        return (nx, ny);
    }
    match displays.iter().find(|d| d.contains(x, y)).or(displays.first()) {
        Some(d) => (nx.clamp(d.x, d.x + d.w - 1), ny.clamp(d.y, d.y + d.h - 1)),
        None => (x, y),
    }
}

/// Start and length of all displays along `edge`.
fn span(displays: &[Rect], edge: Edge) -> Option<(i32, i32)> {
    let lo = displays.iter().map(|d| if edge.vertical() { d.y } else { d.x }).min()?;
    let hi = displays.iter().map(|d| if edge.vertical() { d.y + d.h } else { d.x + d.w }).max()?;
    Some((lo, hi - lo))
}

/// How long the trigger key is held, alone, to switch in hold mode.
pub const HOLD_MS: u64 = 400;

/// The trigger key: switches on a double-tap, or in hold mode when held alone for `HOLD_MS`.
/// Only clean presses count either way, so Ctrl+C is never a tap or a hold of Ctrl.
pub struct Trigger {
    pub hid: u16,
    pub window_ms: u64,
    pub hold: bool,
    held: bool,
    dirty: bool,
    last_tap: Option<u64>,
    down_at: u64,
    fired: bool,
}

impl Trigger {
    pub fn new(hid: u16, window_ms: u64, hold: bool) -> Trigger {
        Trigger { hid, window_ms, hold, held: false, dirty: false, last_tap: None, down_at: 0, fired: false }
    }

    /// Feed every key event. True when the switch should happen (double-tap mode only; a hold
    /// fires from `tick`).
    pub fn key(&mut self, hid: u16, down: bool, now_ms: u64) -> bool {
        if hid != self.hid {
            self.dirty |= self.held;
            self.last_tap = None;
            return false;
        }
        if down {
            // Auto-repeat sends more downs while held. They change nothing.
            if !self.held {
                (self.held, self.dirty, self.down_at, self.fired) = (true, false, now_ms, false);
            }
            return false;
        }
        self.held = false;
        if self.dirty || self.hold {
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

    /// When a hold would switch, if the key is being held for one.
    pub fn deadline(&self) -> Option<u64> {
        (self.hold && self.held && !self.dirty && !self.fired).then_some(self.down_at + HOLD_MS)
    }

    /// True once, when a hold has lasted long enough.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if self.deadline().is_some_and(|d| now_ms >= d) {
            self.fired = true;
            return true;
        }
        false
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
    fn layout_maps_the_facing_stretch() {
        // Our upper half faces the peer's whole edge.
        let l = Layout { edge: Edge::Left, ours: (0, 32767), theirs: (0, 65535), stamp: 5, corner: false };
        assert!(l.covers(0) && l.covers(32767) && !l.covers(40000));
        assert_eq!((l.to_peer(0), l.to_peer(16384), l.to_peer(32767)), (0, 32769, 65535));
        assert_eq!(l.to_peer(60000), 65535); // a shortcut switch from below lands at the end
        let m = l.mirror();
        assert_eq!(m, Layout { edge: Edge::Right, ours: (0, 65535), theirs: (0, 32767), stamp: 5, corner: false });
        assert_eq!(m.to_peer(l.to_peer(16384)), 16384);
        assert_eq!(Layout::default().to_peer(12345), 12345);
        let point = Layout { ours: (100, 100), ..l };
        assert_eq!(point.to_peer(100), 0);
    }

    #[test]
    fn corners_take_a_diagonal_push() {
        // The top right corner of A, the peer up and to the right.
        let l = Layout { edge: Edge::Right, ours: (0, 0), theirs: (65535, 65535), stamp: 0, corner: true };
        assert!(l.corner_push(&[A], 1919, 3, 5, -2));
        assert!(!l.corner_push(&[A], 1919, 3, 5, 0)); // straight out is not into the corner
        assert!(!l.corner_push(&[A], 1919, 40, 5, -2)); // too far down the edge
        assert_eq!(l.to_peer(0), 65535); // lands in the peer's bottom left
        assert!(l.mirror().corner);
    }

    #[test]
    fn rect_lists_round_trip() {
        assert_eq!(Rect::parse_list(&Rect::list_text(&[A, B])), vec![A, B]);
        assert_eq!(Rect::parse_list(""), vec![]);
        assert_eq!(Rect::parse_list("1 2 3; 0 0 10 10; 0 0 0 5"), vec![Rect { x: 0, y: 0, w: 10, h: 10 }]);
    }

    #[test]
    fn steps_stay_on_screen() {
        assert_eq!(step_within(&[A, B], 1900, 500, 1950, 510), (1950, 510)); // onto B
        assert_eq!(step_within(&[A, B], 1900, 100, 1950, 110), (1919, 110)); // nothing beside A up here
        assert_eq!(step_within(&[A], 10, 10, -5, -5), (0, 0));
    }

    #[test]
    fn hold() {
        let mut t = Trigger::new(0xE4, 300, true);
        assert!(!t.key(0xE4, true, 0));
        assert_eq!(t.deadline(), Some(HOLD_MS));
        assert!(!t.tick(HOLD_MS - 1) && t.tick(HOLD_MS));
        assert!(!t.tick(HOLD_MS + 50)); // once per hold
        assert!(!t.key(0xE4, false, 900));
        // Ctrl+C held long is not a hold.
        assert!(!t.key(0xE4, true, 1000) && !t.key(0x06, true, 1100));
        assert!(!t.tick(2000));
        // In hold mode a double-tap does nothing.
        let mut t = Trigger::new(0xE4, 300, true);
        assert!(!t.key(0xE4, true, 0) && !t.key(0xE4, false, 50) && !t.key(0xE4, true, 100) && !t.key(0xE4, false, 150));
    }

    #[test]
    fn double_tap() {
        let mut t = Trigger::new(0xE4, 300, false);
        assert!(!t.key(0xE4, true, 0) && !t.key(0xE4, false, 50));
        assert!(!t.key(0xE4, true, 100) && !t.key(0xE4, true, 130)); // repeat
        assert!(t.key(0xE4, false, 150));
        // Too slow.
        assert!(!t.key(0xE4, true, 1000) && !t.key(0xE4, false, 1050));
        assert!(!t.key(0xE4, true, 1500) && !t.key(0xE4, false, 1550));
        // Ctrl+C in between is a shortcut, not a tap.
        let mut t = Trigger::new(0xE4, 300, false);
        assert!(!t.key(0xE4, true, 0) && !t.key(0x06, true, 20) && !t.key(0x06, false, 30) && !t.key(0xE4, false, 40));
        assert!(!t.key(0xE4, true, 100) && !t.key(0xE4, false, 150));
    }
}
