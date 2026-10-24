//! Images for the model: prepared once, stored by content, sent byte for
//! byte the same on every request (so the prompt cache keeps them cheap).
//!
//! The rules follow what Codex, pi, opencode and Claude Code converged on
//! (see the smash `docs/images.md` survey):
//! - an image that already fits is sent untouched;
//! - otherwise it is turned upright (EXIF), shrunk to MAX_SIDE with Lanczos3,
//!   kept in its format (PNG for GIF and WebP), and stepped down through JPEG
//!   qualities and smaller sizes until it is under MAX_BYTES;
//! - anything unreadable becomes an error string, which callers turn into a
//!   text placeholder: a bad image never fails a turn.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Content, Message, Role};

/// Longest side sent to a model (the limit every major harness uses).
pub const MAX_SIDE: u32 = 2000;
/// Largest prepared image, in bytes (before base64).
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
/// Largest source file looked at all.
pub const MAX_SOURCE: usize = 50 * 1024 * 1024;
/// Images per request, and their total bytes (base64 adds a third; providers
/// cap requests at about 32 MB).
pub const MAX_IMAGES: usize = 40;
pub const MAX_REQUEST_BYTES: usize = 20 * 1024 * 1024;

/// A prepared image. Messages carry this (a path), never the bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Image {
    /// The prepared file in the image store: what is sent.
    pub path: PathBuf,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// Size before shrinking, when it was shrunk (lets the model map what it
    /// sees back to real pixels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<(u32, u32)>,
    /// Where it came from: a file path, `clipboard`.
    pub name: String,
    /// For images that came from memory (the clipboard): the full-quality
    /// original, saved in the store so the model can open, convert, crop or
    /// re-read it. Images from files have their source path as `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

impl Image {
    /// Estimated prompt tokens (Anthropic's width × height / 750).
    pub fn tokens(&self) -> u64 {
        (u64::from(self.width) * u64::from(self.height) / 750).max(1)
    }

    /// `[Image #2] shot.png · 2000×837 (original 3440×1440)`
    pub fn label(&self, number: usize) -> String {
        let original = self
            .original
            .map(|(w, h)| format!(" (original {w}×{h})"))
            .unwrap_or_default();
        let file = self
            .file
            .as_ref()
            .map(|file| format!(" · saved as {}", file.display()))
            .unwrap_or_default();
        format!(
            "[Image #{number}] {} · {}×{}{original}{file}",
            self.name, self.width, self.height
        )
    }

    /// What stands in for the image when it can't be sent.
    pub fn placeholder(&self, number: usize, why: &str) -> String {
        format!(
            "{} was removed: {why}. read('{}') shows it again.",
            self.label(number),
            self.path.display()
        )
    }

    /// The prepared bytes, base64-encoded for a request.
    pub fn base64(&self) -> std::io::Result<String> {
        let bytes = std::fs::read(&self.path)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

/// Prepare the image file at `path`; the error says why it can't be used.
pub fn prepare_file(path: &Path, store: &Path) -> Result<Image, String> {
    let size = std::fs::metadata(path)
        .map_err(|err| format!("cannot open it: {err}"))?
        .len();
    if size > MAX_SOURCE as u64 {
        return Err(format!(
            "it is {} MB; images over {} MB are not read",
            size / (1024 * 1024),
            MAX_SOURCE / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|err| format!("cannot read it: {err}"))?;
    prepare(&bytes, &path.display().to_string(), store)
}

/// Prepare image `bytes` (named `name` in labels) and store the result in
/// `store`. Same input, same file: the store is content-addressed.
pub fn prepare(bytes: &[u8], name: &str, store: &Path) -> Result<Image, String> {
    if bytes.is_empty() {
        return Err("the file is empty".into());
    }
    if bytes.len() > MAX_SOURCE {
        return Err(format!("it is over {} MB", MAX_SOURCE / (1024 * 1024)));
    }
    let format = image::guess_format(bytes)
        .map_err(|_| "it is not an image smash can read (PNG, JPEG, GIF, WebP)".to_string())?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(format!(
            "{} images are not supported; convert it to PNG first",
            format.to_mime_type()
        ));
    }
    let decode = |err: image::ImageError| format!("cannot decode it: {err}");
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .map_err(decode)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut picture = DynamicImage::from_decoder(decoder).map_err(decode)?;
    let (width, height) = picture.dimensions();
    if width == 0 || height == 0 {
        return Err("it has no pixels".into());
    }
    // GIFs are always re-encoded: some providers refuse animated ones.
    let fits = width.max(height) <= MAX_SIDE
        && bytes.len() <= MAX_BYTES
        && orientation == Orientation::NoTransforms
        && format != ImageFormat::Gif;
    let (data, mime, out_w, out_h) = if fits {
        (bytes.to_vec(), format.to_mime_type(), width, height)
    } else {
        picture.apply_orientation(orientation);
        shrink(&picture, format)?
    };
    let (upright_w, upright_h) = picture.dimensions();
    let original = ((out_w, out_h) != (upright_w, upright_h)).then_some((upright_w, upright_h));
    let path = store_bytes(&data, mime, store)?;
    Ok(Image {
        path,
        mime: mime.to_string(),
        width: out_w,
        height: out_h,
        original,
        name: name.to_string(),
        file: None,
    })
}

/// [`prepare`] for bytes that exist nowhere else (a pasted image): the
/// original is kept in the store too, and the label names it, so the model
/// can work with the file like any other.
pub fn prepare_kept(bytes: &[u8], name: &str, store: &Path) -> Result<Image, String> {
    let mut image = prepare(bytes, name, store)?;
    let original = if std::fs::read(&image.path).is_ok_and(|sent| sent == bytes) {
        image.path.clone()
    } else {
        let mime = image::guess_format(bytes).map_or("image/png", |format| format.to_mime_type());
        store_bytes(bytes, mime, store)?
    };
    image.file = Some(original);
    Ok(image)
}

/// Smallest change that gets `picture` under MAX_SIDE and MAX_BYTES.
fn shrink(
    picture: &DynamicImage,
    format: ImageFormat,
) -> Result<(Vec<u8>, &'static str, u32, u32), String> {
    let (width, height) = picture.dimensions();
    let mut side = width.max(height).min(MAX_SIDE);
    let mut smallest: Option<(Vec<u8>, &'static str, u32, u32)> = None;
    loop {
        let scaled = if width.max(height) > side {
            picture.resize(side, side, FilterType::Lanczos3)
        } else {
            picture.clone()
        };
        let (w, h) = scaled.dimensions();
        // Photos stay JPEG; screenshots (PNG) try lossless first so text
        // stays sharp.
        let mut tries: Vec<Option<u8>> = if format == ImageFormat::Jpeg {
            vec![Some(90)]
        } else {
            vec![None, Some(90)]
        };
        tries.extend([Some(80), Some(70), Some(60)]);
        for quality in tries {
            let (data, mime) = encode(&scaled, quality)?;
            if data.len() <= MAX_BYTES {
                return Ok((data, mime, w, h));
            }
            if smallest
                .as_ref()
                .is_none_or(|best| data.len() < best.0.len())
            {
                smallest = Some((data, mime, w, h));
            }
        }
        if side <= 256 {
            return smallest.ok_or_else(|| "cannot make it small enough".to_string());
        }
        side = side * 3 / 4;
    }
}

/// PNG when `quality` is None, else JPEG at that quality.
fn encode(picture: &DynamicImage, quality: Option<u8>) -> Result<(Vec<u8>, &'static str), String> {
    let mut out = Vec::new();
    let result = match quality {
        None => picture
            .write_with_encoder(PngEncoder::new(&mut out))
            .map(|()| "image/png"),
        // ponytail: alpha is dropped, not composited; fine for screenshots.
        Some(quality) => DynamicImage::ImageRgb8(picture.to_rgb8())
            .write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))
            .map(|()| "image/jpeg"),
    };
    result
        .map(|mime| (out, mime))
        .map_err(|err| format!("cannot encode it: {err}"))
}

/// Write `data` to `store/<sha256>.<ext>` (once) and return the path.
fn store_bytes(data: &[u8], mime: &str, store: &Path) -> Result<PathBuf, String> {
    let digest = Sha256::digest(data);
    let hex: String = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "img",
    };
    let path = store.join(format!("{hex}.{ext}"));
    if !path.exists() {
        std::fs::create_dir_all(store).map_err(|err| format!("cannot save it: {err}"))?;
        std::fs::write(&path, data).map_err(|err| format!("cannot save it: {err}"))?;
    }
    Ok(path)
}

/// Images in `messages`, oldest first, with their number.
pub fn numbered(messages: &[Message]) -> impl Iterator<Item = (usize, &Image)> {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            Content::Image(image) => Some(image),
            _ => None,
        })
        .enumerate()
        .map(|(index, image)| (index + 1, image))
}

/// Replace images with their placeholder text, oldest first, until at most
/// `keep` images (and MAX_REQUEST_BYTES) remain; `keep = 0` replaces all.
/// Done in place, all at once: one cache break, then stable again.
/// Returns how many were replaced.
pub fn drop_oldest(messages: &mut [Message], keep: usize, why: &str) -> usize {
    let sizes: Vec<u64> = numbered(messages)
        .map(|(_, image)| std::fs::metadata(&image.path).map_or(0, |meta| meta.len()))
        .collect();
    let mut total: u64 = sizes.iter().sum();
    let mut excess = sizes.len().saturating_sub(keep);
    let mut drop = 0;
    for size in &sizes {
        if excess == 0 && total <= MAX_REQUEST_BYTES as u64 {
            break;
        }
        excess = excess.saturating_sub(1);
        total = total.saturating_sub(*size);
        drop += 1;
    }
    let mut number = 0;
    for content in messages
        .iter_mut()
        .flat_map(|message| message.content.iter_mut())
    {
        if let Content::Image(image) = content {
            number += 1;
            if number <= drop {
                *content = Content::Text {
                    text: image.placeholder(number, why),
                };
            }
        }
    }
    drop
}

/// Keep every request under MAX_IMAGES and MAX_REQUEST_BYTES.
pub fn fit(messages: &mut [Message]) -> usize {
    drop_oldest(messages, MAX_IMAGES, "too many images for one request")
}

/// A user message with text and images.
pub fn user_message(text: &str, images: Vec<Image>) -> Message {
    let mut content = vec![Content::Text {
        text: text.to_string(),
    }];
    content.extend(images.into_iter().map(Content::Image));
    Message {
        role: Role::User,
        content,
        tool_call_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn store() -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("smash-images-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let picture = RgbImage::from_fn(width, height, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(picture)
            .write_with_encoder(PngEncoder::new(&mut out))
            .unwrap();
        out
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn an_image_that_fits_is_sent_untouched_and_stored_once() {
        let dir = store();
        let bytes = png(640, 480);
        let image = prepare(&bytes, "shot.png", &dir).unwrap();
        assert_eq!(std::fs::read(&image.path).unwrap(), bytes, "byte for byte");
        assert_eq!(
            (image.width, image.height, image.original),
            (640, 480, None)
        );
        assert_eq!(image.mime, "image/png");
        let again = prepare(&bytes, "copy.png", &dir).unwrap();
        assert_eq!(again.path, image.path, "content-addressed");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(image.label(3), "[Image #3] shot.png · 640×480");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pasted_images_keep_their_original_and_name_it() {
        let dir = store();
        let small = png(64, 48);
        let kept = prepare_kept(&small, "clipboard", &dir).unwrap();
        assert_eq!(
            kept.file.as_ref(),
            Some(&kept.path),
            "fits: one file serves both"
        );
        assert!(
            kept.label(1)
                .ends_with(&format!("· saved as {}", kept.path.display()))
        );
        let big = png(3000, 1000);
        let kept = prepare_kept(&big, "clipboard", &dir).unwrap();
        let file = kept.file.clone().unwrap();
        assert_ne!(
            file, kept.path,
            "shrunk copy is sent, original kept beside it"
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            big,
            "full quality, byte for byte"
        );
        assert_eq!(::image::open(&file).unwrap().dimensions(), (3000, 1000));
        let from_file = prepare(&small, "/tmp/x.png", &dir).unwrap();
        assert!(
            from_file.file.is_none() && !from_file.label(1).contains("saved as"),
            "files are named by their path"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_big_screenshot_shrinks_and_keeps_its_format() {
        let dir = store();
        let image = prepare(&png(3440, 1440), "wide.png", &dir).unwrap();
        assert_eq!((image.width, image.height), (2000, 837));
        assert_eq!(image.original, Some((3440, 1440)));
        assert_eq!(image.mime, "image/png");
        assert!(image.label(1).ends_with("2000×837 (original 3440×1440)"));
        let stored = image::open(&image.path).unwrap();
        assert_eq!(stored.dimensions(), (2000, 837));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn noise_too_big_for_png_steps_down_to_jpeg_under_the_cap() {
        let dir = store();
        let mut seed = 7u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let picture = RgbaImage::from_fn(1900, 1900, |_, _| {
            let v = next();
            Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255])
        });
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(picture)
            .write_with_encoder(PngEncoder::new(&mut bytes))
            .unwrap();
        assert!(bytes.len() > MAX_BYTES, "the fixture is over the cap");
        let image = prepare(&bytes, "noise.png", &dir).unwrap();
        let size = std::fs::metadata(&image.path).unwrap().len() as usize;
        assert!(size <= MAX_BYTES, "{size}");
        assert_eq!(image.mime, "image/jpeg");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A 40×20 JPEG whose EXIF says "rotate 90° clockwise" (orientation 6).
    fn rotated_jpeg() -> Vec<u8> {
        let mut plain = Vec::new();
        DynamicImage::ImageRgb8(RgbImage::from_pixel(40, 20, Rgb([200, 40, 40])))
            .write_with_encoder(JpegEncoder::new_with_quality(&mut plain, 90))
            .unwrap();
        // APP1 "Exif\0\0" + a little-endian TIFF with one IFD entry:
        // tag 0x0112 (Orientation), type SHORT, count 1, value 6.
        let tiff: [u8; 26] = [
            b'I', b'I', 42, 0, 8, 0, 0, 0, // header, IFD at offset 8
            1, 0, // one entry
            0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, // orientation = 6
            0, 0, 0, 0, // no next IFD
        ];
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let length = (app1.len() + 2) as u16;
        let mut out = plain[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn exif_orientation_is_applied_and_cmyk_jpegs_decode() {
        let dir = store();
        // 40×20 pixels, EXIF says "rotate 90°": the model must see 20×40.
        let rotated = prepare(&rotated_jpeg(), "rotated.jpg", &dir).unwrap();
        assert_eq!((rotated.width, rotated.height), (20, 40));
        assert_eq!(rotated.mime, "image/jpeg");
        let cmyk = prepare(&fixture("cmyk.jpg"), "cmyk.jpg", &dir).unwrap();
        assert_eq!((cmyk.width, cmyk.height), (16, 16));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_input_explains_itself() {
        let dir = store();
        let err = |bytes: &[u8]| prepare(bytes, "x", &dir).unwrap_err();
        assert!(err(b"").contains("empty"));
        assert!(err(b"<svg xmlns='http://www.w3.org/2000/svg'/>").contains("not an image"));
        assert!(
            err(&png(10, 10)[..40]).contains("cannot decode"),
            "truncated"
        );
        let mut bmp = Vec::new();
        DynamicImage::ImageRgb8(RgbImage::new(4, 4))
            .write_to(&mut Cursor::new(&mut bmp), ImageFormat::Bmp)
            .ok();
        if !bmp.is_empty() {
            assert!(err(&bmp).contains("not supported") || err(&bmp).contains("not an image"));
        }
        let missing = prepare_file(Path::new("/nonexistent/x.png"), &dir).unwrap_err();
        assert!(missing.contains("cannot open"));
        assert!(!dir.exists(), "nothing stored for bad input");
    }

    #[test]
    fn gifs_become_png() {
        let dir = store();
        let mut gif = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::new(8, 8))
            .write_to(&mut Cursor::new(&mut gif), ImageFormat::Gif)
            .unwrap();
        let image = prepare(&gif, "a.gif", &dir).unwrap();
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.original, None, "same size, only re-encoded");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_oldest_images_become_placeholders_all_at_once() {
        let dir = store();
        let image = |n: u32| prepare(&png(10 + n, 10), &format!("i{n}.png"), &dir).unwrap();
        let mut messages = vec![
            user_message("a", vec![image(1), image(2)]),
            Message::assistant(vec![Content::Text { text: "ok".into() }]),
            user_message("b", vec![image(3)]),
        ];
        assert_eq!(
            numbered(&messages).map(|(n, _)| n).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(drop_oldest(&mut messages, 1, "too many"), 2);
        let first = &messages[0].content;
        assert!(
            matches!(&first[1], Content::Text { text } if text.starts_with("[Image #1] i1.png") && text.contains("too many") && text.contains("read("))
        );
        assert!(matches!(&first[2], Content::Text { text } if text.starts_with("[Image #2]")));
        assert!(
            matches!(&messages[2].content[1], Content::Image(_)),
            "the newest stays"
        );
        assert_eq!(
            drop_oldest(&mut messages, 1, "too many"),
            0,
            "stable afterwards"
        );
        assert_eq!(fit(&mut messages), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
