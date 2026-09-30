//! The tray icon, drawn in code: two screens side by side, coloured by the link's state.

pub const SIZE: usize = 32;

#[derive(Clone, Copy, PartialEq)]
pub enum Look {
    Waiting,
    Linked,
    Paused,
}

/// SIZE x SIZE pixels as RGBA.
pub fn rgba(look: Look) -> Vec<u8> {
    let color = match look {
        Look::Waiting => [0x9a, 0xa0, 0xa6],
        Look::Linked => [0x3f, 0xb9, 0x50],
        Look::Paused => [0xd2, 0x99, 0x22],
    };
    let mut px = vec![0u8; SIZE * SIZE * 4];
    let mut paint = |x0: usize, y0: usize, x1: usize, y1: usize, alpha: u8| {
        for y in y0..y1 {
            for x in x0..x1 {
                px[(y * SIZE + x) * 4..][..4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
            }
        }
    };
    for left in [1, 17] {
        // A screen: solid frame, translucent glass, and a stand below it.
        paint(left, 6, left + 14, 22, 255);
        paint(left + 2, 8, left + 12, 20, 90);
        paint(left + 6, 22, left + 8, 25, 255);
        paint(left + 3, 25, left + 11, 27, 255);
    }
    px
}
