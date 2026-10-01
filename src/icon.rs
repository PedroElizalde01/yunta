//! Yunta's Yoke mark, with the tray colour showing the link's state.

pub const SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Look {
    Waiting,
    Offline,
    Linked,
    Paused,
}

impl Look {
    pub fn for_link(connected: bool, waiting: bool, paused: bool) -> Self {
        match (connected, waiting, paused) {
            (true, _, true) => Self::Paused,
            (true, _, false) => Self::Linked,
            (false, true, _) => Self::Waiting,
            (false, false, _) => Self::Offline,
        }
    }
}

/// The same exported SVG for window icons and in-app branding.
pub fn image(size: u32) -> image::RgbaImage {
    image::load_from_memory(include_bytes!("../assets/yunta.png"))
        .expect("embedded Yunta icon")
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .into_rgba8()
}

/// SIZE x SIZE pixels as RGBA. Only the colour changes, never the silhouette.
pub fn rgba(look: Look) -> Vec<u8> {
    static MARK: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let mut pixels = MARK.get_or_init(|| image(SIZE as u32).into_raw()).clone();
    let color = match look {
        Look::Offline => [0x9a, 0xa0, 0xa6],
        Look::Linked => [0x3f, 0xb9, 0x50],
        Look::Waiting | Look::Paused => [0xd2, 0x99, 0x22],
    };
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[..3].copy_from_slice(&color);
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yoke_keeps_its_shape_and_changes_colour_with_the_link() {
        for (connected, waiting, paused, look) in [
            (false, false, false, Look::Offline),
            (false, false, true, Look::Offline),
            (false, true, false, Look::Waiting),
            (false, true, true, Look::Waiting),
            (true, false, false, Look::Linked),
            (true, true, false, Look::Linked),
            (true, false, true, Look::Paused),
            (true, true, true, Look::Paused),
        ] {
            assert_eq!(Look::for_link(connected, waiting, paused), look);
        }
        let linked = rgba(Look::Linked);
        let alpha = |pixels: &[u8]| pixels.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>();
        assert_eq!(linked.len(), SIZE * SIZE * 4);
        let at = |x, y| linked[(y * SIZE + x) * 4 + 3];
        // Two arms, an open centre, a joined base and a stem, on a transparent canvas.
        for (x, y, a) in [(0, 0, 0), (7, 6, 255), (25, 6, 255), (16, 6, 0), (16, 18, 255), (16, 25, 255), (7, 25, 0)] {
            assert!(at(x, y).abs_diff(a) <= 8, "wrong Yoke coverage at {x}, {y}");
        }
        assert!(linked.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
        for (look, color) in [
            (Look::Linked, [0x3f, 0xb9, 0x50]),
            (Look::Waiting, [0xd2, 0x99, 0x22]),
            (Look::Offline, [0x9a, 0xa0, 0xa6]),
            (Look::Paused, [0xd2, 0x99, 0x22]),
        ] {
            let pixels = rgba(look);
            assert_eq!(alpha(&pixels), alpha(&linked));
            assert!(pixels.chunks_exact(4).all(|p| p[..3] == color));
        }
    }
}
