//! Image previews in the transcript.
//!
//! Every image gets a half-block rendering (`▀`, top pixel as foreground,
//! bottom pixel as background): plain rows, so it scrolls, clips, fades and
//! prints exactly like text in every client. Clients that can draw real
//! images (the window, kitty) lay the picture over those same cells, using
//! the PNG this module also provides.
//!
//! Decoding is slow next to a frame, so previews are made on a background
//! thread and cached. Until one is ready the layout reserves its cells (no
//! reflow when it lands); [`take_ready`] tells the UI to lay out again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, LazyLock, Mutex};

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};

use crate::NIGHT;
use crate::layout::{Paint, Row, run};

/// Tallest preview, in rows; widest, in columns.
pub const MAX_ROWS: usize = 12;
pub const MAX_COLS: usize = 64;
/// A cell is about this many times taller than wide (terminals and the
/// window's 14/20 px grid are all close to it).
const CELL_ASPECT: f64 = 2.2;

/// A ready preview.
#[derive(Debug)]
pub struct Thumb {
    /// Exactly the requested rows × cols of half-block cells.
    pub rows: Vec<Row>,
    /// The image as PNG (itself when it already is one), for real drawing.
    pub png: PathBuf,
}

type Key = (PathBuf, usize, usize);

/// Ready previews; a key present with `None` is being made or failed.
static CACHE: LazyLock<Mutex<HashMap<Key, Option<Arc<Thumb>>>>> = LazyLock::new(Mutex::default);
static READY: AtomicBool = AtomicBool::new(false);
static JOBS: LazyLock<Mutex<Sender<Key>>> = LazyLock::new(|| {
    let (tx, rx) = channel::<Key>();
    std::thread::spawn(move || {
        while let Ok(key) = rx.recv() {
            let thumb = make(&key.0, key.1, key.2).ok().map(Arc::new);
            if let Ok(mut cache) = CACHE.lock() {
                cache.insert(key, thumb);
            }
            READY.store(true, Ordering::Release);
        }
    });
    Mutex::new(tx)
});

/// Preview size in (cols, rows) for a `width`×`height` image, at most
/// `max_cols` wide: as large as fits MAX_ROWS, never blown up past about
/// one row per 20 source pixels.
pub fn size(width: u32, height: u32, max_cols: usize) -> (usize, usize) {
    if width == 0 || height == 0 {
        return (1, 1);
    }
    let ratio = f64::from(width) / f64::from(height);
    let max_cols = max_cols.clamp(1, MAX_COLS);
    let natural_rows = (f64::from(height) / 20.0).ceil() as usize;
    let mut rows = MAX_ROWS.min(natural_rows).max(1);
    let mut cols = (ratio * rows as f64 * CELL_ASPECT).round() as usize;
    if cols > max_cols {
        cols = max_cols;
        rows = ((cols as f64 / ratio / CELL_ASPECT).round() as usize).max(1);
    }
    (cols.max(1), rows.max(1))
}

/// The preview of `path` at `cols`×`rows`, if ready; asks for it if not.
pub fn get(path: &Path, cols: usize, rows: usize) -> Option<Arc<Thumb>> {
    let key = (path.to_path_buf(), cols, rows);
    let mut cache = CACHE.lock().ok()?;
    match cache.get(&key) {
        Some(found) => found.clone(),
        None => {
            cache.insert(key.clone(), None);
            if let Ok(jobs) = JOBS.lock() {
                let _ = jobs.send(key);
            }
            None
        }
    }
}

/// Wait until `path` at `cols`×`rows` is made (tests, screenshots).
pub fn wait(
    path: &Path,
    cols: usize,
    rows: usize,
    timeout: std::time::Duration,
) -> Option<Arc<Thumb>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        get(path, cols, rows);
        let done = CACHE
            .lock()
            .ok()
            .and_then(|cache| cache.get(&(path.to_path_buf(), cols, rows)).cloned());
        match done {
            Some(Some(thumb)) => return Some(thumb),
            _ if std::time::Instant::now() >= deadline => return None,
            _ => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

/// True once after any preview finished: time to lay out again.
pub fn take_ready() -> bool {
    READY.swap(false, Ordering::AcqRel)
}

/// Cells reserved while a preview is on its way: faint, same size.
pub fn pending(cols: usize, rows: usize) -> Vec<Row> {
    let faint = Paint::fg(NIGHT.faint);
    (0..rows)
        .map(|_| vec![run("░".repeat(cols), faint)])
        .collect()
}

fn make(path: &Path, cols: usize, rows: usize) -> Result<Thumb, String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    let format = image::guess_format(&bytes).map_err(|err| err.to_string())?;
    let picture =
        image::load_from_memory_with_format(&bytes, format).map_err(|err| err.to_string())?;
    let png = if format == ImageFormat::Png {
        path.to_path_buf()
    } else {
        let dir = std::env::temp_dir().join("smash-thumbs");
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let stem = path
            .file_stem()
            .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
        let png = dir.join(format!("{stem}.png"));
        if !png.exists() {
            picture
                .save_with_format(&png, ImageFormat::Png)
                .map_err(|err| err.to_string())?;
        }
        png
    };
    let small = picture.resize_exact(cols as u32, (rows * 2) as u32, FilterType::Triangle);
    let bg = NIGHT.bg;
    // Transparent pixels show the background, like the window does.
    let color = |x: u32, y: u32| {
        let [r, g, b, a] = small.get_pixel(x, y).0;
        let mix = |c: u8, under: u32| {
            let a = u32::from(a);
            (u32::from(c) * a + under * (255 - a)) / 255
        };
        (mix(r, (bg >> 16) & 0xff) << 16) | (mix(g, (bg >> 8) & 0xff) << 8) | mix(b, bg & 0xff)
    };
    let rows = (0..rows as u32)
        .map(|row| {
            (0..cols as u32)
                .map(|col| {
                    run(
                        "▀",
                        Paint::fg(color(col, row * 2)).on(color(col, row * 2 + 1)),
                    )
                })
                .collect()
        })
        .collect();
    Ok(Thumb { rows, png })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::width;

    #[test]
    fn sizes_keep_the_shape_and_respect_limits() {
        let (cols, rows) = size(1600, 900, 200);
        assert_eq!(rows, MAX_ROWS, "big images fill the height");
        assert!(cols <= MAX_COLS);
        let shape = cols as f64 / (rows as f64 * CELL_ASPECT);
        assert!(
            (shape - 16.0 / 9.0).abs() < 0.25,
            "16:9 stays 16:9: {cols}×{rows}"
        );
        assert_eq!(
            size(12, 8, 80),
            (1, 1).max(size(12, 8, 80)),
            "tiny stays tiny"
        );
        assert!(size(12, 8, 80).1 == 1);
        let (cols, _) = size(4000, 400, 30);
        assert_eq!(cols, 30, "narrow screens cap the width");
        assert_eq!(size(0, 10, 40), (1, 1));
    }

    #[test]
    fn previews_are_exact_half_blocks_with_a_png_beside_them() {
        let dir = std::env::temp_dir().join(format!("smash-thumbs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Top half red, bottom half blue.
        let picture = image::RgbImage::from_fn(40, 20, |_, y| {
            if y < 10 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        });
        let jpg = dir.join("split.jpg");
        image::DynamicImage::ImageRgb8(picture).save(&jpg).unwrap();
        assert!(get(&jpg, 8, 2).is_none(), "first ask starts the work");
        let thumb = wait(&jpg, 8, 2, std::time::Duration::from_secs(5)).expect("made");
        assert_eq!(thumb.rows.len(), 2);
        assert!(thumb.rows.iter().all(|row| width(row) == 8));
        let top = thumb.rows[0][0].paint;
        assert!(
            top.fg >> 16 > 0xc0 && top.fg & 0xff < 0x40,
            "top pixel red: {:06x}",
            top.fg
        );
        let bottom = thumb.rows[1][0].paint;
        assert!(bottom.bg.unwrap() & 0xff > 0xc0, "bottom pixel blue");
        assert_eq!(
            thumb.png.extension().unwrap(),
            "png",
            "a JPEG gets a PNG twin"
        );
        assert!(image::open(&thumb.png).is_ok());
        assert!(take_ready());
        assert!(!take_ready(), "once");
        assert_eq!(pending(5, 3).len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
