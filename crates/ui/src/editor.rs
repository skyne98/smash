//! The prompt's text field: caret, selection, word motions, undo. Pure state;
//! both clients map their keys and clicks onto these methods, so editing
//! behaves like a browser textarea everywhere.

use std::ops::Range;

#[derive(Default)]
pub struct Editor {
    text: String,
    /// Byte offset of the caret, always on a char boundary.
    cursor: usize,
    /// Other end of the selection while one exists (shift / drag).
    anchor: Option<usize>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// Last edit was typing a word character: the next one joins its undo step.
    typing: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Selected byte range, if any characters are selected.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub fn selected(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }

    fn prev(&self, at: usize) -> usize {
        self.text[..at]
            .chars()
            .next_back()
            .map_or(0, |c| at - c.len_utf8())
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8())
    }

    /// Start of the word left of `at`: skip spaces, then one run of a class.
    fn word_left(&self, mut at: usize) -> usize {
        let mut run = None;
        while let Some(c) = self.text[..at].chars().next_back() {
            let kind = class(c);
            match run {
                None if kind == Class::Space => {}
                None => run = Some(kind),
                Some(current) if current != kind => break,
                Some(_) => {}
            }
            at -= c.len_utf8();
        }
        at
    }

    fn word_right(&self, mut at: usize) -> usize {
        let mut run = None;
        while let Some(c) = self.text[at..].chars().next() {
            let kind = class(c);
            match run {
                None if kind == Class::Space => {}
                None => run = Some(kind),
                Some(current) if current != kind => break,
                Some(_) => {}
            }
            at += c.len_utf8();
        }
        at
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |n| n + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |n| at + n)
    }

    /// Byte offset of char column `col` in `start..end`, clamped to `end`.
    fn column(&self, start: usize, end: usize, col: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(at, _)| start + at)
    }

    /// Put the caret at `pos`; `extend` (shift held) keeps or starts a selection.
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        let mut pos = pos.min(self.text.len());
        while !self.text.is_char_boundary(pos) {
            pos -= 1;
        }
        self.cursor = pos;
        self.typing = false;
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        // Without shift a selection collapses to its start, like the web.
        if !extend && let Some(range) = self.selection() {
            return self.move_to(range.start, false);
        }
        let to = if word {
            self.word_left(self.cursor)
        } else {
            self.prev(self.cursor)
        };
        self.move_to(to, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        if !extend && let Some(range) = self.selection() {
            return self.move_to(range.end, false);
        }
        let to = if word {
            self.word_right(self.cursor)
        } else {
            self.next(self.cursor)
        };
        self.move_to(to, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.move_to(self.line_start(self.cursor), extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_to(self.line_end(self.cursor), extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        self.move_to(self.text.len(), extend);
    }

    /// One line up. False when already on the first line (caller may scroll).
    pub fn up(&mut self, extend: bool) -> bool {
        let start = self.line_start(self.cursor);
        if start == 0 {
            return false;
        }
        let col = self.text[start..self.cursor].chars().count();
        let above = self.line_start(start - 1);
        self.move_to(self.column(above, start - 1, col), extend);
        true
    }

    /// One line down. False when already on the last line.
    pub fn down(&mut self, extend: bool) -> bool {
        let end = self.line_end(self.cursor);
        if end == self.text.len() {
            return false;
        }
        let col = self.text[self.line_start(self.cursor)..self.cursor]
            .chars()
            .count();
        let below = end + 1;
        self.move_to(self.column(below, self.line_end(below), col), extend);
        true
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Double click: the run of same-class characters under `at`.
    pub fn select_word_at(&mut self, at: usize) {
        let at = at.min(self.text.len());
        let Some(c) = self.text[at..]
            .chars()
            .next()
            .or_else(|| self.text[..at].chars().next_back())
        else {
            return;
        };
        let kind = class(c);
        let mut start = at;
        while let Some(c) = self.text[..start].chars().next_back()
            && class(c) == kind
        {
            start -= c.len_utf8();
        }
        let mut end = at;
        while let Some(c) = self.text[end..].chars().next()
            && class(c) == kind
        {
            end += c.len_utf8();
        }
        self.anchor = Some(start);
        self.cursor = end;
    }

    /// Triple click: the whole line under `at`.
    pub fn select_line_at(&mut self, at: usize) {
        let at = at.min(self.text.len());
        self.anchor = Some(self.line_start(at));
        self.cursor = self.line_end(at);
    }

    fn checkpoint(&mut self, typing: bool) {
        if !(typing && self.typing) {
            self.undo.push((self.text.clone(), self.cursor));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.typing = typing;
    }

    fn replace(&mut self, range: Range<usize>, with: &str) {
        self.text.replace_range(range.clone(), with);
        self.cursor = range.start + with.len();
        self.anchor = None;
    }

    /// Type or paste: replaces the selection, if any.
    pub fn insert(&mut self, text: &str) {
        let typing = self.selection().is_none() && text.chars().all(|c| class(c) == Class::Word);
        self.checkpoint(typing);
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.replace(range, text);
    }

    pub fn backspace(&mut self, word: bool) {
        let range = match self.selection() {
            Some(range) => range,
            None => {
                let from = if word {
                    self.word_left(self.cursor)
                } else {
                    self.prev(self.cursor)
                };
                from..self.cursor
            }
        };
        if !range.is_empty() {
            self.checkpoint(false);
            self.replace(range, "");
        }
    }

    pub fn delete(&mut self, word: bool) {
        let range = match self.selection() {
            Some(range) => range,
            None => {
                let to = if word {
                    self.word_right(self.cursor)
                } else {
                    self.next(self.cursor)
                };
                self.cursor..to
            }
        };
        if !range.is_empty() {
            self.checkpoint(false);
            self.replace(range, "");
        }
    }

    /// Remove and return the selection.
    pub fn cut(&mut self) -> Option<String> {
        let range = self.selection()?;
        let text = self.text[range.clone()].to_string();
        self.checkpoint(false);
        self.replace(range, "");
        Some(text)
    }

    /// Esc: drop the selection first, then the text (undoable).
    pub fn escape(&mut self) {
        if self.selection().is_some() {
            self.anchor = None;
        } else if !self.text.is_empty() {
            self.checkpoint(false);
            self.replace(0..self.text.len(), "");
        }
    }

    pub fn undo(&mut self) {
        if let Some((text, cursor)) = self.undo.pop() {
            self.redo
                .push((std::mem::replace(&mut self.text, text), self.cursor));
            self.cursor = cursor;
            self.anchor = None;
            self.typing = false;
        }
    }

    pub fn redo(&mut self) {
        if let Some((text, cursor)) = self.redo.pop() {
            self.undo
                .push((std::mem::replace(&mut self.text, text), self.cursor));
            self.cursor = cursor;
            self.anchor = None;
            self.typing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;

    fn typed(text: &str) -> Editor {
        let mut editor = Editor::default();
        editor.insert(text);
        editor
    }

    #[test]
    fn words_selection_and_replace() {
        let mut e = typed("let foo = bar.baz");
        e.left(true, false);
        assert_eq!(&e.text()[e.cursor()..], "baz");
        e.left(true, true);
        e.left(true, true);
        assert_eq!(
            e.selected(),
            Some("bar."),
            "word motion stops at punctuation"
        );
        e.insert("x");
        assert_eq!(e.text(), "let foo = xbaz");
        e.backspace(true);
        assert_eq!(e.text(), "let foo = baz");
        e.undo();
        e.undo();
        assert_eq!(e.text(), "let foo = bar.baz");
        e.redo();
        assert_eq!(e.text(), "let foo = xbaz");
    }

    #[test]
    fn typing_coalesces_undo() {
        let mut e = Editor::default();
        for c in ["h", "e", "y", " ", "y", "o"] {
            e.insert(c);
        }
        e.undo();
        assert_eq!(e.text(), "hey ");
        e.undo();
        assert_eq!(e.text(), "hey");
    }

    #[test]
    fn lines_and_clicks() {
        let mut e = typed("ab\ncdef\ng");
        e.move_to(5, false);
        assert!(e.up(false));
        assert_eq!(e.cursor(), 2, "column clamps to the shorter line");
        assert!(!e.up(false), "first line: caller scrolls instead");
        assert!(e.down(true) && e.down(true));
        assert_eq!(e.selected(), Some("\ncdef\ng"));
        e.select_word_at(4);
        assert_eq!(e.selected(), Some("cdef"));
        e.select_line_at(0);
        assert_eq!(e.selected(), Some("ab"));
        e.move_to(1, false);
        e.end(true);
        e.left(false, false);
        assert_eq!(e.cursor(), 1, "left collapses a selection to its start");
    }

    #[test]
    fn multibyte_is_safe() {
        let mut e = typed("héllo ✓");
        e.backspace(false);
        e.left(false, false);
        e.left(true, false);
        e.delete(true);
        assert_eq!(e.text(), " ");
        e.move_to(99, false);
        assert_eq!(e.cursor(), 1);
    }
}
