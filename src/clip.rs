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
                log!("clipboard unavailable: {e}");
                None
            }
        }
    }

    /// The clipboard as a message, unless it is what last crossed. Text wins over an image,
    /// because editors also offer a picture of copied text.
    pub fn take(&mut self) -> Option<Msg> {
        // Compared before an image is encoded: a big PNG takes long enough to delay the switch.
        let (hash, msg): (u64, Box<dyn FnOnce() -> Option<Msg>>) = match self.board.get_text() {
            Ok(text) if !text.is_empty() && text.len() <= TEXT_MAX => (text_hash(&text), Box::new(|| Some(Msg::ClipText(text)))),
            _ => {
                let img = self.board.get_image().ok()?;
                (image_hash(&img), Box::new(move || encode(&img).filter(|png| png.len() <= PNG_MAX).map(Msg::ClipPng)))
            }
        };
        if hash == self.last {
            return None;
        }
        self.last = hash;
        msg()
    }

    pub fn put(&mut self, msg: &Msg) {
        let result = match msg {
            Msg::ClipText(text) => {
                self.last = text_hash(text);
                self.board.set_text(text.as_str())
            }
            Msg::ClipPng(png) => match decode(png) {
                // Remembered as the image itself, so reading it back here is not sent back.
                Some(img) => {
                    self.last = image_hash(&img);
                    self.board.set_image(img)
                }
                None => return log!("clipboard: the peer sent an image that is not a PNG"),
            },
            _ => return,
        };
        if let Err(e) = result {
            log!("clipboard: {e}");
        }
    }
}

fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    ("text", text).hash(&mut h);
    h.finish()
}

fn image_hash(img: &ImageData) -> u64 {
    let mut h = DefaultHasher::new();
    ("image", img.width, img.height, &img.bytes[..]).hash(&mut h);
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
