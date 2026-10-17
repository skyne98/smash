//! Client runtime shared by every frontend.

use smash_core::{Action, App, Effect, Event, Hooks, View};

/// Where effects go. In-process today; a socket/daemon transport slots in here
/// without touching either client or the core.
pub trait Backend {
    fn exec(&mut self, effect: Effect) -> Vec<Event>;
}

/// Owns the [`App`], routes actions through extension hooks, runs effects on
/// the backend, folds events back into state. A UI only calls
/// [`Client::dispatch`] and [`Client::view`].
pub struct Client {
    app: App,
    backend: Box<dyn Backend>,
    hooks: Box<dyn Hooks>,
}

impl Client {
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self::with_hooks(backend, Box::new(()))
    }

    pub fn with_hooks(backend: impl Backend + 'static, hooks: Box<dyn Hooks>) -> Self {
        Self {
            app: App::new(),
            backend: Box::new(backend),
            hooks,
        }
    }

    /// Pull initial state from the backend.
    pub fn start(&mut self) {
        self.run(vec![Effect::Load]);
    }

    pub fn dispatch(&mut self, action: Action) {
        let Some(action) = self.hooks.action(action) else {
            return;
        };
        let effects = self.app.dispatch(action);
        self.run(effects);
    }

    pub fn view(&mut self) -> View {
        let mut view = self.app.view();
        self.hooks.view(&mut view);
        view
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    fn run(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            for event in self.backend.exec(effect) {
                self.hooks.event(&event);
                self.app.apply(event);
            }
        }
    }
}
