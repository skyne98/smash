//! Markdown to styled lines.
//!
//! Structure is resolved here once (indent, list markers, quotes, tables) and
//! each frontend maps [`Ink`] to its own style, so gpui and the terminal paint
//! the same document. Parsing a 100 KB answer costs well under a millisecond;
//! frontends still cache the result and re-render only when the text changes.

use crate::highlight::{Token, highlight};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Plain,
    Bold,
    Italic,
    Strike,
    /// Inline `code`.
    Code,
    /// A highlighted token inside a fenced code block.
    Syntax(Token),
    /// Heading level 1..=6; frontends make each level visibly different.
    Heading(u8),
    Dim,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub ink: Ink,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub indent: usize,
    pub spans: Vec<Span>,
    /// True for code-block rows. A block's rows are: a label row (one Dim
    /// span: the language), then the code lines.
    pub code: bool,
}

impl Block {
    /// Empty, or only blockquote bars.
    pub fn is_blank(&self) -> bool {
        !self.code
            && self
                .spans
                .iter()
                .all(|span| span.ink == Ink::Dim && span.text == "│ ")
    }
}

pub fn markdown(source: &str) -> Vec<Block> {
    render(source, 48)
}

/// `width` sizes horizontal rules.
pub fn render(source: &str, width: usize) -> Vec<Block> {
    let mut state = State::default();
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    );

    for event in parser {
        state.event(event, width);
    }
    state.flush();
    while state
        .blocks
        .last()
        .is_some_and(|block| block.spans.is_empty())
    {
        state.blocks.pop();
    }
    state.blocks
}

#[derive(Default)]
struct State {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    indent: usize,
    lists: Vec<List>,
    quote: usize,
    bold: usize,
    italic: usize,
    strike: usize,
    code: bool,
    link: Option<String>,
    cells: Vec<String>,
    /// Whole table, buffered: columns can only be aligned once every row is known.
    rows: Vec<Vec<String>>,
    aligns: Vec<Alignment>,
    in_table: bool,
    /// Level of the heading being parsed, 0 outside headings.
    heading: u8,
    /// Fenced code is buffered whole: highlighting needs the full block.
    lang: String,
    code_text: String,
}

enum List {
    Bullet,
    Ordered(u64),
}

impl State {
    fn event(&mut self, event: Event<'_>, width: usize) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag, width),
            Event::Text(text) if self.code => self.code_text.push_str(&text),
            Event::Text(text) => {
                let ink = if self.heading > 0 {
                    Ink::Heading(self.heading)
                } else if self.bold > 0 {
                    Ink::Bold
                } else if self.strike > 0 {
                    Ink::Strike
                } else if self.italic > 0 {
                    Ink::Italic
                } else {
                    Ink::Plain
                };
                for (index, line) in text.split('\n').enumerate() {
                    if index > 0 {
                        self.flush();
                    }
                    if !line.is_empty() {
                        self.push(ink, line);
                    }
                }
            }
            Event::Code(text) => self.push(Ink::Code, text.as_ref()),
            Event::SoftBreak => self.push(Ink::Plain, " "),
            Event::HardBreak => self.flush(),
            Event::TaskListMarker(done) => self.push(Ink::Dim, if done { "[x] " } else { "[ ] " }),
            Event::Rule => {
                self.flush();
                let indent = self.indent + self.quote * 2;
                self.blocks.push(Block {
                    indent,
                    spans: vec![Span {
                        ink: Ink::Dim,
                        text: "─".repeat(width.max(8)),
                    }],
                    code: false,
                });
            }
            // Raw HTML is dropped rather than shown as tag soup.
            Event::Html(_) | Event::InlineHtml(_) => {}
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::CodeBlock(kind) => {
                self.flush();
                self.blank();
                self.code = true;
                // ```rust,ignore or ```py title=x: the first word names the language.
                self.lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
            }
            Tag::List(start) => {
                self.flush();
                self.blank();
                self.lists.push(match start {
                    Some(index) => List::Ordered(index),
                    None => List::Bullet,
                });
                self.indent = self.lists.len() * 2;
            }
            Tag::Item => {
                self.flush();
                match self.lists.last_mut() {
                    Some(List::Ordered(index)) => {
                        let marker = format!("{index}. ");
                        *index += 1;
                        self.push(Ink::Dim, &marker);
                    }
                    _ => self.push(Ink::Dim, "• "),
                }
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.blank();
                self.quote += 1;
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                self.heading = level as u8;
            }
            Tag::Link { dest_url, .. } => self.link = Some(dest_url.to_string()),
            Tag::Table(aligns) => {
                self.flush();
                self.blank();
                self.in_table = true;
                self.aligns = aligns;
                self.rows.clear();
            }
            Tag::TableRow => self.cells.clear(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd, width: usize) {
        match tag {
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::CodeBlock => {
                let lang = std::mem::take(&mut self.lang);
                let code = std::mem::take(&mut self.code_text);
                self.push(Ink::Dim, if lang.is_empty() { "code" } else { &lang });
                self.flush();
                for line in highlight(&lang, &code) {
                    for (token, text) in line {
                        self.push(Ink::Syntax(token), &text);
                    }
                    self.flush();
                }
                self.code = false;
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.indent = self.lists.len() * 2;
                self.blank();
            }
            TagEnd::Item => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                // A quote never starts or ends on a bar-only row.
                while self
                    .blocks
                    .last()
                    .is_some_and(|block| !block.spans.is_empty() && block.is_blank())
                {
                    self.blocks.pop();
                }
                self.quote = self.quote.saturating_sub(1);
                self.blank();
            }
            TagEnd::Heading(level) => {
                let title: usize = self.spans.iter().map(|span| span.text.width()).sum();
                self.flush();
                self.heading = 0;
                if level as u8 == 1 {
                    let spans = self.quoted(vec![Span {
                        ink: Ink::Dim,
                        text: "━".repeat(title.clamp(3, width.max(3))),
                    }]);
                    self.blocks.push(Block {
                        indent: self.indent,
                        spans,
                        code: false,
                    });
                }
                // Big headings get air below; small ones hug their section.
                if (level as u8) <= 2 {
                    self.blank();
                }
            }
            TagEnd::Paragraph => self.flush(),
            TagEnd::Link => {
                if let Some(url) = self.link.take() {
                    let text: String = self.spans.iter().map(|span| span.text.as_str()).collect();
                    if !url.starts_with('#') && url != text {
                        self.push(Ink::Dim, &format!(" ({url})"));
                    }
                }
            }
            TagEnd::TableCell => {
                let text: String = self.spans.iter().map(|span| span.text.as_str()).collect();
                self.cells.push(text.trim().to_string());
                self.spans.clear();
            }
            TagEnd::TableRow | TagEnd::TableHead => {
                if !self.cells.is_empty() {
                    self.rows.push(std::mem::take(&mut self.cells));
                }
            }
            TagEnd::Table => {
                self.in_table = false;
                self.table_blocks(width);
                self.blank();
            }
            _ => {}
        }
    }

    /// Emit the buffered table: padded columns, bold header, one rule under it.
    /// Too wide for the pane: the widest column shrinks and cells end in `…`.
    fn table_blocks(&mut self, width: usize) {
        let rows = std::mem::take(&mut self.rows);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let mut widths = vec![0; columns];
        for row in &rows {
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column].max(cell.width());
            }
        }
        let room = width.saturating_sub(self.indent + 2 * self.quote + 3 * (columns - 1));
        // ponytail: shrinks one column at a time, fine for any table a chat holds.
        while widths.iter().sum::<usize>() > room {
            let widest = (0..columns)
                .max_by_key(|&column| widths[column])
                .unwrap_or(0);
            if widths[widest] <= 3 {
                break;
            }
            widths[widest] -= 1;
        }
        let dim = |text: &str| Span {
            ink: Ink::Dim,
            text: text.into(),
        };
        for (index, row) in rows.iter().enumerate() {
            let mut spans = Vec::with_capacity(columns * 2);
            for (column, &cell_width) in widths.iter().enumerate() {
                if column > 0 {
                    spans.push(dim(" │ "));
                }
                let cell = fit(row.get(column).map_or("", String::as_str), cell_width);
                let gap = " ".repeat(cell_width.saturating_sub(cell.width()));
                let right = self.aligns.get(column) == Some(&Alignment::Right);
                let text = if right {
                    format!("{gap}{cell}")
                } else {
                    format!("{cell}{gap}")
                };
                let ink = if index == 0 { Ink::Bold } else { Ink::Plain };
                spans.push(Span { ink, text });
            }
            let spans = self.quoted(spans);
            self.blocks.push(Block {
                indent: self.indent,
                spans,
                code: false,
            });
            if index == 0 {
                let rule = widths
                    .iter()
                    .map(|&w| "─".repeat(w))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                let spans = self.quoted(vec![dim(&rule)]);
                self.blocks.push(Block {
                    indent: self.indent,
                    spans,
                    code: false,
                });
            }
        }
    }

    /// Append text to the current line, merging with a same-ink neighbour.
    fn push(&mut self, ink: Ink, text: &str) {
        match self.spans.last_mut() {
            Some(last) if last.ink == ink => last.text.push_str(text),
            _ => self.spans.push(Span {
                ink,
                text: text.to_string(),
            }),
        }
    }

    /// One empty line between blocks: never leading, never two in a row.
    /// Inside a quote the blank row keeps its bars, so the quote reads as one.
    fn blank(&mut self) {
        if self.blocks.last().is_none_or(Block::is_blank) {
            return;
        }
        let spans = self.quoted(Vec::new());
        self.blocks.push(Block {
            indent: self.indent,
            spans,
            code: false,
        });
    }

    fn flush(&mut self) {
        // Code rows survive empty: blank lines inside code are content.
        if self.spans.is_empty() && !self.code {
            return;
        }
        let code = self.code;
        let spans = std::mem::take(&mut self.spans);
        let spans = self.quoted(spans);
        self.blocks.push(Block {
            indent: self.indent,
            spans,
            code,
        });
    }

    /// Prefix one `│ ` bar per open blockquote level.
    fn quoted(&self, spans: Vec<Span>) -> Vec<Span> {
        if self.quote == 0 {
            return spans;
        }
        let bar = Span {
            ink: Ink::Dim,
            text: "│ ".into(),
        };
        let mut out = vec![bar; self.quote];
        out.extend(spans);
        out
    }

    #[allow(dead_code)]
    fn table(&self) -> bool {
        self.in_table
    }
}

/// Source of the last fenced or indented code block in `markdown`.
pub fn last_code_block(markdown: &str) -> Option<String> {
    let mut last = None;
    let mut current: Option<String> = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => current = Some(String::new()),
            Event::Text(text) => {
                if let Some(code) = current.as_mut() {
                    code.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => last = current.take(),
            _ => {}
        }
    }
    last.map(|code| code.trim_end_matches('\n').to_string())
}

/// Cut `text` to `width` columns, ending in `…` when it had to cut.
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_block_rows() {
        let blocks = super::render("```rust\nfn a() {}\n\nlet b = 1;\n```\n", 80);
        let code: Vec<String> = blocks
            .iter()
            .filter(|block| block.code)
            .map(|block| block.spans.iter().map(|span| span.text.as_str()).collect())
            .collect();
        assert_eq!(
            code,
            ["rust", "fn a() {}", "", "let b = 1;"],
            "label, lines, no phantom row"
        );
        assert_eq!(
            super::last_code_block("x\n```\none\n```\n```py\ntwo\n```").as_deref(),
            Some("two")
        );
    }

    #[test]
    fn table_columns_align() {
        let lines: Vec<String> = super::render(
            "| ID | Name |\n|---:|---|\n| 1 | Alice |\n| 10 | Bo |\n",
            80,
        )
        .into_iter()
        .filter(|block| !block.spans.is_empty())
        .map(|block| block.spans.into_iter().map(|span| span.text).collect())
        .collect();
        assert_eq!(
            lines,
            ["ID │ Name ", "───┼──────", " 1 │ Alice", "10 │ Bo   "]
        );
    }

    use super::*;

    fn text(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .map(|block| {
                block
                    .spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn structure_survives_markdown() {
        let source = r#"# Title

Some **bold**, *italic*, ~~gone~~ and `code`.

1. first
2. second
   - nested

> quoted line

| a | b |
|---|---|
| 1 | 2 |

```rust
let x = 1;
```

[link](https://example.com)
"#;
        let blocks = markdown(source);
        let lines = text(&blocks);
        assert!(
            lines.iter().any(|line| line == "Title"),
            "heading text is clean"
        );
        assert!(
            blocks
                .iter()
                .any(|block| block.spans.iter().any(|span| span.ink == Ink::Heading(1)))
        );
        assert!(lines.iter().any(|line| line.contains("1. ")));
        assert!(
            lines.iter().any(|line| line == "• nested"),
            "nested bullet keeps its marker"
        );
        assert!(lines.iter().any(|line| line.contains("│ quoted line")));
        assert!(lines.iter().any(|line| line.contains("a │ b")));
        assert!(lines.iter().any(|line| line == "let x = 1;"));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("(https://example.com)"))
        );
        let nested = blocks
            .iter()
            .find(|block| {
                block
                    .spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
                    .contains("nested")
            })
            .expect("nested bullet");
        assert_eq!(nested.indent, 4);
    }
}
