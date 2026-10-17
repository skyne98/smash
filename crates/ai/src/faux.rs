//! Scripted stream for UI work and tests; no network, no key.

use crate::{Event, EventStream};

/// Replays `events` as a stream.
pub fn faux(events: Vec<Event>) -> EventStream {
    Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
}
