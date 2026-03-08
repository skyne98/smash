pub use sycamore_reactive::*;

// Base composables for smash-shell

/// A simple composable for tab management
pub fn use_tabs(initial: usize, _count: usize) -> Signal<usize> {
    let active = create_signal(initial);
    active
}

/// A simple composable for toggle states (like light/dark mode)
pub fn use_toggle(initial: bool) -> Signal<bool> {
    create_signal(initial)
}
