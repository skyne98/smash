//! Character-grid layout shared by every frontend.
//!
//! Everything is laid out in monospace cells: a [`Row`] is exactly one line of
//! the screen, already wrapped and padded. The terminal prints rows as-is; gpui
//! draws them with a monospace font. Same rows, same wrapping, same pixels-per-
//! cell decisions, so both clients look and behave identically.

use crate::editor::Editor;
use crate::highlight::Token;
use crate::markdown::{self, Block, Ink};
use crate::{NIGHT, Role, transcript};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Colour and weight of one run. Colours are `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint {
    pub fg: u32,
    pub bg: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

impl Paint {
    pub const fn fg(fg: u32) -> Self {
        Self {
            fg,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
        }
    }
    pub const fn on(mut self, bg: u32) -> Self {
        self.bg = Some(bg);
        self
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

pub const TEXT: Paint = Paint::fg(NIGHT.text);
pub const DIM: Paint = Paint::fg(NIGHT.dim);
pub const ACCENT: Paint = Paint::fg(NIGHT.accent);
pub const BAND: Paint = TEXT.on(NIGHT.surface);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub paint: Paint,
}

pub fn run(text: impl Into<String>, paint: Paint) -> Run {
    Run {
        text: text.into(),
        paint,
    }
}

/// One screen line. Never wider than the width it was laid out for.
pub type Row = Vec<Run>;

pub fn width(row: &[Run]) -> usize {
    row.iter().map(|run| run.text.width()).sum()
}

pub fn ink(ink: Ink) -> Paint {
    match ink {
        Ink::Plain => TEXT,
        Ink::Bold => TEXT.bold(),
        Ink::Italic => TEXT.italic(),
        Ink::Strike => Paint {
            strike: true,
            ..DIM
        },
        Ink::Code => Paint::fg(NIGHT.code).on(NIGHT.surface),
        Ink::Syntax(token) => Paint::fg(syntax(token)),
        // Four visible tiers: underlined accent, accent, bright, dim.
        Ink::Heading(1) => Paint {
            underline: true,
            ..ACCENT.bold()
        },
        Ink::Heading(2) => ACCENT.bold(),
        Ink::Heading(3) => TEXT.bold(),
        Ink::Heading(_) => DIM.bold(),
        Ink::Dim => DIM,
    }
}

fn syntax(token: Token) -> u32 {
    token.color()
}

/// Word-wrap `body` to `width` cells. Row one starts with `first`, later rows
/// with `hang` so wrapped text keeps its column. `fill` pads every row to the
/// full width (bands behind prompts and code).
pub fn wrap(first: Row, hang: Row, body: Row, width: usize, fill: Option<Paint>) -> Vec<Row> {
    fn finish(row: &mut Row, col: usize, width: usize, fill: Option<Paint>, rows: &mut Vec<Row>) {
        if let Some(paint) = fill
            && col < width
        {
            row.push(run(" ".repeat(width - col), paint));
        }
        rows.push(std::mem::take(row));
    }
    let width = width.max(8);
    let mut rows = Vec::new();
    let mut col = self::width(&first);
    let mut row = first;
    let mut empty = true;
    for span in body {
        let paint = span.paint;
        for word in span.text.split_inclusive(' ') {
            let mut rest = word;
            while !rest.is_empty() {
                if col + rest.trim_end().width() <= width {
                    // Trailing spaces may hang past the edge; clip them.
                    let fits: String = clip(rest, width - col);
                    col += fits.width();
                    row.push(run(fits, paint));
                    empty = false;
                    break;
                }
                if !empty {
                    finish(&mut row, col, width, fill, &mut rows);
                    row = hang.clone();
                    col = self::width(&row);
                    empty = true;
                    rest = rest.trim_start();
                    continue;
                }
                // One word wider than the row: hard split it.
                let room = width.saturating_sub(col).max(1);
                let piece = clip(rest, room);
                let piece = if piece.is_empty() {
                    rest.chars().next().map(String::from).unwrap_or_default()
                } else {
                    piece
                };
                col += piece.width();
                rest = &rest[piece.len()..];
                row.push(run(piece, paint));
                empty = false;
            }
        }
    }
    finish(&mut row, col, width, fill, &mut rows);
    rows
}

/// Longest prefix of `text` that fits in `cells`.
pub(crate) fn clip(text: &str, cells: usize) -> String {
    let mut used = 0;
    let mut end = 0;
    for (at, ch) in text.char_indices() {
        let w = ch.width().unwrap_or(0);
        if used + w > cells {
            break;
        }
        used += w;
        end = at + ch.len_utf8();
    }
    text[..end].to_string()
}

/// Laid-out rows plus where the code blocks are, for click-to-copy.
#[derive(Default)]
pub struct Laid {
    pub rows: Vec<Row>,
    /// (header row index, code source) for every code panel.
    pub copies: Vec<(usize, String)>,
}

impl Laid {
    fn extend(&mut self, other: Laid) {
        let offset = self.rows.len();
        self.copies.extend(
            other
                .copies
                .into_iter()
                .map(|(row, code)| (row + offset, code)),
        );
        self.rows.extend(other.rows);
    }
}

/// Whole stored transcript: banded prompts, markdown answers, one blank row
/// between turns. `copy` labels code headers when the client has a mouse.
pub fn transcript_rows(body: &[String], width: usize, copy: Option<&str>) -> Laid {
    let parsed = transcript(body);
    let mut laid = Laid::default();
    let mut index = 0;
    while index < parsed.len() {
        if !laid.rows.is_empty() {
            laid.rows.push(Row::new());
        }
        match parsed[index].role {
            Role::User => {
                for (line, text) in parsed[index].text.split('\n').enumerate() {
                    let first = run(if line == 0 { "> " } else { "  " }, DIM.on(NIGHT.surface));
                    laid.rows.extend(wrap(
                        vec![first],
                        vec![run("  ", BAND)],
                        vec![run(text, BAND)],
                        width,
                        Some(BAND),
                    ));
                }
                index += 1;
            }
            Role::Assistant => {
                let mut text = String::new();
                while index < parsed.len() && parsed[index].role == Role::Assistant {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&parsed[index].text);
                    index += 1;
                }
                laid.extend(answer_rows(&text, width, copy));
            }
        }
    }
    laid
}

fn is_bar(span: &markdown::Span) -> bool {
    span.ink == Ink::Dim && span.text == "│ "
}

/// One assistant answer: `● ` marker on the first row, markdown blocks below.
pub fn answer_rows(text: &str, width: usize, copy: Option<&str>) -> Laid {
    let blocks = markdown::render(text, width.saturating_sub(4).max(8));
    let mut laid = Laid::default();
    let rows = &mut laid.rows;
    let mut index = 0;
    while index < blocks.len() {
        let block = &blocks[index];
        if block.code {
            let end = blocks[index..]
                .iter()
                .position(|block| !block.code)
                .map_or(blocks.len(), |offset| index + offset);
            let (panel, code) = code_panel(&blocks[index..end], width, index == 0, copy);
            laid.copies.push((rows.len(), code));
            rows.extend(panel);
            index = end;
            continue;
        }
        index += 1;
        if block.spans.is_empty() {
            rows.push(Row::new());
            continue;
        }
        let marker = if index == 1 {
            run("● ", ACCENT)
        } else {
            run("  ", TEXT)
        };
        // Wrapped list items hang under their text, not under the bullet.
        let bullet = match block.spans.first() {
            Some(span)
                if span.ink == Ink::Dim && span.text.ends_with(' ') && span.text.len() <= 5 =>
            {
                span.text.width()
            }
            _ => 0,
        };
        // Quote bars repeat on wrapped rows; bullets and numbers do not.
        let quote: Row = block
            .spans
            .iter()
            .take_while(|span| is_bar(span))
            .map(|span| run(span.text.clone(), ink(span.ink)))
            .collect();
        let hang = if quote.is_empty() {
            vec![run(" ".repeat(2 + block.indent + bullet), TEXT)]
        } else {
            let mut hang = vec![run(" ".repeat(2 + block.indent), TEXT)];
            hang.extend(quote);
            hang
        };
        let body = block
            .spans
            .iter()
            .map(|span| run(span.text.clone(), ink(span.ink)))
            .collect();
        let first = vec![marker, run(" ".repeat(block.indent), TEXT)];
        rows.extend(wrap(first, hang, body, width, None));
    }
    laid
}

/// One fenced block as a panel: language label row, then the code lines,
/// numbered when there are 3+. Wrapped lines hang under the code.
fn code_panel(
    blocks: &[Block],
    width: usize,
    first: bool,
    copy: Option<&str>,
) -> (Vec<Row>, String) {
    let surface = Paint::fg(NIGHT.text).on(NIGHT.surface);
    let bars = blocks[0]
        .spans
        .iter()
        .take_while(|span| is_bar(span))
        .count();
    // Everything left of the panel: answer marker, list indent, quote bars.
    let mut outside = vec![
        run(if first { "● " } else { "  " }, ACCENT),
        run(" ".repeat(blocks[0].indent), TEXT),
    ];
    outside.extend((0..bars).map(|_| run("│ ", DIM)));
    let mut blank = outside.clone();
    blank[0] = run("  ", TEXT);

    let label: String = blocks[0].spans[bars..]
        .iter()
        .map(|span| span.text.as_str())
        .collect();
    let code = &blocks[1..];
    let numbered = code.len() >= 3;
    let digits = if numbered {
        code.len().to_string().len()
    } else {
        0
    };
    let number = Paint::fg(NIGHT.faint).on(NIGHT.surface);

    let mut rows = Vec::with_capacity(code.len() + 1);
    let mut head = outside;
    head.push(run(format!(" {label}"), DIM.on(NIGHT.surface).italic()));
    rows.extend(wrap(head, blank.clone(), Row::new(), width, Some(surface)));
    // Right-aligned click target in the header, carved out of the fill.
    if let Some(label) = copy
        && let Some(fill) = rows[0].pop()
    {
        let room = fill.text.len();
        let need = label.width() + 1;
        if room > need {
            rows[0].push(run(" ".repeat(room - need), surface));
            rows[0].push(run(label, Paint::fg(NIGHT.dim).on(NIGHT.surface)));
            rows[0].push(run(" ", surface));
        } else {
            rows[0].push(fill);
        }
    }
    let source = code
        .iter()
        .map(|block| {
            block.spans[bars.min(block.spans.len())..]
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (line, block) in code.iter().enumerate() {
        let mut first = blank.clone();
        let mut hang = blank.clone();
        if numbered {
            first.push(run(format!(" {:>digits$}  ", line + 1), number));
            hang.push(run(format!(" {}  ", " ".repeat(digits)), number));
        } else {
            first.push(run("  ", surface));
            hang.push(run("  ", surface));
        }
        let body = block.spans[bars.min(block.spans.len())..]
            .iter()
            .map(|span| {
                let mut paint = ink(span.ink);
                paint.bg = Some(NIGHT.surface);
                run(span.text.clone(), paint)
            })
            .collect();
        rows.extend(wrap(first, hang, body, width, Some(surface)));
    }
    (rows, source)
}

/// One visual row of the prompt and how its cells map back to the text.
pub struct InputRow {
    pub row: Row,
    /// Byte range of the text on this row.
    pub start: usize,
    pub end: usize,
    /// Byte offset under each text column (after the 2-cell `> ` prefix).
    pub cols: Vec<usize>,
}

const CARET: Paint = Paint::fg(NIGHT.bg).on(NIGHT.accent);
const SELECT: Paint = TEXT.on(NIGHT.select);

/// Cells a char takes in the prompt; tabs and controls show as one space.
fn cells(c: char) -> usize {
    if c == '\t' || c.is_control() {
        1
    } else {
        c.width().unwrap_or(0)
    }
}

/// The prompt: word-wrapped rows with block caret and selection, plus the
/// index of the row holding the caret (so the client can keep it visible).
pub fn input(editor: &Editor, placeholder: &str, width: usize) -> (Vec<InputRow>, usize) {
    let text = editor.text();
    if text.is_empty() {
        let mut rest = placeholder.chars();
        let first = rest.next().map_or(" ".into(), String::from);
        let row = vec![
            run("> ", ACCENT),
            run(first, CARET),
            run(rest.as_str(), DIM),
        ];
        let empty = InputRow {
            row,
            start: 0,
            end: 0,
            cols: Vec::new(),
        };
        return (vec![empty], 0);
    }
    // Room for the prefix and a caret cell after a full row.
    let avail = width.saturating_sub(3).max(4);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut base = 0;
    for line in text.split('\n') {
        let (mut start, mut col, mut space) = (base, 0, None);
        for (i, c) in line.char_indices() {
            let at = base + i;
            let w = cells(c);
            if col + w > avail && at > start {
                // Break after the last space on the row, else mid-word.
                let cut = space.filter(|&sp| sp > start && sp <= at).unwrap_or(at);
                ranges.push((start, cut));
                start = cut;
                space = None;
                col = text[cut..at].chars().map(cells).sum();
            }
            col += w;
            if c == ' ' {
                space = Some(at + 1);
            }
        }
        ranges.push((start, base + line.len()));
        base += line.len() + 1;
    }

    // At a soft wrap the caret belongs to the next row; at a newline, this one.
    let cursor = editor.cursor();
    let caret = ranges
        .iter()
        .enumerate()
        .position(|(r, &(s, e))| {
            s <= cursor
                && (cursor < e
                    || (cursor == e && ranges.get(r + 1).is_none_or(|&(next, _)| next != e)))
        })
        .unwrap_or(ranges.len() - 1);
    let selection = editor.selection();
    let selected = |at: usize| selection.as_ref().is_some_and(|range| range.contains(&at));

    let rows = ranges
        .iter()
        .enumerate()
        .map(|(r, &(start, end))| {
            let mut row = vec![run(if r == 0 { "> " } else { "  " }, ACCENT)];
            let mut cols = Vec::new();
            for (i, c) in text[start..end].char_indices() {
                let at = start + i;
                let shown = if c == '\t' || c.is_control() { ' ' } else { c };
                let paint = if r == caret && at == cursor {
                    CARET
                } else if selected(at) {
                    SELECT
                } else {
                    TEXT
                };
                match row.last_mut() {
                    Some(last) if last.paint == paint && last.text != "> " => last.text.push(shown),
                    _ => row.push(run(shown.to_string(), paint)),
                }
                cols.extend(std::iter::repeat_n(at, cells(c)));
            }
            if r == caret && cursor == end {
                row.push(run(" ", CARET));
            } else if text[end..].starts_with('\n') && selected(end) {
                // A selected newline shows as one highlighted cell.
                row.push(run(" ", SELECT));
            }
            InputRow {
                row,
                start,
                end,
                cols,
            }
        })
        .collect();
    (rows, caret)
}

/// `left` and `right` on one row of `width`; `left` is cut at ` · `
/// boundaries so a hint is shown whole or not at all.
pub fn status_row(left: &str, left_paint: Paint, right: &str, width: usize) -> Row {
    let right = clip(right, width);
    let room = width.saturating_sub(right.width() + 2);
    let mut shown = String::new();
    for item in left.split(" · ") {
        let next = if shown.is_empty() {
            item.to_string()
        } else {
            format!("{shown} · {item}")
        };
        if next.width() > room {
            break;
        }
        shown = next;
    }
    let gap = width.saturating_sub(shown.width() + right.width());
    vec![
        run(shown, left_paint),
        run(" ".repeat(gap), DIM),
        run(right, DIM),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_never_exceed_width() {
        let text = "# Title\n\nA paragraph that is long enough to wrap several times at forty columns.\n\n- item with `code`\n\n```rust\nfn main() { println!(\"a very long line that has to wrap inside the panel\"); }\n```\n";
        for width in [20, 40, 80] {
            for row in answer_rows(text, width, Some("copy")).rows {
                assert!(super::width(&row) <= width, "{width}: {row:?}");
            }
        }
        let body: Vec<String> = vec![format!(
            "{}hello there, a long prompt that wraps",
            crate::USER
        )];
        for row in transcript_rows(&body, 20, None).rows {
            assert_eq!(super::width(&row), 20, "prompt band fills the row");
        }
    }

    #[test]
    fn code_header_is_a_copy_target() {
        let laid = answer_rows("hi\n\n```py\na = 1\nb = 2\n```\n", 40, Some("copy"));
        assert_eq!(laid.copies.len(), 1);
        let (row, code) = &laid.copies[0];
        assert_eq!(code, "a = 1\nb = 2");
        let header: String = laid.rows[*row]
            .iter()
            .map(|run| run.text.as_str())
            .collect();
        assert!(
            header.contains("py") && header.trim_end().ends_with("copy"),
            "{header:?}"
        );
        assert_eq!(super::width(&laid.rows[*row]), 40);
    }

    #[test]
    fn input_wraps_and_maps_clicks() {
        let mut editor = Editor::default();
        editor.insert("hello world again\nx");
        editor.move_to(0, false);
        let (rows, caret) = input(&editor, "", 12);
        assert_eq!(caret, 0);
        let texts: Vec<String> = rows
            .iter()
            .map(|row| row.row.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert_eq!(texts, ["> hello ", "  world ", "  again", "  x"]);
        assert_eq!(rows[1].cols[0], 6, "column 0 of row 2 is the 'w'");
        for row in &rows {
            assert!(super::width(&row.row) <= 12);
        }
        editor.move_to(6, false);
        assert_eq!(
            input(&editor, "", 12).1,
            1,
            "caret at a soft wrap sits on the next row"
        );
    }

    #[test]
    fn status_drops_whole_hints() {
        let row = status_row("ctrl+p sessions · ctrl+n new", DIM, "s1", 22);
        let text: String = row.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(text, "ctrl+p sessions     s1");
    }
}
