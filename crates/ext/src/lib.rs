//! Wasm extension host.
//!
//! Two hooks, both optional, both JSON in / JSON out — a plugin is any wasm
//! module (extism PDK, any language) that exports either:
//!
//! * `smash_action` — receives an [`Action`], returns an [`Action`]. Empty reply
//!   keeps the previous value; `null` drops the action.
//! * `smash_view` — receives the [`View`], returns a [`View`]. Add rows, change
//!   the status line, replace the title.
//!
//! That is the whole "extend anything" surface: every action and every frame
//! passes through here, and neither client knows plugins exist.

use std::path::Path;

use extism::{Function, Manifest, Plugin, Wasm};
use smash_core::{Action, Hooks, View};

pub const ACTION_HOOK: &str = "smash_action";
pub const VIEW_HOOK: &str = "smash_view";

#[derive(Default)]
pub struct Ext {
    plugins: Vec<Plugin>,
}

impl Ext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads every `*.wasm` in `dir`. A plugin that fails to load is skipped.
    pub fn load_dir(dir: impl AsRef<Path>) -> Self {
        let mut ext = Self::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return ext;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wasm") {
                let _ = ext.load(&path);
            }
        }
        ext
    }

    pub fn load(&mut self, wasm: impl AsRef<Path>) -> Result<(), extism::Error> {
        let manifest = Manifest::new([Wasm::file(wasm)]);
        self.plugins
            .push(Plugin::new(manifest, Vec::<Function>::new(), true)?);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Runs `hook` through every plugin in load order, threading the payload
    /// through the chain. Errors and empty replies leave the payload alone.
    fn call(&mut self, hook: &str, mut payload: Vec<u8>) -> Vec<u8> {
        for plugin in &mut self.plugins {
            if let Ok(reply) = plugin.call::<&[u8], Vec<u8>>(hook, &payload)
                && !reply.is_empty()
            {
                payload = reply;
            }
        }
        payload
    }
}

impl Hooks for Ext {
    fn action(&mut self, action: Action) -> Option<Action> {
        // No plugin, no JSON - the common path costs nothing.
        if self.plugins.is_empty() {
            return Some(action);
        }
        let payload = self.call(ACTION_HOOK, sonic_rs::to_vec(&action).ok()?);
        sonic_rs::from_slice(&payload).ok()
    }

    fn view(&mut self, view: &mut View) {
        // Both hooks use sonic-rs: it wins from ~1 KB up (3.6 GB/s borrowed vs
        // 3.3 serde_json at 11 MB, see examples/json-bench.rs). Above ~10 MB the
        // win comes from borrowed `&str` fields, not from SIMD, so large
        // payloads should deserialize borrowed.
        if self.plugins.is_empty() {
            return;
        }
        let Ok(payload) = sonic_rs::to_vec(&*view) else {
            return;
        };
        if let Ok(decorated) = sonic_rs::from_slice(&self.call(VIEW_HOOK, payload)) {
            *view = decorated;
        }
    }
}
