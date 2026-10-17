//! Append-only session log: one JSON line per action or event.
//!
//! The log *is* the session state: the UI is a fold over it, replay is reading
//! it back, and a crash leaves at most one torn trailing line. Parsing uses
//! sonic-rs; at session scale the win comes from deserializing borrowed where
//! possible (see `crates/ext/examples/json-bench.rs`).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use smash_core::{Action, Event};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Monotonic per session, starting at 0.
    pub seq: u64,
    /// Unix milliseconds.
    pub ts_ms: u64,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Body {
    Action(Action),
    Event(Event),
}

/// Open handle. Writes are append-only; a session file is never rewritten.
pub struct Session {
    path: PathBuf,
    writer: BufWriter<File>,
    seq: u64,
}

impl Session {
    /// Creates `dir` and a new `<unix_ms>-<pid>.jsonl` file inside it.
    pub fn create(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let id = format!("{}-{}", now_ms(), std::process::id());
        let path = dir.join(format!("{id}.jsonl"));
        Self::open(path)
    }

    /// Opens an existing log and continues its sequence.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        // A brand-new log does not exist yet; that is not an error.
        let next = match read(&path) {
            Ok(records) => records.last().map(|record| record.seq + 1).unwrap_or(0),
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => return Err(err),
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
            seq: next,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Appends one record and flushes it, so a crash loses nothing already returned.
    pub fn append(&mut self, body: Body) -> io::Result<u64> {
        let seq = self.seq;
        let record = Record {
            seq,
            ts_ms: now_ms(),
            body,
        };
        let mut line = sonic_rs::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        self.writer.flush()?;
        self.seq += 1;
        Ok(seq)
    }
}

/// Reads every complete record. A torn trailing line (crash mid-write) is
/// skipped rather than failing the whole replay.
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<Record>> {
    let bytes = std::fs::read(path)?;
    let mut records = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = sonic_rs::from_slice::<Record>(line) {
            records.push(record);
        }
    }
    Ok(records)
}

/// Every action and every event lands in the log; `view` is not recorded
/// (a view is derivable state, and it changes every frame).
impl smash_core::Hooks for Session {
    fn action(&mut self, action: Action) -> Option<Action> {
        // Hooks cannot fail: a full disk must not take the UI down with it.
        let _ = self.append(Body::Action(action.clone()));
        Some(action)
    }

    fn event(&mut self, event: &Event) {
        let _ = self.append(Body::Event(event.clone()));
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        // Tests run in parallel: pid + millisecond alone can collide.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "smash-session-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn appends_replays_and_survives_a_torn_tail() {
        let dir = temp_dir();
        let mut session = Session::create(&dir).unwrap();

        assert_eq!(session.append(Body::Action(Action::New)).unwrap(), 0);
        assert_eq!(session.append(Body::Event(Event::Saved(1))).unwrap(), 1);
        assert_eq!(
            session
                .append(Body::Event(Event::Failed("boom".into())))
                .unwrap(),
            2
        );

        let records = read(session.path()).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].body, Body::Action(Action::New));
        assert_eq!(records[2].body, Body::Event(Event::Failed("boom".into())));

        // Reopening continues the sequence instead of restarting it.
        drop(session);
        let mut session = Session::open(session_path(&dir)).unwrap();
        assert_eq!(session.seq(), 3);
        assert_eq!(session.append(Body::Action(Action::Select(1))).unwrap(), 3);

        // A crash mid-write leaves a partial line; replay keeps the good ones.
        let path = session.path().to_path_buf();
        drop(session);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"seq\":9,\"ts_ms\":1,\"kind\":\"Act")
            .unwrap();
        drop(file);

        let records = read(&path).unwrap();
        assert_eq!(records.len(), 4, "torn tail skipped");
        assert_eq!(
            records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn client_writes_actions_and_events_into_the_log() {
        let dir = temp_dir();
        let session = Session::create(dir.join("sessions")).unwrap();
        let path = session.path().to_path_buf();

        let mut client = smash_client::Client::with_hooks(
            smash_backend::Files::new(dir.join("data")),
            Box::new(session),
        );
        client.start();
        client.dispatch(Action::New);

        let records = read(&path).unwrap();
        assert!(matches!(records[0].body, Body::Event(Event::Loaded(_))));
        assert!(
            records
                .iter()
                .any(|r| matches!(r.body, Body::Action(Action::New)))
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(r.body, Body::Event(Event::Saved(1))))
        );
        assert_eq!(
            records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn session_path(dir: &Path) -> PathBuf {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path()
    }
}
