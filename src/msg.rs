//! Messages carried over the link: a tag byte, then big-endian fields.

use crate::crossing::{Edge, Layout, Rect};

/// More displays than anyone plugs into one machine; caps what a peer can make us hold.
pub const DISPLAYS_MAX: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// Keeps the link alive. Either side sends one when it has been quiet for a second.
    Ping,
    /// Raw relative pointer motion.
    Move {
        dx: i16,
        dy: i16,
    },
    /// 1 left, 2 right, 3 middle, 4 back, 5 forward.
    Button {
        button: u8,
        down: bool,
    },
    /// 120 per wheel notch, as on Windows, so smooth scrolling fits between notches.
    /// Positive `dy` scrolls up (away from you), positive `dx` scrolls right.
    Scroll {
        dx: i16,
        dy: i16,
    },
    /// A USB HID keyboard usage (page 7), so every machine maps it to its own codes by position.
    Key {
        hid: u16,
        down: bool,
    },
    /// Input moves to the receiver. The pointer appears on `edge` of its screens, `pos` of the
    /// way along (0 to 65535).
    Enter {
        edge: Edge,
        pos: u16,
    },
    /// Input goes back to the machine that sent `Enter`. Either side can send it: the driven one
    /// when its pointer goes back out through the edge it came in by, `pos` of the way along.
    Leave {
        pos: u16,
    },
    ClipText(String),
    ClipPng(Vec<u8>),
    /// The sender's displays, for drawing the arrangement. Sent on link up and when they change.
    Displays(Vec<Rect>),
    /// The sender's arrangement, as it sees it. Sent on link up and when it changes.
    Layout(Layout),
    /// Who the sender is and what it allows: its name, and whether its own keyboard and mouse
    /// may be taken over. Sent on link up and when either changes.
    Hello {
        receive: bool,
        name: String,
    },
    /// A touchpad swipe: how many fingers, and which way (0 up, 1 down, 2 left, 3 right).
    Gesture {
        fingers: u8,
        direction: u8,
    },
    /// A focused app fills this part of the sender's screens: the pointer is not to cross into
    /// it at the edge, though the shortcut may still switch. None when there is none. Sent on
    /// link up and whenever it changes.
    Busy(Option<Rect>),
    /// The sender's own mouse moved while the receiver was driving it: the receiver stops
    /// driving and puts its pointer back where it left, quietly.
    TakeBack,
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8);
        match self {
            Msg::Ping => out.push(0),
            Msg::Move { dx, dy } => {
                out.push(1);
                out.extend_from_slice(&dx.to_be_bytes());
                out.extend_from_slice(&dy.to_be_bytes());
            }
            Msg::Button { button, down } => out.extend_from_slice(&[2, *button, *down as u8]),
            Msg::Scroll { dx, dy } => {
                out.push(3);
                out.extend_from_slice(&dx.to_be_bytes());
                out.extend_from_slice(&dy.to_be_bytes());
            }
            Msg::Key { hid, down } => {
                out.push(4);
                out.extend_from_slice(&hid.to_be_bytes());
                out.push(*down as u8);
            }
            Msg::Enter { edge, pos } => {
                out.extend_from_slice(&[5, *edge as u8]);
                out.extend_from_slice(&pos.to_be_bytes());
            }
            Msg::Leave { pos } => {
                out.push(6);
                out.extend_from_slice(&pos.to_be_bytes());
            }
            Msg::ClipText(text) => {
                out.push(7);
                out.extend_from_slice(text.as_bytes());
            }
            Msg::ClipPng(png) => {
                out.push(8);
                out.extend_from_slice(png);
            }
            Msg::Displays(rects) => {
                out.push(9);
                for r in rects.iter().take(DISPLAYS_MAX) {
                    for v in [r.x, r.y, r.w, r.h] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                }
            }
            Msg::Layout(l) => {
                out.extend_from_slice(&[10, l.edge as u8]);
                for v in [l.ours.0, l.ours.1, l.theirs.0, l.theirs.1] {
                    out.extend_from_slice(&v.to_be_bytes());
                }
                out.extend_from_slice(&l.stamp.to_be_bytes());
                out.push(l.corner as u8);
            }
            Msg::Hello { receive, name } => {
                out.extend_from_slice(&[11, *receive as u8]);
                out.extend_from_slice(name.as_bytes());
            }
            Msg::Gesture { fingers, direction } => out.extend_from_slice(&[12, *fingers, *direction]),
            Msg::Busy(busy) => {
                out.push(13);
                if let Some(r) = busy {
                    for v in [r.x, r.y, r.w, r.h] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                }
            }
            Msg::TakeBack => out.push(14),
        }
        out
    }

    /// Rejects unknown tags, bad field values and trailing bytes.
    pub fn decode(bytes: &[u8]) -> Option<Msg> {
        let (&tag, r) = bytes.split_first()?;
        let u16_at = |i: usize| Some(u16::from_be_bytes(r.get(i..i + 2)?.try_into().ok()?));
        let i16_at = |i: usize| u16_at(i).map(|v| v as i16);
        let bool_at = |i: usize| match r.get(i)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let (msg, len) = match tag {
            0 => (Msg::Ping, 0),
            1 => (Msg::Move { dx: i16_at(0)?, dy: i16_at(2)? }, 4),
            2 => (Msg::Button { button: *r.first()?, down: bool_at(1)? }, 2),
            3 => (Msg::Scroll { dx: i16_at(0)?, dy: i16_at(2)? }, 4),
            4 => (Msg::Key { hid: u16_at(0)?, down: bool_at(2)? }, 3),
            5 => (Msg::Enter { edge: Edge::from_u8(*r.first()?)?, pos: u16_at(1)? }, 3),
            6 => (Msg::Leave { pos: u16_at(0)? }, 2),
            7 => (Msg::ClipText(String::from_utf8(r.to_vec()).ok()?), r.len()),
            8 => (Msg::ClipPng(r.to_vec()), r.len()),
            9 if r.len() % 16 == 0 && r.len() / 16 <= DISPLAYS_MAX => {
                let at = |i: usize| i32::from_be_bytes(r[i..i + 4].try_into().unwrap());
                let rects: Vec<Rect> =
                    (0..r.len()).step_by(16).map(|i| Rect { x: at(i), y: at(i + 4), w: at(i + 8), h: at(i + 12) }).collect();
                if !rects.iter().all(Rect::sane) {
                    return None;
                }
                (Msg::Displays(rects), r.len())
            }
            10 => {
                let (ours, theirs) = ((u16_at(1)?, u16_at(3)?), (u16_at(5)?, u16_at(7)?));
                if ours.0 > ours.1 || theirs.0 > theirs.1 {
                    return None;
                }
                let stamp = u64::from_be_bytes(r.get(9..17)?.try_into().ok()?);
                (Msg::Layout(Layout { edge: Edge::from_u8(*r.first()?)?, ours, theirs, stamp, corner: bool_at(17)? }), 18)
            }
            // A name is shown, so it is held to 64 characters and no control characters.
            11 => {
                let name =
                    String::from_utf8(r.get(1..)?.to_vec()).ok().filter(|n| n.chars().count() <= 64 && !n.chars().any(char::is_control))?;
                (Msg::Hello { receive: bool_at(0)?, name }, r.len())
            }
            12 if *r.get(1)? < 4 => (Msg::Gesture { fingers: *r.first()?, direction: r[1] }, 2),
            13 if r.is_empty() => (Msg::Busy(None), 0),
            13 => {
                let at = |i: usize| Some(i32::from_be_bytes(r.get(i..i + 4)?.try_into().ok()?));
                let busy = Rect { x: at(0)?, y: at(4)?, w: at(8)?, h: at(12)? };
                if !busy.sane() {
                    return None;
                }
                (Msg::Busy(Some(busy)), 16)
            }
            14 => (Msg::TakeBack, 0),
            _ => return None,
        };
        (len == r.len()).then_some(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejects() {
        let all = [
            Msg::Ping,
            Msg::Move { dx: -300, dy: 7 },
            Msg::Button { button: 4, down: true },
            Msg::Scroll { dx: 0, dy: -120 },
            Msg::Key { hid: 0xE3, down: false },
            Msg::Enter { edge: Edge::Left, pos: 40000 },
            Msg::Leave { pos: 1 },
            Msg::ClipText("mate ☕".into()),
            Msg::ClipPng(vec![0x89, b'P', b'N', b'G']),
            Msg::Displays(vec![Rect { x: -1920, y: 0, w: 1920, h: 1080 }, Rect { x: 0, y: 0, w: 2560, h: 1440 }]),
            Msg::Displays(vec![]),
            Msg::Layout(Layout { edge: Edge::Bottom, ours: (10, 20), theirs: (0, 65535), stamp: 1_759_000_000_000, corner: false }),
            Msg::Layout(Layout { edge: Edge::Left, ours: (0, 0), theirs: (65535, 65535), stamp: 1, corner: true }),
            Msg::Hello { receive: false, name: "Pedro's laptop".into() },
            Msg::Gesture { fingers: 3, direction: 1 },
            Msg::Busy(None),
            Msg::Busy(Some(Rect { x: -1920, y: 0, w: 1920, h: 1080 })),
            Msg::TakeBack,
        ];
        for m in all {
            assert_eq!(Msg::decode(&m.encode()), Some(m));
        }
        assert_eq!(Msg::decode(&[]), None);
        assert_eq!(Msg::decode(&[0, 0]), None); // trailing byte
        assert_eq!(Msg::decode(&[1, 0, 0]), None); // short
        assert_eq!(Msg::decode(&[2, 1, 2]), None); // bool out of range
        assert_eq!(Msg::decode(&[5, 9, 0, 0]), None); // no such edge
        assert_eq!(Msg::decode(&[7, 0xFF]), None); // not UTF-8
        assert_eq!(Msg::decode(&[9, 0, 0]), None); // not whole rects
        assert_eq!(Msg::decode(&[[9].as_slice(), &[0; 16]].concat()), None); // zero size
        assert_eq!(Msg::decode(&[[9].as_slice(), &[1; 16 * 17]].concat()), None); // too many
        let backwards = Msg::Layout(Layout { ours: (20, 10), ..Layout::default() }).encode();
        assert_eq!(Msg::decode(&backwards), None);
        assert_eq!(Msg::decode(&[11, 1, b'a', 7]), None); // a control character in the name
        assert_eq!(Msg::decode(&[12, 3, 4]), None); // no such direction
        let mut busy = vec![13];
        for v in [0i32, 0, -5, 10] {
            busy.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(Msg::decode(&busy), None); // a full-screen app of negative width
        assert_eq!(Msg::decode(&[99]), None);
    }
}
