//! gpui frontend: renders `core::View`, maps key bindings to `core::Action`.
//! Same runtime and same state machine as the terminal client; only the paint differs.

use gpui::{
    App, Bounds, Context, FocusHandle, KeyBinding, Render, SharedString, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use smash_backend::Files;
use smash_client::Client;
use smash_core::Action;
use smash_ui::{Mode, Role, Theme, hints, transcript};

const THEME: Theme = smash_ui::NIGHT;

actions!(smash, [NewDoc, NextDoc, PrevDoc, CloseDoc, Quit]);

struct Root {
    client: Client,
    focus: FocusHandle,
}

impl Root {
    fn new(cx: &mut Context<Self>) -> Self {
        let dir = Files::default_dir();
        let ext = smash_ext::Ext::load_dir(dir.join("extensions"));
        let hooks: Box<dyn smash_core::Hooks> =
            match smash_session::Session::create(dir.join("sessions")) {
                Ok(session) => Box::new((session, ext)),
                Err(_) => Box::new(ext),
            };
        let mut client = Client::with_hooks(Files::new(dir), hooks);
        client.start();
        Self {
            client,
            focus: cx.focus_handle(),
        }
    }

    fn on_new(&mut self, _: &NewDoc, _: &mut Window, cx: &mut Context<Self>) {
        self.client.dispatch(Action::New);
        cx.notify();
    }

    fn on_next(&mut self, _: &NextDoc, _: &mut Window, cx: &mut Context<Self>) {
        self.client.dispatch(Action::Next);
        cx.notify();
    }

    fn on_prev(&mut self, _: &PrevDoc, _: &mut Window, cx: &mut Context<Self>) {
        self.client.dispatch(Action::Prev);
        cx.notify();
    }

    fn on_close(&mut self, _: &CloseDoc, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.client.app().selected().map(|doc| doc.id) {
            self.client.dispatch(Action::Close(id));
            cx.notify();
        }
    }

    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Nothing else is focusable yet; keep the root focused so bindings fire.
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }

        let view = self.client.view();
        let title = view
            .rows
            .iter()
            .find(|row| row.selected)
            .map(|row| row.text.clone())
            .unwrap_or_else(|| "no session".into());

        // Same view model the terminal paints: smash_ui::transcript.
        let lines = transcript(&view.transcript).into_iter().map(|line| {
            let (marker, color) = match line.role {
                Role::User => ("❯ ", THEME.dim),
                Role::Assistant => ("", THEME.text),
            };
            div()
                .w_full()
                .text_color(rgb(color))
                .child(SharedString::from(format!("{marker}{}", line.text)))
        });

        div()
            .size_full()
            .bg(rgb(THEME.bg))
            .text_color(rgb(THEME.text))
            .flex()
            .flex_col()
            .gap_2()
            .p_6()
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_quit))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_sm()
                    .text_color(rgb(THEME.dim))
                    .child(SharedString::from(title))
                    .child(SharedString::from(view.status)),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .gap_1()
                    .children(lines),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(THEME.dim))
                    .child(SharedString::from(hints(Mode::Chat, false, false))),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
        // VS Code habits: arrows move, ctrl+n new, ctrl+w close, ctrl+q quit.
        cx.bind_keys([
            KeyBinding::new("up", PrevDoc, None),
            KeyBinding::new("down", NextDoc, None),
            KeyBinding::new("ctrl-n", NewDoc, None),
            KeyBinding::new("ctrl-w", CloseDoc, None),
            KeyBinding::new("ctrl-q", Quit, None),
        ]);

        let bounds = Bounds::centered(None, size(px(720.), px(520.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Root::new),
        )
        .expect("open window");
        cx.activate(true);
    });
}
