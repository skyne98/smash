#[cfg(test)]
mod unit_tests {
    use crate::button::{ButtonEvent, ButtonVariant, use_button, use_button_variant};
    use crate::dialog::{DialogEvent, use_dialog};
    use crate::events::{EventStatus, SmashEvent, use_dispatcher};
    use crate::prelude::*;
    use crate::reactive::{
        FocusDirection, FocusNode, use_focus, use_focus_navigator, use_selection,
    };
    use crate::textbox::use_textbox;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use ratatui::backend::TestBackend;

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn key_release(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Release,
            state: KeyEventState::empty(),
        }
    }

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn dispatcher_processes_all_queued_events_in_order() {
        let _root = create_root(|| {
            let dispatcher = use_dispatcher();
            dispatcher.emit(SmashEvent::Key(key_event(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            )));
            dispatcher.emit(SmashEvent::Key(key_event(
                KeyCode::Char('b'),
                KeyModifiers::SHIFT,
            )));

            let mut seen = Vec::new();
            let handled = dispatcher.dispatch(|event| {
                if let SmashEvent::Key(key) = event {
                    if let KeyCode::Char(c) = key.code {
                        seen.push(c);
                    }
                    return EventStatus::Handled;
                }
                EventStatus::Ignored
            });

            assert!(handled);
            assert_eq!(seen, vec!['a', 'b']);
            assert!(dispatcher.drain().is_empty());
        });
    }

    #[test]
    fn focus_state_helpers_are_consistent() {
        let _root = create_root(|| {
            let focus = use_focus(false);

            assert!(!focus.get());
            focus.focus();
            assert!(focus.get());
            focus.toggle();
            assert!(!focus.get());
            focus.set(true);
            assert!(focus.get());
            focus.blur();
            assert!(!focus.get());
        });
    }

    #[test]
    fn selection_state_cycles_and_clamps() {
        let _root = create_root(|| {
            let selection = use_selection(1, 3);

            assert_eq!(selection.get(), 1);
            selection.next();
            assert_eq!(selection.get(), 2);
            selection.next();
            assert_eq!(selection.get(), 0);
            selection.prev();
            assert_eq!(selection.get(), 2);

            selection.set_len(2);
            assert_eq!(selection.get(), 1);
            selection.set(99);
            assert_eq!(selection.get(), 1);
        });
    }

    #[test]
    fn focus_navigator_cycles_visible_controls() {
        let _root = create_root(|| {
            let navigator = use_focus_navigator(Some(2usize));
            let nodes = [
                FocusNode::new(1usize, Rect::new(0, 0, 10, 3)),
                FocusNode::new(2usize, Rect::new(12, 0, 10, 3)),
                FocusNode::new(3usize, Rect::new(24, 0, 10, 3)),
            ];

            assert_eq!(navigator.next(&nodes), Some(3));
            assert_eq!(navigator.next(&nodes), Some(1));
            assert_eq!(navigator.prev(&nodes), Some(3));

            navigator.set(Some(99));
            assert_eq!(navigator.sync(&nodes), Some(1));
        });
    }

    #[test]
    fn focus_navigator_prefers_nearest_control_in_direction() {
        let _root = create_root(|| {
            let navigator = use_focus_navigator(Some(1usize));
            let nodes = [
                FocusNode::new(1usize, Rect::new(0, 0, 10, 3)),
                FocusNode::new(2usize, Rect::new(14, 0, 10, 3)),
                FocusNode::new(3usize, Rect::new(0, 6, 10, 3)),
                FocusNode::new(4usize, Rect::new(14, 6, 10, 3)),
            ];

            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Right),
                Some(2)
            );
            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Down),
                Some(4)
            );
            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Left),
                Some(3)
            );
            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Up),
                Some(1)
            );
        });
    }

    #[test]
    fn focus_navigator_prefers_same_lane_over_closer_off_axis_target() {
        let _root = create_root(|| {
            let navigator = use_focus_navigator(Some(3usize));
            let nodes = [
                FocusNode::new(1usize, Rect::new(0, 0, 80, 3)),
                FocusNode::new(2usize, Rect::new(20, 8, 12, 3)),
                FocusNode::new(3usize, Rect::new(36, 8, 12, 3)),
            ];

            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Left),
                Some(2)
            );
        });
    }

    #[test]
    fn focus_navigator_prefers_aligned_downward_target() {
        let _root = create_root(|| {
            let navigator = use_focus_navigator(Some(1usize));
            let nodes = [
                FocusNode::new(1usize, Rect::new(0, 0, 12, 3)),
                FocusNode::new(2usize, Rect::new(0, 6, 12, 3)),
                FocusNode::new(3usize, Rect::new(14, 6, 12, 3)),
            ];

            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Down),
                Some(2)
            );
            assert_eq!(
                navigator.move_direction(&nodes, FocusDirection::Up),
                Some(1)
            );
        });
    }

    #[test]
    fn dialog_confirms_after_keyboard_selection() {
        let _root = create_root(|| {
            let dialog = use_dialog("quit", "leave the app?");
            dialog.set_labels("stay", "quit");
            dialog.open();

            assert!(dialog.is_open());
            assert_eq!(
                dialog.handle_smash_event(&SmashEvent::Key(key_event(
                    KeyCode::Right,
                    KeyModifiers::NONE
                ))),
                DialogEvent::Handled
            );
            assert_eq!(
                dialog.handle_smash_event(&SmashEvent::Key(key_event(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))),
                DialogEvent::Confirmed
            );
            assert!(!dialog.is_open());
        });
    }

    #[test]
    fn dialog_escape_cancels() {
        let _root = create_root(|| {
            let dialog = use_dialog("quit", "leave the app?");
            dialog.open();

            assert_eq!(
                dialog.handle_smash_event(&SmashEvent::Key(key_event(
                    KeyCode::Esc,
                    KeyModifiers::NONE
                ))),
                DialogEvent::Cancelled
            );
            assert!(!dialog.is_open());
        });
    }

    #[test]
    fn button_keyboard_click_keeps_focus_and_clears_pressed_state() {
        let _root = create_root(|| {
            let button = use_button("save");
            let clicks = create_signal(0);
            button.on_click({
                let clicks = clicks;
                move |event| {
                    if let ButtonEvent::Click = event {
                        clicks.set(clicks.get() + 1);
                    }
                }
            });

            button.focus();

            assert_eq!(
                button.handle_event(&SmashEvent::Key(key_event(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))),
                EventStatus::Handled
            );
            assert_eq!(clicks.get(), 1);
            assert!(button.is_focused.get());
            assert!(!button.is_pressed.get());

            assert_eq!(
                button.handle_event(&SmashEvent::Key(key_release(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))),
                EventStatus::Handled
            );
            assert!(button.is_focused.get());
        });
    }

    #[test]
    fn button_variants_are_configurable() {
        let _root = create_root(|| {
            let button = use_button_variant("delete", ButtonVariant::Danger);

            assert_eq!(button.variant.get(), ButtonVariant::Danger);
            button.set_variant(ButtonVariant::Secondary);
            assert_eq!(button.variant.get(), ButtonVariant::Secondary);
        });
    }

    #[test]
    fn button_render_tracks_area_for_mouse_events() {
        let _root = create_root(|| {
            let button = use_button("save");
            let clicks = create_signal(0);
            button.on_click({
                let clicks = clicks;
                move |event| {
                    if let ButtonEvent::Click = event {
                        clicks.set(clicks.get() + 1);
                    }
                }
            });

            let theme = SmashTheme::from_seed(crate::theme::presets::VIOLET, true);
            let backend = TestBackend::new(20, 5);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(2, 1, 10, 3), &theme);
                })
                .unwrap();

            assert_eq!(
                button.handle_event(&SmashEvent::Mouse(mouse_event(
                    MouseEventKind::Down(MouseButton::Left),
                    3,
                    2
                ))),
                EventStatus::Handled
            );
            assert!(button.is_pressed.get());

            assert_eq!(
                button.handle_event(&SmashEvent::Mouse(mouse_event(
                    MouseEventKind::Up(MouseButton::Left),
                    3,
                    2
                ))),
                EventStatus::Handled
            );
            assert_eq!(clicks.get(), 1);
            assert!(button.is_focused.get());
            assert!(!button.is_pressed.get());
        });
    }

    #[test]
    fn focused_button_keeps_accent_bar_separate_from_label_area() {
        let _root = create_root(|| {
            let button = use_button_variant("save", ButtonVariant::Primary);
            button.focus();

            let theme = SmashTheme::from_seed(crate::theme::presets::VIOLET, true);
            let backend = TestBackend::new(20, 5);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(1, 1, 12, 3), &theme);
                })
                .unwrap();

            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(2, 2)].bg, theme.on_primary);
            assert_eq!(buffer[(3, 2)].bg, theme.primary);
        });
    }

    #[test]
    fn textbox_smash_event_reports_handled_for_selection_commands() {
        let _root = create_root(|| {
            let textbox = use_textbox("hello world");
            textbox.focus();

            assert_eq!(
                textbox.handle_smash_event(&SmashEvent::Key(key_event(
                    KeyCode::Char('a'),
                    KeyModifiers::CONTROL
                ))),
                EventStatus::Handled
            );
            assert_eq!(textbox.get_normalized_selection(), Some(((0, 0), (0, 11))));
        });
    }

    #[test]
    fn read_only_textbox_allows_selection_but_blocks_edits() {
        let _root = create_root(|| {
            let textbox = use_textbox("hello");
            textbox.set_read_only(true);
            textbox.focus();

            assert_eq!(
                textbox.handle_smash_event(&SmashEvent::Key(key_event(
                    KeyCode::Char('a'),
                    KeyModifiers::CONTROL
                ))),
                EventStatus::Handled
            );
            assert_eq!(textbox.get_normalized_selection(), Some(((0, 0), (0, 5))));

            assert_eq!(
                textbox.handle_smash_event(&SmashEvent::Key(key_event(
                    KeyCode::Char('!'),
                    KeyModifiers::NONE
                ))),
                EventStatus::Ignored
            );
            assert_eq!(textbox.lines.get_clone(), vec!["hello".to_string()]);
        });
    }
}
