//! Popups as data. A [`Popup`] is a titled stack of grid rows; each client
//! owns only the machinery (overlay, border, focus, hover) and paints the rows
//! with its normal row painter. Every popup's look and behaviour lives here.

use unicode_width::UnicodeWidthStr;

use crate::NIGHT;
use crate::layout::{ACCENT, Bar, DIM, Paint, Row, TEXT, clip, run};

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
    /// Scrollbar beside the item rows when not all items fit, in the popup's
    /// own row/column coordinates. Its column is just past the rows: the
    /// box's edge (the border in a terminal, the padding in the window), so
    /// it takes no room from the content.
    pub bar: Option<Bar>,
}

/// Blank columns after every item row, mirroring the two-cell lead on the
/// left; they also keep a wide last glyph (▣) off the very edge.
const GUTTER: usize = 2;

/// `text` clipped (with `…`) and padded to exactly `width` cells in one paint.
fn cell(text: &str, width: usize, paint: Paint) -> Row {
    let text = if text.width() > width && width > 1 {
        format!("{}…", clip(text, width - 1))
    } else {
        clip(text, width)
    };
    let pad = width - text.width();
    vec![run(text, paint), run(" ".repeat(pad), paint)]
}

/// One text field (renaming): `placeholder` dim while it is empty.
pub fn field(title: String, text: &str, placeholder: &str, width: usize) -> Popup {
    let width = width.max(12);
    let mut row = vec![run("❯ ", ACCENT)];
    if text.is_empty() {
        row.extend(cell(placeholder, width - 2, DIM));
    } else {
        row.extend(cell(text, width - 2, TEXT));
    }
    Popup {
        open: 1.0,
        title,
        width,
        rows: vec![row],
        items: vec![None],
        caret: Some((0, (2 + text.width()).min(width - 1))),
        bar: None,
    }
}

/// Filter field on top, then the visible window of matches.
/// `items`: (match index, label, selected); `total` matches in all, the
/// first shown being match `top` (for the scrollbar).
pub fn picker(
    title: String,
    query: &str,
    items: &[(usize, String, bool)],
    width: usize,
    total: usize,
    top: usize,
) -> Popup {
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
    // More matches than rows: a scrollbar in the box's edge, past the rows.
    let bar = crate::layout::bar(width, 1, items.len(), total, top);
    let inner = width - GUTTER;
    for (index, label, selected) in items {
        // `name\thint`: the hint (a key, "current", a model's columns) sits
        // flush right and keeps its full width; the name gives way, with …
        let (name, hint) = label.split_once('\t').unwrap_or((label, ""));
        let hint = clip(hint, inner.saturating_sub(16).max(4));
        // Lead "▌ ", the name, then (only with a hint) two cells and the hint.
        let gap = if hint.is_empty() { 0 } else { 2 };
        let room = inner.saturating_sub(2 + gap + hint.width());
        let (lead, band, name_paint, hint_paint) = if *selected {
            let band = TEXT.on(NIGHT.surface);
            (
                run("▌ ", ACCENT.on(NIGHT.surface)),
                band,
                band.bold(),
                DIM.on(NIGHT.surface),
            )
        } else {
            (run("  ", DIM), TEXT, TEXT, Paint::fg(NIGHT.faint))
        };
        let mut row = vec![lead];
        row.extend(cell(name, room, name_paint));
        if gap > 0 {
            row.push(run("  ", band));
            row.push(run(hint, hint_paint));
        }
        row.push(run(" ".repeat(GUTTER), TEXT));
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
        bar,
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
        let popup = picker("sessions 2/2".into(), "se", &items, 20, 2, 0);
        assert!(popup.rows.iter().all(|row| width(row) == 20));
        let hinted = picker(
            "c".into(),
            "",
            &[(0, "New session\tctrl+n".into(), false)],
            30,
            1,
            0,
        );
        let text: String = hinted.rows[1].iter().map(|run| run.text.as_str()).collect();
        assert!(
            text.starts_with("  New session") && text.trim_end().ends_with("ctrl+n"),
            "{text:?}"
        );
        assert_eq!(width(&hinted.rows[1]), 30);
        assert_eq!(popup.items, [None, Some(0), Some(1)]);
        assert_eq!(popup.caret, Some((0, 4)));
        let empty = picker("sessions 0/2".into(), "zz", &[], 20, 0, 0);
        assert_eq!(empty.items, [None, None]);
        assert!(hinted.bar.is_none(), "everything fits: no scrollbar");
        assert_eq!(
            text.len() - text.trim_end().len(),
            2,
            "two cells of air, like the lead"
        );
    }

    #[test]
    fn long_names_give_way_hints_stay_whole_and_a_bar_appears_when_needed() {
        let label = "DeepSeek V4 Flash Vision Experimental Preview\tdeepseek  1M  $0.15/$0.60  ✻ ▣"
            .to_string();
        let items: Vec<_> = (0..3).map(|i| (i, label.clone(), i == 1)).collect();
        let popup = picker("models".into(), "", &items, 60, 9, 6);
        for row in &popup.rows {
            assert_eq!(width(row), 60, "exact width");
        }
        let text: String = popup.rows[1].iter().map(|run| run.text.as_str()).collect();
        assert!(
            text.contains('…'),
            "name clipped with an ellipsis: {text:?}"
        );
        assert!(
            text.trim_end().ends_with("✻ ▣"),
            "the marks survive: {text:?}"
        );
        let bar = popup.bar.expect("9 matches, 3 shown");
        assert_eq!(
            (bar.col, bar.rows.clone()),
            (60, 1..4),
            "in the edge, past the rows"
        );
        assert_eq!(
            bar.thumb.end, bar.rows.end,
            "last page: thumb at the bottom"
        );
    }
}
