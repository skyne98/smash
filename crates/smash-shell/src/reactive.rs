use crate::events::{EventStatus, SmashEvent};
use ratatui::layout::Rect;

pub use sycamore_reactive::*;

// Base composables for smash-shell

/// Shared focus helper for interactive components.
#[derive(Clone, Copy)]
pub struct FocusState {
    signal: Signal<bool>,
}

#[derive(Clone, Copy)]
pub struct InteractionState {
    selected: FocusState,
    focused: FocusState,
}

#[derive(Clone, Copy)]
pub struct SelectionState {
    index: Signal<usize>,
    len: Signal<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDirection {
    Next,
    Previous,
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusNode<T: Copy + Eq> {
    pub id: T,
    pub area: Rect,
}

#[derive(Clone, Copy)]
pub struct FocusNavigator<T: Copy + Eq + 'static> {
    selected: Signal<Option<T>>,
}

pub fn use_focus(initial: bool) -> FocusState {
    FocusState {
        signal: create_signal(initial),
    }
}

pub fn use_interaction(initial_selected: bool, initial_focused: bool) -> InteractionState {
    let selected = use_focus(initial_selected || initial_focused);
    let focused = use_focus(initial_focused);
    InteractionState { selected, focused }
}

pub fn use_selection(initial: usize, len: usize) -> SelectionState {
    let len = len.max(1);
    SelectionState {
        index: create_signal(initial.min(len - 1)),
        len: create_signal(len),
    }
}

pub fn use_focus_navigator<T: Copy + Eq + 'static>(initial: Option<T>) -> FocusNavigator<T> {
    FocusNavigator {
        selected: create_signal(initial),
    }
}

impl FocusState {
    pub fn get(self) -> bool {
        self.signal.get()
    }

    pub fn set(self, focused: bool) {
        self.signal.set(focused);
    }

    pub fn focus(self) {
        self.set(true);
    }

    pub fn blur(self) {
        self.set(false);
    }

    pub fn toggle(self) {
        self.set(!self.get());
    }

    pub fn signal(self) -> Signal<bool> {
        self.signal
    }
}

impl InteractionState {
    pub fn selected(self) -> FocusState {
        self.selected
    }

    pub fn focused(self) -> FocusState {
        self.focused
    }

    pub fn is_selected(self) -> bool {
        self.selected.get()
    }

    pub fn is_focused(self) -> bool {
        self.focused.get()
    }

    pub fn select(self) {
        self.selected.focus();
    }

    pub fn deselect(self) {
        self.selected.blur();
        self.focused.blur();
    }

    pub fn focus(self) {
        self.select();
        self.focused.focus();
    }

    pub fn blur(self) {
        self.focused.blur();
    }

    pub fn sync_navigator(self, selected: bool) {
        if selected {
            if self.is_focused() {
                self.focus();
            } else {
                self.select();
            }
        } else {
            self.deselect();
        }
    }
}

impl SelectionState {
    pub fn get(self) -> usize {
        self.index.get()
    }

    pub fn len(self) -> usize {
        self.len.get()
    }

    pub fn set(self, index: usize) {
        let len = self.len();
        if len == 0 {
            self.index.set(0);
            return;
        }
        self.index.set(index.min(len - 1));
    }

    pub fn set_len(self, len: usize) {
        let len = len.max(1);
        self.len.set(len);
        self.set(self.get());
    }

    pub fn next(self) {
        let len = self.len();
        if len == 0 {
            return;
        }
        self.index.set((self.get() + 1) % len);
    }

    pub fn prev(self) {
        let len = self.len();
        if len == 0 {
            return;
        }
        self.index.set((self.get() + len - 1) % len);
    }
}

impl<T: Copy + Eq> FocusNode<T> {
    pub fn new(id: T, area: Rect) -> Self {
        Self { id, area }
    }
}

impl<T: Copy + Eq + 'static> FocusNavigator<T> {
    pub fn get(self) -> Option<T> {
        self.selected.get()
    }

    pub fn set(self, selected: Option<T>) {
        self.selected.set(selected);
    }

    pub fn clear(self) {
        self.set(None);
    }

    pub fn sync(self, nodes: &[FocusNode<T>]) -> Option<T> {
        if nodes.is_empty() {
            self.clear();
            return None;
        }

        if let Some(selected) = self.get()
            && nodes.iter().any(|node| node.id == selected)
        {
            return Some(selected);
        }

        let first = Some(nodes[0].id);
        self.set(first);
        first
    }

    pub fn sync_with_preferred(self, nodes: &[FocusNode<T>], preferred: T) -> Option<T> {
        if nodes.is_empty() {
            self.clear();
            return None;
        }

        if let Some(selected) = self.get()
            && nodes.iter().any(|node| node.id == selected)
        {
            return Some(selected);
        }

        let next = nodes
            .iter()
            .find(|node| node.id == preferred)
            .map(|node| node.id)
            .or_else(|| nodes.first().map(|node| node.id));
        self.set(next);
        next
    }

    pub fn next(self, nodes: &[FocusNode<T>]) -> Option<T> {
        self.step(nodes, 1)
    }

    pub fn prev(self, nodes: &[FocusNode<T>]) -> Option<T> {
        self.step(nodes, -1)
    }

    pub fn move_direction(self, nodes: &[FocusNode<T>], direction: FocusDirection) -> Option<T> {
        match direction {
            FocusDirection::Next => self.next(nodes),
            FocusDirection::Previous => self.prev(nodes),
            FocusDirection::Up
            | FocusDirection::Down
            | FocusDirection::Left
            | FocusDirection::Right => self.move_spatially(nodes, direction),
        }
    }

    fn step(self, nodes: &[FocusNode<T>], delta: isize) -> Option<T> {
        if nodes.is_empty() {
            self.clear();
            return None;
        }

        let current = self.sync(nodes)?;
        let current_idx = nodes
            .iter()
            .position(|node| node.id == current)
            .unwrap_or_default();
        let len = nodes.len() as isize;
        let next_idx = (current_idx as isize + delta).rem_euclid(len) as usize;
        let next = Some(nodes[next_idx].id);
        self.set(next);
        next
    }

    fn move_spatially(self, nodes: &[FocusNode<T>], direction: FocusDirection) -> Option<T> {
        let current = self.sync(nodes)?;
        let current_node = nodes.iter().find(|node| node.id == current)?;

        let mut best: Option<(u8, i32, i32, usize, T)> = None;
        for (idx, node) in nodes.iter().enumerate() {
            if node.id == current {
                continue;
            }

            let Some((lane_rank, primary_distance, secondary_distance)) =
                directional_metrics(current_node.area, node.area, direction)
            else {
                continue;
            };

            let candidate = (
                lane_rank,
                primary_distance,
                secondary_distance,
                idx,
                node.id,
            );
            if best.map_or(true, |best_candidate| {
                (candidate.0, candidate.1, candidate.2, candidate.3)
                    < (
                        best_candidate.0,
                        best_candidate.1,
                        best_candidate.2,
                        best_candidate.3,
                    )
            }) {
                best = Some(candidate);
            }
        }

        let next = best.map(|(_, _, _, _, id)| id).or(Some(current));
        self.set(next);
        next
    }
}

fn directional_metrics(from: Rect, to: Rect, direction: FocusDirection) -> Option<(u8, i32, i32)> {
    let from_center_x = from.x as i32 + from.width as i32 / 2;
    let from_center_y = from.y as i32 + from.height as i32 / 2;
    let to_center_x = to.x as i32 + to.width as i32 / 2;
    let to_center_y = to.y as i32 + to.height as i32 / 2;

    let dx = to_center_x - from_center_x;
    let dy = to_center_y - from_center_y;

    let (primary_distance, secondary_distance, overlaps_lane) = match direction {
        FocusDirection::Left if dx < 0 => (
            -dx,
            dy.abs(),
            ranges_overlap(from.y, from.y + from.height, to.y, to.y + to.height),
        ),
        FocusDirection::Right if dx > 0 => (
            dx,
            dy.abs(),
            ranges_overlap(from.y, from.y + from.height, to.y, to.y + to.height),
        ),
        FocusDirection::Up if dy < 0 => (
            -dy,
            dx.abs(),
            ranges_overlap(from.x, from.x + from.width, to.x, to.x + to.width),
        ),
        FocusDirection::Down if dy > 0 => (
            dy,
            dx.abs(),
            ranges_overlap(from.x, from.x + from.width, to.x, to.x + to.width),
        ),
        _ => return None,
    };

    let lane_rank = if overlaps_lane { 0 } else { 1 };

    Some((lane_rank, primary_distance, secondary_distance))
}

fn ranges_overlap(a_start: u16, a_end: u16, b_start: u16, b_end: u16) -> bool {
    a_start < b_end && b_start < a_end
}

/// Bridges app-level navigator selection with a component's own interaction model.
///
/// Components with a separate "active" mode, such as textboxes and terminals,
/// can override `is_navigator_active()` and `handle_navigator_event()` so callers
/// do not need ad hoc selected-vs-focused glue.
pub trait NavigatorFocusable {
    fn sync_navigator_focus(&self, selected: bool);

    fn is_navigator_active(&self) -> bool {
        false
    }

    fn handle_navigator_event(&self, _event: &SmashEvent) -> EventStatus {
        EventStatus::Ignored
    }
}

impl<T> NavigatorFocusable for &T
where
    T: NavigatorFocusable + ?Sized,
{
    fn sync_navigator_focus(&self, selected: bool) {
        T::sync_navigator_focus(*self, selected);
    }

    fn is_navigator_active(&self) -> bool {
        T::is_navigator_active(*self)
    }

    fn handle_navigator_event(&self, event: &SmashEvent) -> EventStatus {
        T::handle_navigator_event(*self, event)
    }
}

pub fn sync_navigator_focus<T, C, I>(selected: Option<T>, components: I)
where
    T: Copy + Eq,
    C: NavigatorFocusable,
    I: IntoIterator<Item = (T, C)>,
{
    for (id, component) in components {
        component.sync_navigator_focus(Some(id) == selected);
    }
}

pub fn active_navigator_focus<T, C, I>(selected: Option<T>, components: I) -> Option<T>
where
    T: Copy + Eq,
    C: NavigatorFocusable,
    I: IntoIterator<Item = (T, C)>,
{
    let selected = selected?;
    components.into_iter().find_map(|(id, component)| {
        (id == selected && component.is_navigator_active()).then_some(id)
    })
}

pub fn handle_selected_navigator_event<T, C, I>(
    selected: Option<T>,
    event: &SmashEvent,
    components: I,
) -> EventStatus
where
    T: Copy + Eq,
    C: NavigatorFocusable,
    I: IntoIterator<Item = (T, C)>,
{
    let Some(selected) = selected else {
        return EventStatus::Ignored;
    };

    components
        .into_iter()
        .find_map(|(id, component)| {
            (id == selected).then(|| component.handle_navigator_event(event))
        })
        .unwrap_or(EventStatus::Ignored)
}

#[deprecated(note = "Prefer use_selection for bounded tab state.")]
pub fn use_tabs(initial: usize, count: usize) -> Signal<usize> {
    let count = count.max(1);
    create_signal(initial.min(count - 1))
}

#[deprecated(note = "Prefer create_signal or use_focus for semantic state helpers.")]
pub fn use_toggle(initial: bool) -> Signal<bool> {
    create_signal(initial)
}
