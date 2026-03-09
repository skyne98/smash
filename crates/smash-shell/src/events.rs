use crossterm::event::{KeyEvent, MouseEvent};
use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
use sycamore_reactive::*;

#[derive(Clone, Debug)]
pub enum SmashEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Custom(String, Arc<dyn Any + Send + Sync>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventStatus {
    #[default]
    Ignored, // Continue bubbling
    Handled, // Stop bubbling
}

/// A local event emitter for components
#[derive(Clone, Default)]
pub struct EventEmitter<T: Clone + 'static> {
    listeners: Signal<Vec<Listener<T>>>,
}

type Listener<T> = Arc<dyn Fn(T)>;

impl<T: Clone + 'static> EventEmitter<T> {
    pub fn new() -> Self {
        Self {
            listeners: create_signal(Vec::new()),
        }
    }

    pub fn subscribe(&self, f: impl Fn(T) + 'static) {
        let mut list = self.listeners.get_clone();
        list.push(Arc::new(f));
        self.listeners.set(list);
    }

    pub fn emit(&self, event: T) {
        for listener in self.listeners.get_clone() {
            listener(event.clone());
        }
    }
}

/// The global dispatcher
#[derive(Clone, Copy)]
pub struct Dispatcher {
    pub(crate) events: Signal<VecDeque<SmashEvent>>,
}

pub fn use_dispatcher() -> Dispatcher {
    Dispatcher {
        events: create_signal(VecDeque::new()),
    }
}

impl Dispatcher {
    pub fn emit(&self, event: SmashEvent) {
        let mut events = self.events.get_clone();
        events.push_back(event);
        self.events.set(events);
    }

    pub fn clear(&self) {
        self.events.set(VecDeque::new());
    }

    pub fn drain(&self) -> Vec<SmashEvent> {
        let events = self.events.get_clone();
        self.clear();
        events.into_iter().collect()
    }

    pub fn dispatch<F>(&self, mut f: F) -> bool
    where
        F: FnMut(&SmashEvent) -> EventStatus,
    {
        let mut handled = false;
        for event in self.drain() {
            if f(&event) == EventStatus::Handled {
                handled = true;
            }
        }
        handled
    }

    pub fn on<F>(&self, f: F) -> bool
    where
        F: FnMut(&SmashEvent) -> EventStatus,
    {
        self.dispatch(f)
    }
}
