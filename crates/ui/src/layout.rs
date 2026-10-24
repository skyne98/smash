//! Character-grid layout shared by every frontend.
//!
//! Everything is laid out in monospace cells: a [`Row`] is exactly one line of
//! the screen, already wrapped and padded. The terminal prints rows as-is; gpui
//! draws them with a monospace font. Same rows, same wrapping, same pixels-per-
//! cell decisions, so both clients look and behave identically.

use std::ops::Range;

use crate::editor::Editor;
use crate::highlight::Token;
use crate::markdown::{self, Block, Ink};
use crate::{Line, NIGHT, Role, transcript};
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
    /// Every image preview: where its cells are, for clients that draw the
    /// real picture over them.
    pub images: Vec<Picture>,
    /// (row, key) of every folded run's summary row: a click opens it.
    pub folds: Vec<(usize, usize)>,
}

/// How finished work (reasoning, programs, subagents) is drawn. By default
/// each run of it folds into one summary row, so a turn reads as prompt →
/// answer; only the turn in flight shows its current step in full.
#[derive(Clone, Copy, Default)]
pub struct Fold<'a> {
    /// Everything expanded (ctrl+o).
    pub all: bool,
    /// Runs opened one by one (a click), keyed by their first line's index.
    pub open: Option<&'a std::collections::HashSet<usize>>,
    /// Index of the first line given, in the whole transcript (for keys).
    pub base: usize,
}

impl Fold<'_> {
    pub fn all() -> Self {
        Self {
            all: true,
            ..Self::default()
        }
    }

    fn opened(&self, key: usize) -> bool {
        self.all || self.open.is_some_and(|open| open.contains(&key))
    }
}

/// An image drawn over a block of cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    /// First row and column of the block.
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    /// A PNG of the image.
    pub png: std::path::PathBuf,
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
        self.images
            .extend(other.images.into_iter().map(|mut picture| {
                picture.row += offset;
                picture
            }));
        self.folds.extend(
            other
                .folds
                .into_iter()
                .map(|(row, key)| (row + offset, key)),
        );
        self.rows.extend(other.rows);
    }
}

/// Whole stored transcript: banded prompts, then each answer turn
/// (thinking, tool programs, markdown text), one blank row between turns.
/// `copy` labels code headers when the client has a mouse.
pub fn transcript_rows(body: &[String], width: usize, copy: Option<&str>, fold: Fold) -> Laid {
    let parsed = transcript(body);
    let mut laid = Laid::default();
    let mut index = 0;
    while index < parsed.len() {
        if !laid.rows.is_empty() {
            laid.rows.push(Row::new());
        }
        if parsed[index].role == Role::User {
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
            continue;
        }
        let end = parsed[index..]
            .iter()
            .position(|line| line.role == Role::User)
            .map_or(parsed.len(), |offset| index + offset);
        let fold = Fold {
            base: index,
            ..fold
        };
        laid.extend(turn_rows(&parsed[index..end], width, copy, fold, false));
        index = end;
    }
    laid
}

/// One answer. Consecutive lines of a kind form a block (a program owns
/// its call lines). Runs of work blocks (reasoning, programs, subagents)
/// fold into one summary row unless opened; in the turn in flight (`live`)
/// the last block, the one happening now, is always shown.
pub fn turn_rows(lines: &[Line], width: usize, copy: Option<&str>, fold: Fold, live: bool) -> Laid {
    let blocks = blocks(lines);
    let mut laid = Laid::default();
    let mut previous: Option<Role> = None;
    let mut at = 0;
    while at < blocks.len() {
        let (start, _, role) = blocks[at];
        if !is_work(role) {
            block_rows(
                &mut laid,
                &mut previous,
                lines,
                blocks[at],
                width,
                copy,
                true,
                false,
            );
            at += 1;
            continue;
        }
        let end = blocks[at..]
            .iter()
            .position(|block| !is_work(block.2))
            .map_or(blocks.len(), |offset| at + offset);
        let run = &blocks[at..end];
        let key = fold.base + start;
        if fold.opened(key) {
            // Opened by a click: a ▾ header above it folds it again.
            if !fold.all {
                if previous.is_some() {
                    laid.rows.push(Row::new());
                }
                laid.folds.push((laid.rows.len(), key));
                laid.rows.push(summary_row(lines, run, width, "▾ "));
                previous = Some(Role::Program);
            }
            for (n, &block) in run.iter().enumerate() {
                let now = live && end == blocks.len() && n + 1 == run.len();
                block_rows(
                    &mut laid,
                    &mut previous,
                    lines,
                    block,
                    width,
                    copy,
                    true,
                    now,
                );
            }
        } else {
            let now = live && end == blocks.len();
            let (done, current) = if now {
                (&run[..run.len() - 1], run.last().copied())
            } else {
                (run, None)
            };
            if !done.is_empty() {
                if previous.is_some() {
                    laid.rows.push(Row::new());
                }
                laid.folds.push((laid.rows.len(), key));
                laid.rows.push(summary_row(lines, done, width, "▸ "));
                previous = Some(Role::Program);
            }
            if let Some(block) = current {
                block_rows(
                    &mut laid,
                    &mut previous,
                    lines,
                    block,
                    width,
                    copy,
                    false,
                    true,
                );
            }
        }
        at = end;
    }
    laid
}

/// Reasoning, programs and subagents: the work behind an answer.
fn is_work(role: Role) -> bool {
    matches!(
        role,
        Role::Thinking | Role::Program | Role::Call | Role::Agent
    )
}

/// (first line, end, role) of each block.
fn blocks(lines: &[Line]) -> Vec<(usize, usize, Role)> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let role = lines[index].role;
        let same = |line: &Line| match role {
            Role::Program | Role::Call | Role::Agent => line.role == Role::Call,
            other => line.role == other,
        };
        let end = lines[index + 1..]
            .iter()
            .position(|line| !same(line))
            .map_or(lines.len(), |offset| index + 1 + offset);
        out.push((index, end, role));
        index = end;
    }
    out
}

/// One block, after the blank row (or not) that separates it from the one
/// before. `show` = reasoning in full; `now` = it is happening right now.
#[allow(clippy::too_many_arguments)]
fn block_rows(
    laid: &mut Laid,
    previous: &mut Option<Role>,
    lines: &[Line],
    (start, end, role): (usize, usize, Role),
    width: usize,
    copy: Option<&str>,
    show: bool,
    now: bool,
) {
    let group = &lines[start..end];
    // Programs stack tight, and so do agents; an agent after a program
    // (or the reverse) gets air, so it never reads as one more call.
    let dense = match role {
        Role::Program => matches!(previous, Some(Role::Program | Role::Call)),
        Role::Agent => *previous == Some(Role::Agent),
        // Images sit right under what brought them: a prompt or a program.
        Role::Image => matches!(
            previous,
            Some(Role::User | Role::Image | Role::Program | Role::Call)
        ),
        _ => false,
    };
    if previous.is_some() && !dense {
        laid.rows.push(Row::new());
    }
    match role {
        Role::Thinking => laid.rows.extend(thinking_rows(group, width, show, now)),
        Role::Program | Role::Call => laid.rows.extend(program_rows(group, width, now)),
        Role::Agent => laid.rows.extend(agent_rows(group, width)),
        Role::Image => {
            for line in group {
                laid.extend(image_rows(&line.text, width));
            }
        }
        Role::Fail => {
            let error = Paint::fg(NIGHT.error);
            for line in group {
                laid.rows.extend(wrap(
                    vec![run("⚠ ", error)],
                    vec![run("  ", error)],
                    vec![run(line.text.clone(), error)],
                    width,
                    None,
                ));
            }
        }
        _ => {
            let text: Vec<&str> = group.iter().map(|line| line.text.as_str()).collect();
            laid.extend(answer_rows(&text.join("\n"), width, copy));
        }
    }
    *previous = Some(role);
}

/// A folded run as one row: `▸ Find the config, Run the tests   5 calls ·
/// 2.1s · thought`, failures in red. A click (or ctrl+o) opens it.
fn summary_row(lines: &[Line], work: &[(usize, usize, Role)], width: usize, mark: &str) -> Row {
    let (mut titles, mut calls, mut failed, mut secs, mut words, mut agents) =
        (Vec::new(), 0, 0, 0.0f64, 0, 0);
    for &(start, end, role) in work {
        let body = &lines[start..end];
        match role {
            Role::Thinking => {
                words += body
                    .iter()
                    .map(|line| line.text.split_whitespace().count())
                    .sum::<usize>();
            }
            Role::Program | Role::Agent => {
                let mut fields = lines[start].text.split('\t');
                let first = fields.next().unwrap_or_default();
                let title = if role == Role::Agent {
                    agents += 1;
                    fields.next().unwrap_or(first)
                } else {
                    let summary = fields.next().unwrap_or_default();
                    secs += summary
                        .split(" · ")
                        .find_map(|part| part.strip_suffix('s')?.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    // A program that failed with no failed call to show for it
                    // (a thrown error) still counts once.
                    let failed_calls = body[1..]
                        .iter()
                        .filter(|call| call.text.starts_with('✗'))
                        .count();
                    if summary.contains("error:") && failed_calls == 0 {
                        failed += 1;
                    }
                    first
                };
                if !title.trim().is_empty() {
                    titles.push(title.to_string());
                }
                for call in &body[1..] {
                    calls += 1;
                    if call.text.starts_with('✗') {
                        failed += 1;
                    }
                }
            }
            _ => {}
        }
    }
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut meta = Vec::new();
    if calls > 0 {
        meta.push(plural(calls, "call", "calls"));
    }
    if secs >= 0.05 {
        meta.push(format!("{secs:.1}s"));
    }
    if agents > 0 {
        meta.push(plural(agents, "agent", "agents"));
    }
    if words > 0 && !titles.is_empty() {
        meta.push("thought".to_string());
    }
    let left = if titles.is_empty() {
        format!("thought · {}", plural(words, "word", "words"))
    } else {
        titles.join(", ")
    };
    let mut right = vec![run(meta.join(" · "), Paint::fg(NIGHT.faint))];
    if failed > 0 {
        right.push(run(format!(" · {failed} failed"), Paint::fg(NIGHT.error)));
    }
    spread(vec![run(mark, DIM), run(left, DIM)], right, width)
}

const THOUGHT: Paint = Paint {
    italic: true,
    ..DIM
};

/// Reasoning: dim italic under a `✻` gutter, or one summary row when hidden.
fn thinking_rows(lines: &[Line], width: usize, show: bool, live: bool) -> Vec<Row> {
    let words: usize = lines
        .iter()
        .map(|line| line.text.split_whitespace().count())
        .sum();
    if !show {
        let label = if live { "thinking…" } else { "thought" };
        let mut rows = vec![vec![
            run("✻ ", DIM),
            run(format!("{label} · {words} words"), THOUGHT),
        ]];
        // Thinking now: its last few lines, so it is visibly moving.
        if live {
            let mut tail = Vec::new();
            for line in lines {
                tail.extend(wrap(
                    vec![run("  ", DIM)],
                    vec![run("  ", DIM)],
                    vec![run(line.text.clone(), THOUGHT)],
                    width,
                    None,
                ));
            }
            let skip = tail.len().saturating_sub(LIVE_THOUGHT);
            rows.extend(tail.into_iter().skip(skip));
        }
        return rows;
    }
    let mut rows = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let gutter = run(if index == 0 { "✻ " } else { "  " }, DIM);
        rows.extend(wrap(
            vec![gutter],
            vec![run("  ", DIM)],
            vec![run(line.text.clone(), THOUGHT)],
            width,
            None,
        ));
    }
    rows
}

/// Lines of the current reasoning shown while it streams, folded.
const LIVE_THOUGHT: usize = 3;

/// Width of the verb column in call rows (`spawn`, `write`).
const VERB: usize = 5;
/// Anything still running: one colour everywhere.
const RUNNING: Paint = Paint::fg(NIGHT.code);

/// Calls shown per program before the middle collapses to one `⋯` row.
const CALLS_SHOWN: usize = 8;

/// A tool program as a dense log: header row, one row per call.
fn program_rows(lines: &[Line], width: usize, live: bool) -> Vec<Row> {
    let error = Paint::fg(NIGHT.error);
    let mut rows = Vec::new();
    let (head, calls) = match lines.first() {
        Some(line) if line.role == Role::Program => (Some(line), &lines[1..]),
        _ => (None, lines),
    };
    if let Some(head) = head {
        let (title, summary) = head
            .text
            .split_once('\t')
            .unwrap_or((head.text.as_str(), ""));
        let (summary, failure) = match summary.split_once(" · error: ") {
            Some((summary, failure)) => (summary.to_string(), Some(failure)),
            None => (summary.to_string(), None),
        };
        let (right, paint) = match (summary.is_empty(), failure) {
            (true, _) if live => ("running…".to_string(), RUNNING),
            (_, Some(_)) => (format!("{summary} · failed"), error),
            _ => (summary, DIM),
        };
        rows.push(spread(
            vec![run("⏵ ", ACCENT), run(title, TEXT.bold())],
            vec![run(right, paint)],
            width,
        ));
        if let Some(failure) = failure {
            rows.push(vec![
                run("  └ ", error),
                run(clip(failure, width.saturating_sub(4)), error),
            ]);
        }
    }
    let shown: Vec<Option<&Line>> = if calls.len() > CALLS_SHOWN {
        let tail = CALLS_SHOWN / 2 - 1;
        let mut shown: Vec<Option<&Line>> = calls[..CALLS_SHOWN / 2].iter().map(Some).collect();
        shown.push(None);
        shown.extend(calls[calls.len() - tail..].iter().map(Some));
        shown
    } else {
        calls.iter().map(Some).collect()
    };
    for call in shown {
        let Some(call) = call else {
            let hidden = &calls[CALLS_SHOWN / 2..calls.len() - (CALLS_SHOWN / 2 - 1)];
            let failed = hidden
                .iter()
                .filter(|line| line.text.starts_with('✗'))
                .count();
            let note = if failed > 0 {
                format!(" · {failed} failed")
            } else {
                String::new()
            };
            rows.push(vec![run(
                format!("  ⋯ {} more calls{note}", hidden.len()),
                DIM,
            )]);
            continue;
        };
        let mut fields = call.text.splitn(4, '\t');
        let mark = fields.next().unwrap_or_default();
        let (ok, running) = (mark != "✗", mark == "…");
        let verb = fields.next().unwrap_or_default();
        let target = fields.next().unwrap_or_default();
        let info = fields.next().unwrap_or_default();
        // One fixed verb column. Subagent calls drop their `agents.` and
        // take the agent colour: violet always means delegated work.
        let (verb, verb_paint) = match verb.strip_prefix("agents.") {
            Some(short) => (short, Paint::fg(NIGHT.agent)),
            None => (
                verb,
                Paint::fg(match verb {
                    "read" | "ls" => 0x82aaff,
                    "write" | "edit" => 0xe8c77d,
                    "sh" => 0x7fcfc4,
                    _ => NIGHT.dim,
                }),
            ),
        };
        let left = vec![
            run("  ", TEXT),
            match (running, ok) {
                (true, _) => run("◌ ", RUNNING),
                (false, true) => run("✓ ", ACCENT),
                (false, false) => run("✗ ", error),
            },
            run(format!("{verb:<VERB$} "), verb_paint),
            run(target, TEXT),
        ];
        let info = clip(info, (width / 3).max(12));
        rows.push(spread(
            left,
            vec![run(
                info,
                match (running, ok) {
                    (true, _) => RUNNING,
                    (false, true) => DIM,
                    (false, false) => error,
                },
            )],
            width,
        ));
    }
    rows
}

/// A subagent, in the program log's grammar so the two read alike:
/// `◆ Title            a3 · 12s`, then its activity on a violet rail
/// (`│ ◌ sh  cargo test   2s` or `│ thinking`). The rail and marker are
/// the only violet: enough to tell delegated work apart at a glance.
/// Line format: `id\ttitle\tstatus\tactivity`, call lines under it.
fn agent_rows(lines: &[Line], width: usize) -> Vec<Row> {
    let violet = Paint::fg(NIGHT.agent);
    let mut fields = lines[0].text.splitn(4, '\t');
    let id = fields.next().unwrap_or_default();
    let title = fields.next().unwrap_or_default();
    let status = fields.next().unwrap_or_default();
    let activity = fields.next().unwrap_or_default();
    let status_paint = if status.contains("failed") {
        Paint::fg(NIGHT.error)
    } else {
        DIM
    };
    let meta = if status.is_empty() {
        id.to_string()
    } else {
        format!("{id} · {status}")
    };
    let mut rows = vec![spread(
        vec![run("◆ ", violet), run(title, TEXT.bold())],
        vec![run(meta, status_paint)],
        width,
    )];
    let rail = || run("│ ", violet);
    if lines.len() > 1 {
        // Its calls are the program log's call rows, the rail in the indent.
        let calls: Vec<Line> = std::iter::once(Line {
            role: Role::Program,
            text: String::new(),
        })
        .chain(lines[1..].iter().cloned())
        .collect();
        for mut row in program_rows(&calls, width, false).into_iter().skip(1) {
            if let Some(first) = row.first_mut() {
                *first = rail();
            }
            rows.push(row);
        }
    } else if !activity.is_empty() {
        rows.push(vec![
            rail(),
            run(clip(activity, width.saturating_sub(2)), THOUGHT),
        ]);
    }
    rows
}

/// The cyan of images: their marker and size, everywhere.
pub const IMAGE_INK: Paint = Paint::fg(0x7fcfe8);

/// An image line laid out: its preview (half blocks, or reserved cells
/// while it is made) under a caption, indented like the rest of the turn.
pub fn image_rows(text: &str, width: usize) -> Laid {
    let mut laid = Laid {
        rows: vec![image_row(text, width)],
        ..Laid::default()
    };
    let fields: Vec<&str> = text.split('\t').collect();
    let (Some(size), Some(path)) = (fields.get(1), fields.get(3)) else {
        return laid;
    };
    let Some((w, h)) = size
        .split_once('×')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
    else {
        return laid;
    };
    let path = std::path::Path::new(path);
    if !path.exists() {
        return laid;
    }
    // Under the caption's name, past its "  ▣ " marker.
    let indent = 4;
    let (cols, rows) = crate::thumbs::size(w, h, width.saturating_sub(indent + 1));
    let cells = match crate::thumbs::get(path, cols, rows) {
        Some(thumb) => {
            laid.images.push(Picture {
                row: 1,
                col: indent,
                cols,
                rows,
                png: thumb.png.clone(),
            });
            thumb.rows.clone()
        }
        None => crate::thumbs::pending(cols, rows),
    };
    for cells in cells {
        let mut row = vec![run(" ".repeat(indent), TEXT)];
        row.extend(cells);
        laid.rows.push(row);
    }
    laid
}

/// `▣ shot.png  1280×720 · ~1.2k tokens` from an image line
/// (`name\tWxH\tmime\tpath`).
pub fn image_row(text: &str, width: usize) -> Row {
    let mut fields = text.split('\t');
    let name = fields.next().unwrap_or_default();
    let size = fields.next().unwrap_or_default();
    let name = std::path::Path::new(name)
        .file_name()
        .map_or_else(|| name.to_string(), |n| n.to_string_lossy().into_owned());
    let tokens = size
        .split_once('×')
        .and_then(|(w, h)| Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()? / 750))
        .map(|t| {
            if t >= 1000 {
                format!(" · ~{:.1}k tokens", t as f64 / 1000.0)
            } else {
                format!(" · ~{t} tokens")
            }
        })
        .unwrap_or_default();
    spread(
        vec![run("  ▣ ", IMAGE_INK), run(name, TEXT)],
        vec![run(format!("{size}{tokens}"), DIM)],
        width,
    )
}

/// `left` then `right` flush right on one row of `width`; the last run of
/// `left` is clipped with `…` to make room.
fn spread(mut left: Row, right: Row, width: usize) -> Row {
    let right_width = self::width(&right);
    let room = width.saturating_sub(right_width + 2);
    let fixed: usize = left.iter().rev().skip(1).map(|run| run.text.width()).sum();
    if let Some(last) = left.last_mut()
        && fixed + last.text.width() > room
    {
        let keep = room.saturating_sub(fixed + 1);
        last.text = format!("{}…", clip(&last.text, keep));
    }
    let used = self::width(&left);
    left.push(run(
        " ".repeat(width.saturating_sub(used + right_width)),
        TEXT,
    ));
    left.extend(right);
    left
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
pub fn input(
    editor: &Editor,
    placeholder: &str,
    width: usize,
    focused: bool,
) -> (Vec<InputRow>, usize) {
    let text = editor.text();
    // Unfocused (a popup is open): no caret, no selection highlight.
    let caret_paint = if focused { CARET } else { DIM };
    if text.is_empty() {
        let mut rest = placeholder.chars();
        let first = rest.next().map_or(" ".into(), String::from);
        let row = vec![
            run("> ", ACCENT),
            run(first, caret_paint),
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
    let selection = editor.selection().filter(|_| focused);
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
                let paint = if focused && r == caret && at == cursor {
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
            if focused && r == caret && cursor == end {
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
/// boundaries so a hint is shown whole or not at all. Also returns the column
/// range of every shown left item and of `right`, for click zones.
pub fn status_row(
    left: &str,
    left_paint: Paint,
    right: &str,
    width: usize,
) -> (Row, Vec<(Range<usize>, String)>, Range<usize>) {
    let right = clip(right, width);
    let room = width.saturating_sub(right.width() + 2);
    let mut shown = String::new();
    let mut items = Vec::new();
    for item in left.split(" · ") {
        let start = if shown.is_empty() {
            0
        } else {
            shown.width() + 3
        };
        let next = if shown.is_empty() {
            item.to_string()
        } else {
            format!("{shown} · {item}")
        };
        if next.width() > room {
            break;
        }
        items.push((start..start + item.width(), item.to_string()));
        shown = next;
    }
    let gap = width.saturating_sub(shown.width() + right.width());
    let right_at = shown.width() + gap;
    let span = right_at..right_at + right.width();
    let row = vec![
        run(shown, left_paint),
        run(" ".repeat(gap), DIM),
        run(right, DIM),
    ];
    (row, items, span)
}

/// Thumb length and first thumb row for `rows` visible of `total`, top at `top`.
fn thumb(total: usize, rows: usize, top: usize) -> (usize, usize) {
    // At least 2 rows: a 1-cell thumb is too easy to lose and to grab.
    let len = (rows * rows).div_ceil(total).clamp(2.min(rows), rows);
    let room = rows - len;
    let max_top = total - rows;
    (len, (top.min(max_top) * room + max_top / 2) / max_top)
}

/// A vertical scrollbar as geometry, in screen cells. Each client draws it
/// its own way (terminal glyphs, gpui rounded thumb); size and position are
/// decided here once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    pub col: usize,
    /// Screen rows the track covers.
    pub rows: Range<usize>,
    /// Screen rows the thumb covers, inside `rows`.
    pub thumb: Range<usize>,
    /// Which bar it is ([`TRANSCRIPT_BAR`], [`PROMPT_BAR`], [`LIST_BAR`]), so a
    /// client can say which one the pointer is over.
    pub id: usize,
    /// 0 collapsed (a thin thumb, no track) … 1 expanded (hovered or
    /// dragged), animated in between. Stored ×1000 to keep `Eq`.
    pub open: u16,
}

pub const TRANSCRIPT_BAR: usize = 0;
pub const PROMPT_BAR: usize = 1;
pub const LIST_BAR: usize = 2;

impl Bar {
    /// How open it is, 0..1.
    pub fn openness(&self) -> f32 {
        f32::from(self.open) / 1000.0
    }

    pub fn with(mut self, id: usize) -> Self {
        self.id = id;
        self
    }
}

/// Scrollbar for `rows` visible of `total` with `top` the first visible,
/// drawn at screen row `first`. None when everything fits.
pub fn bar(col: usize, first: usize, rows: usize, total: usize, top: usize) -> Option<Bar> {
    if rows == 0 || total <= rows {
        return None;
    }
    let (len, at) = thumb(total, rows, top);
    Some(Bar {
        col,
        rows: first..first + rows,
        thumb: first + at..first + at + len,
        id: TRANSCRIPT_BAR,
        open: 0,
    })
}

/// The `top` that puts the thumb's middle on `row` (scrollbar click/drag).
pub fn scroll_top_at(total: usize, rows: usize, row: usize) -> usize {
    if rows == 0 || total <= rows {
        return 0;
    }
    let (len, _) = thumb(total, rows, 0);
    let room = rows - len;
    if room == 0 {
        return 0;
    }
    let at = row.saturating_sub(len / 2).min(room);
    (at * (total - rows) + room / 2) / room
}

/// The cells of `row` in columns `cols`, styles kept.
pub fn slice(row: &[Run], cols: Range<usize>) -> Row {
    let mut out = Row::new();
    let mut col = 0;
    for part in row {
        let mut text = String::new();
        for c in part.text.chars() {
            let w = c.width().unwrap_or(0);
            if col >= cols.start && col + w <= cols.end {
                text.push(c);
            }
            col += w;
        }
        if !text.is_empty() {
            out.push(run(text, part.paint));
        }
    }
    out
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
        for row in transcript_rows(&body, 20, None, Fold::all()).rows {
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
        let (rows, caret) = input(&editor, "", 12, true);
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
            input(&editor, "", 12, true).1,
            1,
            "caret at a soft wrap sits on the next row"
        );
    }

    #[test]
    fn status_drops_whole_hints() {
        let (row, items, right) = status_row("ctrl+p sessions · ctrl+n new", DIM, "s1", 22);
        let text: String = row.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(text, "ctrl+p sessions     s1");
        assert_eq!(items, [(0..15, "ctrl+p sessions".to_string())]);
        assert_eq!(right, 20..22);
        let (_, items, _) = status_row("a · bb · c", DIM, "x", 40);
        assert_eq!(
            items.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>(),
            [0..1, 4..6, 9..10]
        );
    }

    #[test]
    fn scrollbar_tracks_position() {
        assert_eq!(bar(9, 0, 10, 5, 0), None, "fits: no bar");
        let thumb = |top| bar(9, 3, 10, 100, top).map(|bar| bar.thumb);
        assert_eq!(thumb(0), Some(3..5), "offset by the first screen row");
        assert_eq!(thumb(90), Some(11..13));
        assert_eq!(thumb(45), Some(7..9));
        assert_eq!(scroll_top_at(100, 10, 0), 0);
        assert_eq!(scroll_top_at(100, 10, 9), 90);
    }

    #[test]
    fn slice_keeps_styles() {
        let row = vec![run("ab", TEXT), run("cd", DIM)];
        assert_eq!(slice(&row, 1..3), [run("b", TEXT), run("c", DIM)]);
    }
}
