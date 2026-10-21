//! Everything the gpui and terminal clients share: the chat controller
//! ([`chat::Chat`]), the character-grid layout ([`layout`]), markdown, syntax
//! highlighting, theme, hints. A client only translates keys in and paints
//! rows out, so a behaviour change happens once here instead of twice.

pub mod chat;
pub mod editor;
pub mod highlight;
pub mod layout;
pub mod markdown;
pub mod motion;
pub mod popup;

use std::time::{Duration, Instant};

// ------------------------------------------------------------------ theme

/// One palette for every frontend. `0xRRGGBB`, so gpui's `rgb()` takes it
/// directly and the terminal splits it into bytes.
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: u32,
    /// Slightly lifted background: the input bar and popups.
    pub surface: u32,
    pub text: u32,
    pub dim: u32,
    pub accent: u32,
    /// Inline code.
    pub code: u32,
    /// Line numbers, separators: present but quiet.
    pub faint: u32,
    /// Selected text background.
    pub select: u32,
    /// Failures: red glyphs, error lines.
    pub error: u32,
    /// Subagents: their rail, badge and marker, so delegated work never
    /// reads like the main answer.
    pub agent: u32,
}

pub const NIGHT: Theme = Theme {
    bg: 0x11121a,
    surface: 0x262838,
    text: 0xe6e6ea,
    dim: 0x9b9da4,
    accent: 0x7ec48c,
    code: 0xd6ba82,
    faint: 0x5c6070,
    agent: 0xb48ef0,
    select: 0x3b4766,
    error: 0xf07882,
};

// ------------------------------------------------------------------ hints

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Chat,
    Picker,
}

/// The bottom line: only the keys that do something *right now*.
pub fn hints(mode: Mode, typing: bool, generating: bool) -> &'static str {
    match mode {
        Mode::Picker => "enter open · ↑/↓ move · esc close",
        Mode::Chat if generating && typing => "enter steer · shift+enter newline · esc clear",
        Mode::Chat if generating => "type to steer · esc stop · ctrl+o thinking",
        Mode::Chat if typing => "enter send · shift+enter newline · esc clear",
        Mode::Chat => "ctrl+p sessions · ctrl+l model · ctrl+n new · ctrl+q quit",
    }
}

// ----------------------------------------------------------------- spinner

/// Rotating status words while the model works; changes every ~2s so the line
/// is visibly alive without being noisy.
pub const SPINNER: [&str; 6] = [
    "Thinking",
    "Pondering",
    "Considering",
    "Percolating",
    "Reticulating",
    "Mulling",
];

pub fn spinner_word(elapsed_secs: f64) -> &'static str {
    SPINNER[(elapsed_secs / 2.0) as usize % SPINNER.len()]
}

// ------------------------------------------------------------------ clock

/// Concrete numbers instead of a feeling: how long this turn has been running
/// and how long the last one actually took.
#[derive(Default)]
pub struct Clock {
    start: Option<Instant>,
    last: Option<Duration>,
}

impl Clock {
    pub fn start(&mut self) {
        self.start = Some(Instant::now());
    }

    pub fn finish(&mut self) -> Option<Duration> {
        let took = self.start.take()?.elapsed();
        self.last = Some(took);
        Some(took)
    }

    pub fn generating(&self) -> bool {
        self.start.is_some()
    }

    pub fn running_secs(&self) -> Option<f64> {
        self.start.map(|start| start.elapsed().as_secs_f64())
    }

    pub fn last_secs(&self) -> Option<f64> {
        self.last.map(|last| last.as_secs_f64())
    }

    /// `running 3s · last 1.3s`, `last 1.3s`, or empty.
    pub fn label(&self) -> String {
        match (self.running_secs(), self.last_secs()) {
            (Some(running), Some(last)) => format!("{running:.0}s · last {last:.1}s"),
            (Some(running), None) => format!("{running:.0}s"),
            (None, Some(last)) => format!("last {last:.1}s"),
            (None, None) => String::new(),
        }
    }
}

// ------------------------------------------------------------- transcript

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    /// Answer text (markdown).
    Assistant,
    /// One line of the model's reasoning.
    Thinking,
    /// A `js` program: `title\tsummary`.
    Program,
    /// One tool call inside the program above: `✓|✗\tverb\ttarget\tinfo`.
    Call,
    /// A run that failed: what happened and what to do.
    Fail,
    /// A subagent: `id\ttask\tstatus`, its calls (`Call` lines) under it.
    Agent,
}

/// Stored-body prefixes of the non-text parts of an answer. Answer text has
/// no prefix; a model writing a line that starts with one of these is rare
/// and only changes how that line is drawn.
pub const THINK: &str = "✻ ";
pub const PROGRAM: &str = "⏵ ";
pub const CALL: &str = "⎿ ";
pub const FAIL: &str = "⚠ ";
pub const AGENT: &str = "◆ ";

fn classify(line: &str) -> Line {
    for (prefix, role) in [
        (THINK, Role::Thinking),
        (PROGRAM, Role::Program),
        (CALL, Role::Call),
        (FAIL, Role::Fail),
        (AGENT, Role::Agent),
    ] {
        if let Some(text) = line.strip_prefix(prefix) {
            return Line {
                role,
                text: text.to_string(),
            };
        }
    }
    Line {
        role: Role::Assistant,
        text: line.to_string(),
    }
}

/// Lines of one answer (no prompts), e.g. a run still streaming.
pub fn answer_lines(text: &str) -> Vec<Line> {
    let body: Vec<String> = text.lines().map(String::from).collect();
    tidy(body.iter().map(|line| classify(line)).collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub role: Role,
    pub text: String,
}

/// Turns the stored body into renderable lines: `> ` marks a user message,
/// runs of blank lines collapse to one, and the trailing blank is dropped.
/// Prefix of every user-prompt line in a document body. Not `> `: that is a
/// markdown blockquote, which answers use too.
pub const USER: &str = "❯ ";

/// Body text for one user prompt: every line marked, so multi-line prompts
/// stay one turn.
pub fn user_turn(prompt: &str) -> String {
    prompt
        .lines()
        .map(|line| format!("{USER}{line}\n"))
        .collect()
}

/// Sessions written before USER marked prompts with `> `, same as a markdown
/// quote. A prompt was always one line, after a blank (answers end in `\n\n`),
/// and the answer followed directly; a quote line rarely fits all three.
fn legacy_prompt(body: &[String], index: usize) -> Option<&str> {
    let text = body[index].strip_prefix("> ")?;
    let after_blank = index == 0 || body[index - 1].trim().is_empty();
    let answer_next = body
        .get(index + 1)
        .is_none_or(|next| !next.trim().is_empty() && !next.starts_with('>'));
    (after_blank && answer_next).then_some(text)
}

pub fn transcript(body: &[String]) -> Vec<Line> {
    let legacy = !body.iter().any(|line| line.starts_with(USER));
    let raw: Vec<Line> = body
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let prompt = if legacy {
                legacy_prompt(body, index)
            } else {
                line.strip_prefix(USER)
            };
            match prompt {
                Some(text) => Line {
                    role: Role::User,
                    text: text.to_string(),
                },
                None => classify(line),
            }
        })
        .collect();
    tidy(raw)
}

fn tidy(raw: Vec<Line>) -> Vec<Line> {
    // A blank line only survives between two assistant lines, where it is a
    // paragraph break inside one answer. Separator blanks between turns are
    // dropped: the `>` and `⏺` markers already separate them.
    let mut lines: Vec<Line> = Vec::with_capacity(raw.len());
    for (index, line) in raw.iter().enumerate() {
        if !line.text.trim().is_empty() {
            lines.push(line.clone());
            continue;
        }
        // Blank: keep it only when assistant text surrounds it on both sides.
        let previous = raw[..index]
            .iter()
            .rev()
            .find(|line| !line.text.trim().is_empty());
        let next = raw[index + 1..]
            .iter()
            .find(|line| !line.text.trim().is_empty());
        let keep = matches!(
            (previous, next),
            (Some(previous), Some(next))
                if previous.role == Role::Assistant && next.role == Role::Assistant
        );
        let already_blank = lines.last().is_some_and(|line| line.text.trim().is_empty());
        if keep && !already_blank {
            lines.push(line.clone());
        }
    }
    lines
}

// ----------------------------------------------------------------- picker

#[derive(Clone)]
pub struct Item {
    pub id: u64,
    pub label: String,
}

/// Keyboard menu with type-to-filter. One instance per menu; the frontend only
/// paints [`Picker::visible`] and forwards keys to the methods below.
#[derive(Default)]
pub struct Picker {
    pub query: String,
    items: Vec<Item>,
    index: usize,
}

impl Picker {
    pub fn set_items(&mut self, items: Vec<Item>) {
        self.items = items;
        self.index = 0;
    }

    /// Change one item's label in place (filter and selection unchanged).
    pub fn relabel(&mut self, id: u64, label: String) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.label = label;
        }
    }

    pub fn query_typed(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn push(&mut self, character: char) {
        self.query.push(character);
        self.index = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.index = 0;
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.index = 0;
    }

    pub fn matches(&self) -> Vec<&Item> {
        let query = self.query.trim().to_lowercase();
        self.items
            .iter()
            .filter(|item| query.is_empty() || item.label.to_lowercase().contains(&query))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.matches().len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches().is_empty()
    }

    pub fn move_by(&mut self, delta: isize) {
        let len = self.matches().len();
        if len == 0 {
            self.index = 0;
            return;
        }
        self.index = (self.index as isize + delta).clamp(0, len as isize - 1) as usize;
    }

    pub fn select(&mut self, index: usize) {
        self.index = index.min(self.len().saturating_sub(1));
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn selected(&self) -> Option<u64> {
        self.matches().get(self.index).map(|item| item.id)
    }

    /// At most `rows` entries, scrolled so the selection stays visible.
    /// Returns absolute index + item so the frontend can highlight correctly.
    pub fn visible(&self, rows: usize) -> Vec<(usize, &Item)> {
        let matched = self.matches();
        if matched.is_empty() || rows == 0 {
            return Vec::new();
        }
        let start = (self.index + 1).saturating_sub(rows);
        matched
            .into_iter()
            .enumerate()
            .skip(start)
            .take(rows)
            .collect()
    }

    /// `3/12` for the menu title, or `0/0`.
    pub fn counter(&self) -> String {
        let total = self.matches().len();
        if total == 0 {
            "0/0".to_string()
        } else {
            format!("{}/{}", self.index + 1, total)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_prompts_vs_quotes() {
        let body: Vec<String> = "> hi\nanswer\n\n> quote\n>\n> more\n\n> next\nok"
            .lines()
            .map(String::from)
            .collect();
        let users: Vec<_> = super::transcript(&body)
            .into_iter()
            .filter(|line| line.role == super::Role::User)
            .map(|line| line.text)
            .collect();
        assert_eq!(users, ["hi", "next"]);
    }

    #[test]
    fn quotes_in_answers_stay_with_the_answer() {
        let body: Vec<String> = format!("{}answer\n> a quote\n", super::user_turn("two\nlines"))
            .lines()
            .map(String::from)
            .collect();
        let roles: Vec<_> = super::transcript(&body)
            .into_iter()
            .map(|line| line.role)
            .collect();
        use super::Role::*;
        assert_eq!(roles, [User, User, Assistant, Assistant]);
    }

    use super::*;

    fn item(id: u64, label: &str) -> Item {
        Item {
            id,
            label: label.to_string(),
        }
    }

    #[test]
    fn type_to_filter_keeps_selection_in_range() {
        let mut picker = Picker::default();
        picker.set_items(vec![
            item(1, "fix login bug"),
            item(2, "add tests"),
            item(3, "login copy"),
        ]);

        assert_eq!(picker.len(), 3);
        picker.move_by(2);
        assert_eq!(picker.selected(), Some(3));

        for character in "log".chars() {
            picker.push(character);
        }
        assert_eq!(picker.len(), 2);
        assert_eq!(picker.selected(), Some(1), "index resets on filter");

        picker.backspace();
        assert_eq!(picker.len(), 2);
        picker.clear();
        assert_eq!(picker.len(), 3);
    }

    #[test]
    fn window_keeps_the_selection_visible() {
        let mut picker = Picker::default();
        picker.set_items((0..12).map(|id| item(id, &format!("s{id}"))).collect());
        picker.move_by(9);
        let visible = picker.visible(5);
        assert_eq!(visible.len(), 5);
        assert!(visible.iter().any(|(index, _)| *index == 9));
        assert_eq!(picker.counter(), "10/12");
    }

    #[test]
    fn transcript_marks_users_and_collapses_blank_runs() {
        let body: Vec<String> = ["", "> hi", "hello", "", "", "again", ""]
            .iter()
            .map(|line| line.to_string())
            .collect();
        let lines = transcript(&body);
        // Leading blanks and blank runs collapse; one separator blank stays.
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].role, Role::User);
        assert_eq!(lines[0].text, "hi");
        assert_eq!(lines[1].text, "hello");
        assert_eq!(lines[2].text, "");
        assert_eq!(lines[3].text, "again");
    }
}
