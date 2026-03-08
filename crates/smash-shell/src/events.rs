use crossterm::event::{KeyEvent, MouseEvent};
use sycamore_reactive::*;
use std::any::Any;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum SmashEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Custom(String, Arc<dyn Any + Send + Sync>),
}

#[derive(Clone, Copy, Default)]
pub enum EventStatus {
    #[default]
    Ignored,   // Continue bubbling
    Handled,   // Stop bubbling
    Consumed,  // Handled and hide from others
}

/// A Vue-like event dispatcher
#[derive(Clone, Copy)]
pub struct Dispatcher {
    pub(crate) events: Signal<Option<SmashEvent>>,
}

pub fn use_dispatcher() -> Dispatcher {
    Dispatcher {
        events: create_signal(None),
    }
}

impl Dispatcher {
    pub fn emit(&self, event: SmashEvent) {
        self.events.set(Some(event));
    }

    /// Listen for a specific event type and execute a callback.
    /// Returns true if handled.
    pub fn on<F>(&self, f: F) -> bool 
    where F: Fn(&SmashEvent) -> EventStatus {
        if let Some(event) = self.events.get_clone() {
            match f(&event) {
                EventStatus::Handled | EventStatus::Consumed => {
                    // In a true fine-grained system, we might want to clear 
                    // or mark the event as handled.
                    return true;
                }
                EventStatus::Ignored => return false,
            }
        }
        false
    }
}
