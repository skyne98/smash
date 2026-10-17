//! Shared domain: state machine, actions, effects, view model.
//!
//! No I/O, no rendering, no async. Clients render [`View`] and turn input into
//! [`Action`]; the backend executes [`Effect`] and reports [`Event`]. Everything
//! that is not pixels lives here, so the gpui and terminal clients are thin
//! renderers over the same state.

use serde::{Deserialize, Serialize};

pub type DocId = u64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub id: DocId,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    New,
    Select(DocId),
    Next,
    Prev,
    Rename(String),
    /// Appends text to the selected document's body (chat transcript).
    Append(String),
    Close(DocId),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Load,
    Save(Doc),
    Drop(DocId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    Loaded(Vec<Doc>),
    Saved(DocId),
    Dropped(DocId),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tone {
    #[default]
    Normal,
    Muted,
    Accent,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub id: DocId,
    pub text: String,
    pub tone: Tone,
    pub selected: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub title: String,
    pub rows: Vec<Row>,
    pub status: String,
    pub prompt: Option<String>,
    /// Body of the selected document, one entry per line.
    pub transcript: Vec<String>,
}

/// Extension seam: the client runtime passes every action and every view
/// through this, so a wasm plugin can add commands or decorate the UI without
/// either client knowing it exists.
pub trait Hooks {
    fn action(&mut self, action: Action) -> Option<Action> {
        Some(action)
    }
    fn view(&mut self, _view: &mut View) {}
    /// Every event the runtime applies, in order.
    fn event(&mut self, _event: &Event) {}
}

impl Hooks for () {}

/// Two hooks run in order: the first may rewrite or drop, the second sees the result.
impl<A: Hooks, B: Hooks> Hooks for (A, B) {
    fn action(&mut self, action: Action) -> Option<Action> {
        let action = self.0.action(action)?;
        self.1.action(action)
    }

    fn view(&mut self, view: &mut View) {
        self.0.view(view);
        self.1.view(view);
    }

    fn event(&mut self, event: &Event) {
        self.0.event(event);
        self.1.event(event);
    }
}

#[derive(Clone, Debug, Default)]
pub struct App {
    docs: Vec<Doc>,
    next_id: DocId,
    selected: Option<DocId>,
    status: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Default::default()
        }
    }

    pub fn docs(&self) -> &[Doc] {
        &self.docs
    }

    pub fn selected(&self) -> Option<&Doc> {
        let id = self.selected?;
        self.docs.iter().find(|d| d.id == id)
    }

    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::New => {
                let doc = Doc {
                    id: self.next_id,
                    title: format!("untitled-{}", self.next_id),
                    body: String::new(),
                };
                self.next_id += 1;
                self.selected = Some(doc.id);
                self.docs.push(doc.clone());
                vec![Effect::Save(doc)]
            }
            Action::Select(id) => {
                if self.docs.iter().any(|d| d.id == id) {
                    self.selected = Some(id);
                }
                Vec::new()
            }
            Action::Next => self.step(1),
            Action::Prev => self.step(-1),
            Action::Rename(title) => {
                let Some(id) = self.selected else {
                    return Vec::new();
                };
                let Some(doc) = self.docs.iter_mut().find(|d| d.id == id) else {
                    return Vec::new();
                };
                doc.title = title;
                let saved = doc.clone();
                vec![Effect::Save(saved)]
            }
            Action::Append(text) => {
                let Some(id) = self.selected else {
                    return Vec::new();
                };
                let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else {
                    return Vec::new();
                };
                doc.body.push_str(&text);
                let saved = doc.clone();
                vec![Effect::Save(saved)]
            }
            Action::Close(id) => {
                self.docs.retain(|d| d.id != id);
                if self.selected == Some(id) {
                    self.selected = self.docs.first().map(|d| d.id);
                }
                vec![Effect::Drop(id)]
            }
            Action::Quit => Vec::new(),
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Loaded(docs) => {
                self.next_id = docs.iter().map(|d| d.id).max().unwrap_or(0) + 1;
                self.selected = docs.first().map(|d| d.id);
                self.status = format!("{} docs", docs.len());
                self.docs = docs;
            }
            Event::Saved(id) => self.status = format!("saved {id}"),
            Event::Dropped(id) => self.status = format!("dropped {id}"),
            Event::Failed(err) => self.status = err,
        }
    }

    /// Selection moves in list order, the same for both clients.
    fn step(&mut self, delta: isize) -> Vec<Effect> {
        if self.docs.is_empty() {
            return Vec::new();
        }
        let current = self
            .selected
            .and_then(|id| self.docs.iter().position(|d| d.id == id))
            .unwrap_or(0);
        let len = self.docs.len() as isize;
        let next = (current as isize + delta).rem_euclid(len) as usize;
        self.selected = Some(self.docs[next].id);
        Vec::new()
    }

    pub fn view(&self) -> View {
        View {
            title: "smash".into(),
            rows: self
                .docs
                .iter()
                .map(|d| Row {
                    id: d.id,
                    text: d.title.clone(),
                    tone: if d.body.is_empty() {
                        Tone::Muted
                    } else {
                        Tone::Normal
                    },
                    selected: self.selected == Some(d.id),
                })
                .collect(),
            status: self.status.clone(),
            prompt: None,
            transcript: self
                .selected()
                .map(|doc| doc.body.lines().map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_doc_is_selected_saved_and_rendered() {
        let mut app = App::new();
        assert!(matches!(app.dispatch(Action::New).as_slice(), [Effect::Save(d)] if d.id == 1));
        let view = app.view();
        assert_eq!(view.rows.len(), 1);
        assert!(view.rows[0].selected);

        app.apply(Event::Loaded(vec![Doc {
            id: 7,
            title: "a".into(),
            body: String::new(),
        }]));
        let effects = app.dispatch(Action::New);
        assert!(matches!(effects.as_slice(), [Effect::Save(d)] if d.id == 8));
        assert_eq!(app.view().rows.len(), 2);

        app.dispatch(Action::Append("hello\nworld\n".into()));
        assert_eq!(app.view().transcript, vec!["hello", "world"]);

        app.dispatch(Action::Close(8));
        assert_eq!(app.view().rows.len(), 1);
        assert_eq!(app.selected().unwrap().id, 7);
    }
}
