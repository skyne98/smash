pub mod window;
pub mod prelude;
pub mod terminal;
pub mod textbox;
#[cfg(test)]
mod tests;

pub use window::Window;
pub use prelude::*;

// Re-export common dependencies for user convenience
pub use ratatui;
pub use crossterm;
pub use tachyonfx;
pub use throbber_widgets_tui;
pub use tui_big_text;
pub use tui_piechart;
pub use tui_term;
pub use tui_scrollview;
pub use portable_pty;
pub use arboard;
