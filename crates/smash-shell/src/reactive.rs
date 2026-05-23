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
    last_node: Signal<Option<FocusNode<T>>>,
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
        last_node: create_signal(None),
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

    pub fn is_empty(self) -> bool {
        self.len() == 0
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
        self.last_node.set(None);
    }

    pub fn set_node(self, node: FocusNode<T>) {
        self.select_node(node);
    }

    pub fn clear(self) {
        self.set(None);
        self.last_node.set(None);
    }

    pub fn sync(self, nodes: &[FocusNode<T>]) -> Option<T> {
        if nodes.is_empty() {
            self.clear();
            return None;
        }

        if let Some(node) = self.current_node(nodes) {
            return Some(node.id);
        }

        let next = self
            .nearest_to_last_node(nodes)
            .or_else(|| nodes.first().copied())?;
        self.select_node(next);
        Some(next.id)
    }

    pub fn sync_with_preferred(self, nodes: &[FocusNode<T>], preferred: T) -> Option<T> {
        if nodes.is_empty() {
            self.clear();
            return None;
        }

        if let Some(node) = self.current_node(nodes) {
            return Some(node.id);
        }

        let next = self
            .nearest_to_last_node(nodes)
            .or_else(|| nodes.iter().find(|node| node.id == preferred).copied())
            .or_else(|| nodes.first().copied())?;
        self.select_node(next);
        Some(next.id)
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
        let next = nodes[next_idx];
        self.select_node(next);
        Some(next.id)
    }

    fn move_spatially(self, nodes: &[FocusNode<T>], direction: FocusDirection) -> Option<T> {
        let current = self.sync(nodes)?;
        let current_node = nodes.iter().find(|node| node.id == current)?;

        let mut best: Option<(FocusCandidateRank, usize, FocusNode<T>)> = None;
        for (idx, node) in nodes.iter().enumerate() {
            if node.id == current {
                continue;
            }

            let Some(rank) = directional_rank(current_node.area, node.area, direction) else {
                continue;
            };

            let candidate = (rank, idx, *node);
            if best.is_none_or(|best_candidate| {
                (candidate.0, candidate.1) < (best_candidate.0, best_candidate.1)
            }) {
                best = Some(candidate);
            }
        }

        if let Some((_, _, node)) = best {
            self.select_node(node);
            Some(node.id)
        } else {
            Some(current)
        }
    }

    fn current_node(self, nodes: &[FocusNode<T>]) -> Option<FocusNode<T>> {
        let selected = self.get()?;
        let node = nodes.iter().find(|node| node.id == selected).copied()?;
        self.last_node.set(Some(node));
        Some(node)
    }

    fn select_node(self, node: FocusNode<T>) {
        self.selected.set(Some(node.id));
        self.last_node.set(Some(node));
    }

    fn nearest_to_last_node(self, nodes: &[FocusNode<T>]) -> Option<FocusNode<T>> {
        let anchor = self.last_node.get()?;
        nodes
            .iter()
            .enumerate()
            .min_by_key(|(idx, node)| (nearest_rank(anchor.area, node.area), *idx))
            .map(|(_, node)| *node)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FocusCandidateRank {
    beam_rank: u8,
    primary_gap: i32,
    secondary_gap: i32,
    center_delta: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NearestRank {
    total_gap: i32,
    max_gap: i32,
    center_distance: i32,
}

fn directional_rank(from: Rect, to: Rect, direction: FocusDirection) -> Option<FocusCandidateRank> {
    let from_x = axis_range(from.x, from.width);
    let from_y = axis_range(from.y, from.height);
    let to_x = axis_range(to.x, to.width);
    let to_y = axis_range(to.y, to.height);

    let (primary_gap, secondary_gap, center_delta, overlaps_beam) = match direction {
        FocusDirection::Left if to_x.0 < from_x.0 => (
            (from_x.0 - to_x.1).max(0),
            range_gap(from_y, to_y),
            center_delta(from_y, to_y),
            ranges_overlap(from_y, to_y),
        ),
        FocusDirection::Right if to_x.1 > from_x.1 => (
            (to_x.0 - from_x.1).max(0),
            range_gap(from_y, to_y),
            center_delta(from_y, to_y),
            ranges_overlap(from_y, to_y),
        ),
        FocusDirection::Up if to_y.0 < from_y.0 => (
            (from_y.0 - to_y.1).max(0),
            range_gap(from_x, to_x),
            center_delta(from_x, to_x),
            ranges_overlap(from_x, to_x),
        ),
        FocusDirection::Down if to_y.1 > from_y.1 => (
            (to_y.0 - from_y.1).max(0),
            range_gap(from_x, to_x),
            center_delta(from_x, to_x),
            ranges_overlap(from_x, to_x),
        ),
        _ => return None,
    };

    Some(FocusCandidateRank {
        beam_rank: if overlaps_beam { 0 } else { 1 },
        primary_gap,
        secondary_gap,
        center_delta,
    })
}

fn nearest_rank(from: Rect, to: Rect) -> NearestRank {
    let dx = range_gap(axis_range(from.x, from.width), axis_range(to.x, to.width));
    let dy = range_gap(axis_range(from.y, from.height), axis_range(to.y, to.height));
    NearestRank {
        total_gap: dx + dy,
        max_gap: dx.max(dy),
        center_distance: (center(axis_range(from.x, from.width))
            - center(axis_range(to.x, to.width)))
        .abs()
            + (center(axis_range(from.y, from.height)) - center(axis_range(to.y, to.height))).abs(),
    }
}

fn axis_range(start: u16, len: u16) -> (i32, i32) {
    let start = i32::from(start);
    (start, start + i32::from(len))
}

fn ranges_overlap(a: (i32, i32), b: (i32, i32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn range_gap(a: (i32, i32), b: (i32, i32)) -> i32 {
    if ranges_overlap(a, b) {
        0
    } else if a.1 <= b.0 {
        b.0 - a.1
    } else {
        a.0 - b.1
    }
}

fn center(range: (i32, i32)) -> i32 {
    range.0 + (range.1 - range.0) / 2
}

fn center_delta(a: (i32, i32), b: (i32, i32)) -> i32 {
    (center(a) - center(b)).abs()
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
