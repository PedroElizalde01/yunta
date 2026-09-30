//! Messages carried over the link: a tag byte, then big-endian fields.

use crate::crossing::Edge;

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// Keeps the link alive. Either side sends one when it has been quiet for a second.
    Ping,
    /// Raw relative pointer motion.
    Move { dx: i16, dy: i16 },
    /// 1 left, 2 right, 3 middle, 4 back, 5 forward.
    Button { button: u8, down: bool },
    /// 120 per wheel notch, as on Windows, so smooth scrolling fits between notches.
    Scroll { dx: i16, dy: i16 },
    /// A USB HID keyboard usage (page 7), so every machine maps it to its own codes by position.
    Key { hid: u16, down: bool },
    /// Input moves to the receiver. The pointer appears on `edge` of its screens, `pos` of the
    /// way along (0 to 65535).
    Enter { edge: Edge, pos: u16 },
    /// The receiver's pointer went back out through the return edge, `pos` of the way along.
    Leave { pos: u16 },
    ClipText(String),
    ClipPng(Vec<u8>),
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
        assert_eq!(Msg::decode(&[99]), None);
    }
}
