//! The engine: executes [`Effect`]s. File-backed for now.
//!
//! ponytail: synchronous and in-process, one JSON file per doc. Swap in a
//! tokio task + socket transport when the backend has to outlive the UI.

mod agent;
pub mod agents;
pub mod compact;

pub use agent::{Agent, Inbox, Run, Update};
pub use agents::Agents;
pub use compact::{Compactor, Summarizer};
pub use tokio_util::sync::CancellationToken;

use std::fs;
use std::path::{Path, PathBuf};

use smash_client::Backend;
use smash_core::{Doc, DocId, Effect, Event};

pub struct Files {
    dir: PathBuf,
}

impl Files {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `$XDG_DATA_HOME/smash`, falling back to `~/.local/share/smash`.
    pub fn default_dir() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("smash")
    }

    fn path(&self, id: DocId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn load(&self) -> Result<Vec<Doc>, String> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.to_string()),
        };
        // Only `<id>.json` files are documents. Anything else in the data dir
        // (settings, caches) is skipped, and one unreadable document is skipped
        // too: failing the whole load once made the app start empty and
        // overwrite real sessions.
        let mut docs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let is_doc = path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.parse::<DocId>().is_ok());
            if !is_doc {
                continue;
            }
            let parsed = fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| serde_json::from_slice::<Doc>(&bytes).map_err(|e| e.to_string()));
            match parsed {
                Ok(doc) => docs.push(doc),
                Err(err) => eprintln!("smash: skipping {}: {err}", path.display()),
            }
        }
        docs.sort_by_key(|doc: &Doc| doc.id);
        Ok(docs)
    }

    fn exec_inner(&mut self, effect: Effect) -> Result<Event, String> {
        match effect {
            Effect::Load => Ok(Event::Loaded(self.load()?)),
            Effect::Save(doc) => {
                fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
                let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
                // Write-then-rename: a crash mid-save never leaves half a session.
                let path = self.path(doc.id);
                let partial = path.with_extension("json.part");
                fs::write(&partial, bytes).map_err(|e| e.to_string())?;
                fs::rename(&partial, &path).map_err(|e| e.to_string())?;
                Ok(Event::Saved(doc.id))
            }
            Effect::Drop(id) => {
                match fs::remove_file(self.path(id)) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.to_string()),
                }
                Ok(Event::Dropped(id))
            }
        }
    }
}

impl Backend for Files {
    fn exec(&mut self, effect: Effect) -> Vec<Event> {
        match self.exec_inner(effect) {
            Ok(event) => vec![event],
            Err(err) => vec![Event::Failed(err)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stray_files_never_hide_sessions() {
        let dir = std::env::temp_dir().join(format!("smash-load-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("1.json"),
            r#"{"id":1,"title":"keep","body":"real chat"}"#,
        )
        .unwrap();
        fs::write(dir.join("models.json"), r#"{"deepseek":{"models":{}}}"#).unwrap();
        fs::write(dir.join("settings.json"), r#"{"model":"x"}"#).unwrap();
        fs::write(dir.join("2.json"), "{ torn").unwrap();
        let docs = Files::new(&dir).load().unwrap();
        assert_eq!(docs.len(), 1, "other json files and a torn doc are skipped");
        assert_eq!(docs[0].body, "real chat");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saves_loads_and_drops_through_the_backend_trait() {
        let dir = std::env::temp_dir().join(format!("smash-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut backend = Files::new(&dir);

        let doc = Doc {
            id: 3,
            title: "three".into(),
            body: String::new(),
        };
        assert!(matches!(
            backend.exec(Effect::Save(doc.clone())).as_slice(),
            [Event::Saved(3)]
        ));
        assert!(
            matches!(backend.exec(Effect::Load).as_slice(), [Event::Loaded(docs)] if docs == &vec![doc])
        );
        assert!(matches!(
            backend.exec(Effect::Drop(3)).as_slice(),
            [Event::Dropped(3)]
        ));
        assert!(
            matches!(backend.exec(Effect::Load).as_slice(), [Event::Loaded(docs)] if docs.is_empty())
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
