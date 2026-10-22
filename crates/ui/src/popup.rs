//! Popups as data. A [`Popup`] is a titled stack of grid rows; each client
//! owns only the machinery (overlay, border, focus, hover) and paints the rows
//! with its normal row painter. Every popup's look and behaviour lives here.

use unicode_width::UnicodeWidthStr;

use crate::NIGHT;
use crate::layout::{ACCENT, DIM, Paint, Row, TEXT, clip, run};

#[derive(Clone, Debug)]
pub struct Popup {
    /// How open it is, 0..1 (more while it overshoots): clients mix the
    /// border and title in by it.
    pub open: f64,
    pub title: String,
    /// Inner width in cells; every row is exactly this wide.
    pub width: usize,
    pub rows: Vec<Row>,
    /// Per row: the item a click on it picks (`Chat::pick`), if any.
    pub items: Vec<Option<usize>>,
    /// Text caret (row, col), e.g. in a filter field.
    pub caret: Option<(usize, usize)>,
}

/// `text` clipped and padded to exactly `width` cells in one paint.
fn cell(text: &str, width: usize, paint: Paint) -> Row {
    let text = clip(text, width);
    let pad = width - text.width();
    vec![run(text, paint), run(" ".repeat(pad), paint)]
}

/// Filter field on top, then the visible window of matches.
/// `items`: (match index, label, selected).
pub fn picker(title: String, query: &str, items: &[(usize, String, bool)], width: usize) -> Popup {
    let width = width.max(12);
    let mut rows = Vec::with_capacity(items.len() + 2);
    let mut targets = Vec::with_capacity(items.len() + 2);

    let mut field = vec![run("❯ ", ACCENT)];
    if query.is_empty() {
        field.extend(cell("type to search", width - 2, DIM));
    } else {
        field.extend(cell(query, width - 2, TEXT));
    }
    rows.push(field);
    targets.push(None);
    let caret = Some((0, (2 + query.width()).min(width - 1)));

    if items.is_empty() {
        rows.push(cell("no match · esc closes", width, DIM));
        targets.push(None);
    }
    for (index, label, selected) in items {
        // `name\thint`: the hint (a key, "current") sits flush right, dim.
        let (name, hint) = label.split_once('\t').unwrap_or((label, ""));
        let hint = clip(hint, (width / 3).max(4));
        let room = width - 2 - hint.width();
        let row = if *selected {
            let band = TEXT.on(NIGHT.surface);
            let mut row = vec![run("▌ ", ACCENT.on(NIGHT.surface))];
            row.extend(cell(name, room, band.bold()));
            row.push(run(hint, DIM.on(NIGHT.surface)));
            row
        } else {
            let mut row = vec![run("  ", DIM)];
            row.extend(cell(name, room, TEXT));
            row.push(run(hint, Paint::fg(NIGHT.faint)));
            row
        };
        rows.push(row);
        targets.push(Some(*index));
    }
    Popup {
        open: 1.0,
        title,
        width,
        rows,
        items: targets,
        caret,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::width;

    #[test]
    fn picker_rows_are_exact_and_clickable() {
        let items = vec![
            (0, "short".into(), false),
            (1, "a very long session name indeed".into(), true),
        ];
        let popup = picker("sessions 2/2".into(), "se", &items, 20);
        assert!(popup.rows.iter().all(|row| width(row) == 20));
        let hinted = picker(
            "c".into(),
            "",
            &[(0, "New session\tctrl+n".into(), false)],
            30,
        );
        let text: String = hinted.rows[1].iter().map(|run| run.text.as_str()).collect();
        assert!(
            text.starts_with("  New session") && text.ends_with("ctrl+n"),
            "{text:?}"
        );
        assert_eq!(width(&hinted.rows[1]), 30);
        assert_eq!(popup.items, [None, Some(0), Some(1)]);
        assert_eq!(popup.caret, Some((0, 4)));
        let empty = picker("sessions 0/2".into(), "zz", &[], 20);
        assert_eq!(empty.items, [None, None]);
    }
}
