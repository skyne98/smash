#[cfg(test)]
mod unit_tests {
    use crate::button::{ButtonEvent, ButtonVariant, use_button, use_button_variant};
    use crate::dialog::{DialogEvent, use_dialog};
    use crate::events::{EventStatus, SmashEvent, use_dispatcher};
    use crate::prelude::*;
    use crate::reactive::{
        FocusDirection, FocusNode, use_focus, use_focus_navigator, use_selection,
    };
    use crate::syntax::{SyntaxRequest, SyntaxThemeKind, SyntaxWorker, highlight_request_sync};
    use crate::textbox::{TextBoxLanguage, use_textbox};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use ratatui::backend::TestBackend;
    use std::thread;
    use std::time::{Duration, Instant};

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
    fn interaction_state_helpers_are_consistent() {
        let _root = create_root(|| {
            let interaction = use_interaction(false, false);

            assert!(!interaction.is_selected());
            assert!(!interaction.is_focused());

            interaction.select();
            assert!(interaction.is_selected());
            assert!(!interaction.is_focused());

            interaction.focus();
            assert!(interaction.is_selected());
            assert!(interaction.is_focused());

            interaction.blur();
            assert!(interaction.is_selected());
            assert!(!interaction.is_focused());

            interaction.sync_navigator(false);
            assert!(!interaction.is_selected());
            assert!(!interaction.is_focused());
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
    fn focus_navigator_prefers_requested_default_when_current_is_missing() {
        let _root = create_root(|| {
            let navigator = use_focus_navigator(Some(99usize));
            let nodes = [
                FocusNode::new(1usize, Rect::new(0, 0, 10, 3)),
                FocusNode::new(2usize, Rect::new(12, 0, 10, 3)),
                FocusNode::new(3usize, Rect::new(24, 0, 10, 3)),
            ];

            assert_eq!(navigator.sync_with_preferred(&nodes, 2usize), Some(2usize));
            assert_eq!(navigator.get(), Some(2usize));
        });
    }

    #[test]
    fn navigator_helpers_route_events_and_report_active_controls() {
        let _root = create_root(|| {
            let button = use_button("run");
            let textbox = use_textbox("");

            sync_navigator_focus(
                Some(1usize),
                [
                    (1usize, &button as &dyn NavigatorFocusable),
                    (2usize, &textbox as &dyn NavigatorFocusable),
                ],
            );
            assert!(button.is_focused.get());
            assert!(!textbox.is_selected.get());

            assert_eq!(
                handle_selected_navigator_event(
                    Some(1usize),
                    &SmashEvent::Key(key_event(KeyCode::Enter, KeyModifiers::NONE)),
                    [
                        (1usize, &button as &dyn NavigatorFocusable),
                        (2usize, &textbox as &dyn NavigatorFocusable),
                    ],
                ),
                EventStatus::Handled
            );
            assert!(button.is_pressed.get());

            sync_navigator_focus(
                Some(2usize),
                [
                    (1usize, &button as &dyn NavigatorFocusable),
                    (2usize, &textbox as &dyn NavigatorFocusable),
                ],
            );
            assert_eq!(
                active_navigator_focus(
                    Some(2usize),
                    [
                        (1usize, &button as &dyn NavigatorFocusable),
                        (2usize, &textbox as &dyn NavigatorFocusable),
                    ],
                ),
                None
            );

            assert_eq!(
                handle_selected_navigator_event(
                    Some(2usize),
                    &SmashEvent::Key(key_event(KeyCode::Enter, KeyModifiers::NONE)),
                    [
                        (1usize, &button as &dyn NavigatorFocusable),
                        (2usize, &textbox as &dyn NavigatorFocusable),
                    ],
                ),
                EventStatus::Handled
            );
            assert!(textbox.is_focused.get());
            assert_eq!(
                active_navigator_focus(
                    Some(2usize),
                    [
                        (1usize, &button as &dyn NavigatorFocusable),
                        (2usize, &textbox as &dyn NavigatorFocusable),
                    ],
                ),
                Some(2usize)
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
            assert!(button.is_pressed.get());

            assert_eq!(
                button.handle_event(&SmashEvent::Key(key_release(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))),
                EventStatus::Handled
            );
            assert_eq!(clicks.get(), 1);
            assert!(button.is_focused.get());
            assert!(!button.is_pressed.get());
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
                    button.render(frame, Rect::new(2, 0, 10, 5), &theme);
                })
                .unwrap();

            assert_eq!(button.area(), Rect::new(3, 2, 8, 1));

            assert_eq!(
                button.handle_event(&SmashEvent::Mouse(mouse_event(MouseEventKind::Moved, 3, 2))),
                EventStatus::Ignored
            );
            assert!(button.is_hovered.get());

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
            assert!(button.is_hovered.get());

            assert_eq!(
                button.handle_event(&SmashEvent::Mouse(mouse_event(MouseEventKind::Moved, 0, 0))),
                EventStatus::Ignored
            );
            assert!(!button.is_hovered.get());
        });
    }

    #[test]
    fn button_min_height_expands_the_rendered_area() {
        let _root = create_root(|| {
            let button = use_button("save");
            button.set_min_height(5);

            let theme = SmashTheme::from_seed(crate::theme::presets::VIOLET, true);
            let backend = TestBackend::new(20, 9);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(2, 0, 10, 9), &theme);
                })
                .unwrap();

            assert_eq!(button.area(), Rect::new(3, 2, 8, 5));
        });
    }

    #[test]
    fn button_max_height_still_fits_multiline_labels() {
        let _root = create_root(|| {
            let button = use_button("save");
            button.label.set("save\nall".to_string());
            button.set_max_height(Some(3));

            let theme = SmashTheme::from_seed(crate::theme::presets::VIOLET, true);
            let backend = TestBackend::new(20, 8);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(2, 0, 10, 8), &theme);
                })
                .unwrap();

            assert_eq!(button.area(), Rect::new(3, 3, 8, 2));
        });
    }

    #[test]
    fn button_renders_rest_state_with_soft_fill_without_border_chrome() {
        let _root = create_root(|| {
            let button = use_button_variant("save", ButtonVariant::Primary);

            let theme = SmashTheme::from_seed(crate::theme::presets::VIOLET, true);
            let backend = TestBackend::new(20, 5);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(1, 1, 12, 3), &theme);
                })
                .unwrap();

            let buffer = terminal.backend().buffer();
            assert_eq!(button.area(), Rect::new(2, 2, 10, 1));
            assert_eq!(buffer[(1, 2)].bg, theme.background);
            assert_eq!(buffer[(2, 2)].bg, theme.surface_variant);
            assert_eq!(buffer[(5, 2)].symbol(), "s");
            assert_eq!(buffer[(5, 2)].fg, theme.primary);
            assert_eq!(buffer[(5, 2)].bg, theme.surface_variant);
        });
    }

    #[test]
    fn focused_button_uses_bracketed_label_and_pressed_state_inverts_fill() {
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

            let focused_buffer = terminal.backend().buffer().clone();
            assert_eq!(focused_buffer[(3, 2)].symbol(), "[");
            assert_eq!(focused_buffer[(6, 2)].bg, theme.primary_container);
            assert_eq!(focused_buffer[(6, 2)].fg, theme.on_primary_container);
            assert!(focused_buffer[(6, 2)].modifier.contains(Modifier::BOLD));

            button.is_pressed.set(true);

            let backend = TestBackend::new(20, 5);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(1, 1, 12, 3), &theme);
                })
                .unwrap();

            let pressed_buffer = terminal.backend().buffer();
            assert_eq!(pressed_buffer[(3, 2)].symbol(), ">");
            assert_eq!(pressed_buffer[(6, 2)].bg, theme.primary);
            assert!(pressed_buffer[(6, 2)].modifier.contains(Modifier::BOLD));
            assert_eq!(pressed_buffer[(6, 2)].fg, theme.on_primary);
        });
    }

    #[test]
    fn keyboard_press_feedback_clears_without_release() {
        let _root = create_root(|| {
            let button = use_button_variant("save", ButtonVariant::Primary);
            button.focus();

            assert_eq!(
                button.handle_event(&SmashEvent::Key(key_event(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))),
                EventStatus::Handled
            );
            assert!(button.is_pressed.get());

            button.expire_keyboard_press_feedback_for_test();

            let theme = SmashTheme::from_seed(crate::theme::presets::VIOLET, true);
            let backend = TestBackend::new(20, 5);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    button.render(frame, Rect::new(1, 1, 12, 3), &theme);
                })
                .unwrap();

            assert!(!button.is_pressed.get());
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

    #[test]
    fn textbox_auto_detects_language_from_path_hint() {
        let _root = create_root(|| {
            let textbox = use_textbox("fn greet(name: &str) {\n    println!(\"hi\");\n}");
            textbox.set_path_hint("sample.rs");

            assert_eq!(textbox.resolved_language_label(), "Rust");
        });
    }

    #[test]
    fn textbox_auto_detects_language_from_content_changes() {
        let _root = create_root(|| {
            let textbox = use_textbox("#include <stdio.h>\nint main() {}");
            textbox.set_path_hint("test.h");
            assert_eq!(textbox.resolved_language_label(), "C");

            textbox.lines.set(vec![
                "#include <iostream>".to_string(),
                "int main() {}".to_string(),
            ]);

            assert_eq!(textbox.resolved_language_label(), "C++");
        });
    }

    #[test]
    fn textbox_path_hint_remains_primary_when_present() {
        let _root = create_root(|| {
            let textbox = use_textbox("fn greet(name: &str) {\n    println!(\"hi\");\n}");
            textbox.set_path_hint("sample.rs");
            assert_eq!(textbox.resolved_language_label(), "Rust");

            textbox
                .lines
                .set(vec!["{\"name\":\"smash\",\"kind\":\"demo\"}".to_string()]);

            assert_eq!(textbox.resolved_language_label(), "Rust");
        });
    }

    #[test]
    fn textbox_uses_path_hint_as_primary_hint_for_ambiguous_languages() {
        let snapshot = highlight_request_sync(&SyntaxRequest {
            revision: 1,
            theme_kind: SyntaxThemeKind::Dark,
            title: "header preview".to_string(),
            path_hint: Some("test.h".to_string()),
            language: TextBoxLanguage::Auto,
            lines: vec![
                "#include <iostream>".to_string(),
                "int main() {}".to_string(),
            ],
        });

        assert_eq!(snapshot.language_label, "C++");
    }

    #[test]
    fn textbox_language_can_override_auto_detection() {
        let _root = create_root(|| {
            let textbox = use_textbox("fn greet() {}");
            textbox.set_language(TextBoxLanguage::Json);

            assert_eq!(textbox.resolved_language_label(), "JSON");
        });
    }

    #[test]
    fn textbox_highlighting_uses_syntect_styles() {
        let snapshot = highlight_request_sync(&SyntaxRequest {
            revision: 1,
            theme_kind: SyntaxThemeKind::Dark,
            title: "editor".to_string(),
            path_hint: Some("example.rs".to_string()),
            language: TextBoxLanguage::Auto,
            lines: vec!["let msg = \"hi\";".to_string()],
        });

        assert_eq!(snapshot.language_label, "Rust");
        assert_ne!(
            snapshot.line_styles[0][10],
            ratatui::style::Style::default()
        );
        assert_ne!(
            snapshot.line_styles[0][11].fg,
            snapshot.line_styles[0][4].fg
        );
    }

    #[test]
    fn syntax_worker_debounces_to_latest_request() {
        let worker = SyntaxWorker::new();
        worker.schedule(SyntaxRequest {
            revision: 1,
            theme_kind: SyntaxThemeKind::Dark,
            title: "editor".to_string(),
            path_hint: Some("first.rs".to_string()),
            language: TextBoxLanguage::Auto,
            lines: vec!["fn first() {}".to_string()],
        });
        worker.schedule(SyntaxRequest {
            revision: 2,
            theme_kind: SyntaxThemeKind::Dark,
            title: "notes".to_string(),
            path_hint: Some("notes.md".to_string()),
            language: TextBoxLanguage::Auto,
            lines: vec!["# heading".to_string()],
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        let snapshot = loop {
            if let Some(snapshot) = worker.latest_snapshot() {
                break snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "missing syntax snapshot before timeout"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.language_label, "Markdown");
    }
}
