//! The clipboard travels with input: whichever machine gives input away sends its clipboard first.

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;

use arboard::{Clipboard, ImageData};
use image::{ImageFormat, RgbaImage};

use crate::msg::Msg;

pub const TEXT_MAX: usize = 256 * 1024;
/// A full-screen screenshot on a big display is a few MB as PNG.
pub const PNG_MAX: usize = 8 * 1024 * 1024;

pub struct Clip {
    board: Clipboard,
    /// Hash of what last crossed either way, so an unchanged clipboard is not sent again.
    last: u64,
}

impl Clip {
    pub fn new() -> Option<Clip> {
        match Clipboard::new() {
            Ok(board) => Some(Clip { board, last: 0 }),
            Err(e) => {
                eprintln!("clipboard unavailable: {e}");
                None
            }
        }
    }

    /// The clipboard as a message, unless it is what last crossed. Text wins over an image,
    /// because editors also offer a picture of copied text.
    pub fn take(&mut self) -> Option<Msg> {
        let msg = match self.board.get_text() {
            Ok(text) if !text.is_empty() && text.len() <= TEXT_MAX => Msg::ClipText(text),
            _ => Msg::ClipPng(self.board.get_image().ok().and_then(|img| encode(&img)).filter(|png| png.len() <= PNG_MAX)?),
        };
        let hash = hash(&msg);
        if hash == self.last {
            return None;
        }
        self.last = hash;
        Some(msg)
    }

    pub fn put(&mut self, msg: &Msg) {
        self.last = hash(msg);
        let result = match msg {
            Msg::ClipText(text) => self.board.set_text(text.as_str()),
            Msg::ClipPng(png) => match decode(png) {
                Some(img) => self.board.set_image(img),
                None => return eprintln!("clipboard: the peer sent an image that is not a PNG"),
            },
            _ => return,
        };
        if let Err(e) = result {
            eprintln!("clipboard: {e}");
        }
    }
}

fn hash(msg: &Msg) -> u64 {
    let mut h = DefaultHasher::new();
    msg.encode().hash(&mut h);
    h.finish()
}

fn encode(img: &ImageData) -> Option<Vec<u8>> {
    let rgba = RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.to_vec())?;
    let mut out = Cursor::new(Vec::new());
    rgba.write_to(&mut out, ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// `image` caps how much a decode may allocate, so a small PNG claiming huge dimensions fails.
fn decode(png: &[u8]) -> Option<ImageData<'static>> {
    let rgba = image::load_from_memory_with_format(png, ImageFormat::Png).ok()?.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(ImageData { width: width as usize, height: height as usize, bytes: Cow::Owned(rgba.into_raw()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        let img = ImageData { width: 2, height: 1, bytes: Cow::Owned(vec![255, 0, 0, 255, 0, 0, 255, 128]) };
        let back = decode(&encode(&img).unwrap()).unwrap();
        assert_eq!((back.width, back.height, back.bytes), (2, 1, img.bytes));
        assert!(decode(b"not a png").is_none());
    }
}
