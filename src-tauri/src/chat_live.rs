//! One chat generation's live channel: text out while it is being written, and
//! Stop in while it still matters.
//!
//! `chat_answer_stream` runs the whole pipeline (retrieve → prompt → generate
//! → verify citations → persist) inside one blocking call, so the UI used to sit
//! on a spinner and then type the finished answer out at 6 characters per tick —
//! a preview of a result that already existed. Two things now leave that
//! pipeline early:
//!
//! - **deltas** (`chat:delta`), the provider's text as it arrives, coalesced so
//!   a token-by-token stream doesn't become one IPC call and one re-render per
//!   token;
//! - **cancellation** (`chat_cancel`), because stopping has to reach the
//!   provider while it is still generating — after the answer is finished,
//!   "stop" is only a UI gesture.
//!
//! The returned [`crate::models::ChatAnswer`] is still the single source of
//! truth: it is the post-processed text (fabricated citation tokens stripped,
//! state classified, citations frozen). Streamed text is a PREVIEW that the
//! bubble replaces when the real answer lands — never a second truth.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Event carrying one streamed piece. Payload: [`ChatDelta`].
pub const DELTA_EVENT: &str = "chat:delta";

/// Coalescing window. ~16 paints/s reads as continuous to a human and keeps a
/// fast model from turning the whole reply into per-token IPC traffic.
const FLUSH_MS: u64 = 60;
/// …and a hard cap, so a provider that sends one huge chunk per second still
/// shows something before the window closes.
const FLUSH_CHARS: usize = 240;

/// What the frontend receives for each streamed piece.
#[derive(Clone, serde::Serialize)]
pub struct ChatDelta {
    /// Echoed back so the UI can ignore deltas from a run it already abandoned
    /// (a second question sent after Stop, or another subject's panel).
    pub run_id: String,
    pub text: String,
}

struct Pending {
    text: String,
    since: Instant,
}

/// A generation the UI can watch and stop.
pub struct LiveRun {
    cancel: Arc<AtomicBool>,
    /// Where a flushed piece goes. A closure rather than an `AppHandle` so the
    /// coalescing rules can be tested without a Tauri app.
    emit: Box<dyn Fn(&str) + Send + Sync>,
    pending: Mutex<Pending>,
}

impl LiveRun {
    pub fn new(
        cancel: Arc<AtomicBool>,
        emit: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            cancel,
            emit: Box::new(emit),
            pending: Mutex::new(Pending { text: String::new(), since: Instant::now() }),
        }
    }

    /// Has the user pressed Stop? The stream callback reads this on every chunk,
    /// so the answer stops at the next token instead of at the next request.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Take one piece of streamed text. Returns false when generation must stop —
    /// that return value is what [`crate::llm::Llm::complete_stream`] forwards to
    /// the provider.
    pub fn push(&self, text: &str) -> bool {
        if self.cancelled() {
            return false;
        }
        let mut p = self.pending.lock().unwrap();
        p.text.push_str(text);
        if p.text.chars().count() >= FLUSH_CHARS
            || p.since.elapsed() >= Duration::from_millis(FLUSH_MS)
        {
            flush_locked(&mut p, &*self.emit);
        }
        !self.cancelled()
    }

    /// Send whatever is still buffered. Called when generation ends, so the last
    /// <60ms of text isn't stranded in the buffer.
    pub fn flush(&self) {
        flush_locked(&mut self.pending.lock().unwrap(), &*self.emit);
    }
}

fn flush_locked(p: &mut Pending, emit: &(dyn Fn(&str) + Send + Sync)) {
    if p.text.is_empty() {
        return;
    }
    emit(&p.text);
    p.text.clear();
    p.since = Instant::now();
}

fn runs() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static RUNS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a run and return its live channel. Events go to `app`.
pub fn start(app: AppHandle, run_id: &str) -> LiveRun {
    let cancel = Arc::new(AtomicBool::new(false));
    runs().lock().unwrap().insert(run_id.to_string(), cancel.clone());
    let id = run_id.to_string();
    let handle = app.clone();
    LiveRun::new(cancel, move |text: &str| {
        let _ = handle.emit(
            DELTA_EVENT,
            ChatDelta { run_id: id.clone(), text: text.to_string() },
        );
    })
}

/// Flag a run as stopped. Returns false when nothing was running under that id —
/// a Stop that arrives after the answer landed is a no-op, not an error.
pub fn cancel(run_id: &str) -> bool {
    match runs().lock().unwrap().get(run_id) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Drop the run. Always called, including on error, so a failed generation
/// doesn't leave an id in the map forever.
pub fn end(run_id: &str) {
    runs().lock().unwrap().remove(run_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str) + Send + Sync + 'static) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let emit = move |t: &str| sink.lock().unwrap().push(t.to_string());
        (seen, emit)
    }

    #[test]
    fn nothing_is_lost_between_the_last_chunk_and_the_final_flush() {
        // The coalescing window is wall-clock based, so what a unit test can hold
        // is the contract that matters to the reader: whatever the window did,
        // the pieces arrive in order, whole, and no empty event is emitted.
        let (seen, emit) = collector();
        let run = LiveRun::new(Arc::new(AtomicBool::new(false)), emit);
        assert!(run.push("光"));
        assert!(run.push("合"));
        assert!(run.push("作"));
        assert!(run.push("用"));
        run.flush();
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.concat(), "光合作用");
        assert!(events.iter().all(|e| !e.is_empty()), "no empty event reaches the UI");
        assert!(events.len() <= 4, "coalescing must not duplicate text");
    }

    #[test]
    fn a_big_chunk_flushes_immediately_instead_of_waiting_out_the_window() {
        let (seen, emit) = collector();
        let run = LiveRun::new(Arc::new(AtomicBool::new(false)), emit);
        let big = "字".repeat(FLUSH_CHARS);
        assert!(run.push(&big));
        assert_eq!(seen.lock().unwrap().len(), 1, "the cap wins over the timer");
        run.flush();
        assert_eq!(seen.lock().unwrap().len(), 1, "flush of an empty buffer emits nothing");
    }

    #[test]
    fn stop_is_visible_to_the_next_chunk_and_nothing_more_is_emitted() {
        let (seen, emit) = collector();
        let flag = Arc::new(AtomicBool::new(false));
        let run = LiveRun::new(flag.clone(), emit);
        assert!(run.push("第一句"));
        run.flush();
        flag.store(true, Ordering::Relaxed);
        assert!(!run.push("第二句"), "the provider must be told to stop");
        run.flush();
        assert_eq!(*seen.lock().unwrap(), vec!["第一句"], "text after Stop never reaches the UI");
    }

    #[test]
    fn cancelling_a_finished_run_is_a_no_op_not_an_error() {
        assert!(!cancel("never-registered"));
        let flag = Arc::new(AtomicBool::new(false));
        let (_, emit) = collector();
        let run = LiveRun::new(flag.clone(), emit);
        runs().lock().unwrap().insert("r4".into(), flag.clone());
        assert!(cancel("r4"));
        assert!(run.cancelled());
        end("r4");
        assert!(!cancel("r4"), "the id is gone once the run is over");
    }
}
