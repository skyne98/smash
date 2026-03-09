//! Reactive terminal UI primitives inspired by Vue-style composables.
//!
//! The intended flow is:
//! - create component state with `use_*` helpers,
//! - consume queued input with `window.dispatcher.dispatch(...)`,
//! - render components through each state's `render()` method.

pub mod button;
pub mod dialog;
pub mod events;
pub mod prelude;
pub mod reactive;
pub mod terminal;
#[cfg(test)]
mod tests;
pub mod textbox;
pub mod theme;
pub mod window;

pub use prelude::*;
pub use window::Window;

// Re-export common dependencies for user convenience
pub use arboard;
pub use crossterm;
pub use portable_pty;
pub use ratatui;
pub use tachyonfx;
pub use throbber_widgets_tui;
pub use tui_scrollview;
pub use tui_term;
