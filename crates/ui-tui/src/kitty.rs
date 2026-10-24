//! Real pictures in kitty-protocol terminals (kitty, Ghostty, WezTerm).
//!
//! The frame already holds a half-block rendering of every image, so the
//! terminal shows something sensible on its own. Here, when the terminal
//! speaks the kitty graphics protocol, each visible picture is drawn over its
//! cells as the real image: transmitted once (by file path, so no image bytes
//! cross the pty), then placed per frame. Placements are replaced, never
//! stacked, and cleared when a picture leaves the screen.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

use smash_ui::chat::Shown;

/// Whether this terminal draws kitty-protocol images; `SMASH_IMAGES=off`
/// forces half blocks, `=kitty` forces images.
pub fn supported() -> bool {
    match std::env::var("SMASH_IMAGES").as_deref() {
        Ok("off" | "0" | "none") => return false,
        Ok("kitty") => return true,
        _ => {}
    }
    // tmux eats the protocol unless passthrough is set up; stay with blocks.
    if std::env::var_os("TMUX").is_some() {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term.contains("kitty")
        || term.contains("ghostty")
        || program == "WezTerm"
        || program == "ghostty"
}

#[derive(Default)]
pub struct Kitty {
    /// Image id per transmitted file.
    ids: HashMap<PathBuf, u32>,
    next: u32,
    /// What the last frame placed: (id, x, y, cols, rows, skip).
    placed: HashSet<(u32, u16, u16, usize, usize, usize)>,
}

impl Kitty {
    /// Place `pictures` (frame coordinates, offset by `origin`), replacing
    /// whatever the last frame placed. Writes nothing when nothing changed.
    pub fn draw(
        &mut self,
        pictures: &[Shown],
        origin: (u16, u16),
        cell_px: (u32, u32),
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        let mut now = HashSet::new();
        let mut commands = String::new();
        for shown in pictures
            .iter()
            .filter(|shown| shown.visible > 0 && shown.fade > 0.5)
        {
            let p = &shown.picture;
            let id = match self.ids.get(&p.png) {
                Some(id) => *id,
                None => {
                    self.next += 1;
                    let id = self.next;
                    let path = base64(p.png.display().to_string().as_bytes());
                    // t=f: the terminal reads the file itself. q=2: no replies.
                    commands.push_str(&format!("\x1b_Ga=t,t=f,f=100,i={id},q=2;{path}\x1b\\"));
                    self.ids.insert(p.png.clone(), id);
                    id
                }
            };
            let x = origin.0 + p.col as u16;
            let y = origin.1 + p.row as u16;
            now.insert((id, x, y, p.cols, shown.visible, shown.skip));
        }
        if now == self.placed {
            return Ok(());
        }
        // Clear every placement, then place this frame's: simple and exact.
        commands.push_str("\x1b_Ga=d,d=a,q=2\x1b\\");
        for &(id, x, y, cols, rows, skip) in &now {
            // A picture cut by the top edge shows its lower part (source rows).
            let full_rows = pictures
                .iter()
                .find(|shown| self.ids.get(&shown.picture.png) == Some(&id))
                .map_or(rows, |shown| shown.picture.rows);
            let crop = if skip > 0 && cell_px.1 > 0 {
                format!(
                    ",y={},h={}",
                    skip as u32 * cell_px.1,
                    rows as u32 * cell_px.1
                )
            } else {
                String::new()
            };
            let _ = full_rows;
            commands.push_str(&format!(
                "\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},c={cols},r={rows},C=1,z=1,q=2{crop}\x1b\\\x1b8",
                y + 1,
                x + 1
            ));
        }
        self.placed = now;
        out.write_all(commands.as_bytes())?;
        out.flush()
    }

    /// Remove every picture (on exit).
    pub fn clear(&mut self, out: &mut impl Write) -> std::io::Result<()> {
        if self.ids.is_empty() {
            return Ok(());
        }
        self.placed.clear();
        out.write_all(b"\x1b_Ga=d,d=A,q=2\x1b\\")?;
        out.flush()
    }
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smash_ui::layout::Picture;

    fn shown(row: usize, skip: usize) -> Shown {
        Shown {
            picture: Picture {
                row,
                col: 4,
                cols: 20,
                rows: 8,
                png: PathBuf::from("/tmp/a.png"),
            },
            skip,
            visible: 8 - skip,
            fade: 1.0,
        }
    }

    #[test]
    fn transmits_once_places_per_frame_and_stays_quiet_when_unchanged() {
        let mut kitty = Kitty::default();
        let mut out = Vec::new();
        kitty
            .draw(&[shown(3, 0)], (10, 0), (8, 16), &mut out)
            .unwrap();
        let first = String::from_utf8(out.clone()).unwrap();
        assert!(first.contains("a=t,t=f,f=100,i=1"), "transmitted by path");
        assert!(first.contains(&base64(b"/tmp/a.png")));
        assert!(
            first.contains("\x1b[4;15H"),
            "placed at row 4, col 15: {first:?}"
        );
        assert!(first.contains("a=p,i=1,c=20,r=8,C=1"));

        out.clear();
        kitty
            .draw(&[shown(3, 0)], (10, 0), (8, 16), &mut out)
            .unwrap();
        assert!(out.is_empty(), "same frame, nothing written");

        out.clear();
        kitty
            .draw(&[shown(0, 3)], (10, 0), (8, 16), &mut out)
            .unwrap();
        let scrolled = String::from_utf8(out.clone()).unwrap();
        assert!(!scrolled.contains("a=t"), "not transmitted again");
        assert!(
            scrolled.contains("a=d,d=a")
                && scrolled.contains("r=5")
                && scrolled.contains("y=48,h=80"),
            "cropped: {scrolled:?}"
        );

        out.clear();
        kitty.draw(&[], (10, 0), (8, 16), &mut out).unwrap();
        assert!(
            String::from_utf8(out).unwrap().contains("a=d,d=a"),
            "gone when off screen"
        );
    }

    #[test]
    fn faded_pictures_wait_for_their_fade() {
        let mut kitty = Kitty::default();
        let mut out = Vec::new();
        let mut half = shown(2, 0);
        half.fade = 0.2;
        kitty.draw(&[half], (0, 0), (8, 16), &mut out).unwrap();
        assert!(out.is_empty(), "half blocks fade in first");
    }
}
