//! Jobs the UI started and follows: a push, a pull, a bulk action. A route
//! called with `?async=1` registers one here and answers at once; the job
//! runs on a thread of its own and the UI polls `/api/tasks` for it.
//!
//! The registry lock is bookkeeping only and is never held while work runs.
//! What keeps two jobs off the same store is a lane: one for the hub (push,
//! pull, compare), one for local changes. A job waiting for its lane is
//! `queued`; once it holds it, `running`.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::Value;

/// Finished jobs beyond this many are forgotten, oldest first.
const KEEP: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub enum Lane {
    Hub,
    Local,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: u64,
    pub kind: &'static str,
    /// What makes two requests the same job: a second one while this is
    /// queued or running is answered with this job, and starts nothing.
    pub key: String,
    pub label: String,
    pub state: State,
    pub done: u64,
    /// 0 while unknown.
    pub total: u64,
    pub started: String,
    pub ended: Option<String>,
    /// Exactly the body the route answers without `?async`.
    pub result: Option<Value>,
    /// The message the route would have answered as an HTTP error.
    pub error: Option<String>,
}

// ponytail: process-wide statics, like the index progress in `api`; give
// them to the router as state if the web ever serves more than one store.
/// Newest first.
static JOBS: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static HUB: Mutex<()> = Mutex::new(());
static LOCAL: Mutex<()> = Mutex::new(());

fn lane(lane: Lane) -> &'static Mutex<()> {
    match lane {
        Lane::Hub => &HUB,
        Lane::Local => &LOCAL,
    }
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn update(id: u64, f: impl FnOnce(&mut Job)) {
    if let Some(job) = JOBS.lock().unwrap().iter_mut().find(|j| j.id == id) {
        f(job);
    }
}

fn active(state: State) -> bool {
    matches!(state, State::Queued | State::Running)
}

/// Start `work` as a job, or find the one with this `key` that has not
/// finished. Returns its id and whether it already existed.
/// `work` is handed a `progress(done, total)` to call as it goes.
pub fn submit(
    kind: &'static str,
    key: String,
    label: String,
    total: u64,
    on: Lane,
    work: impl FnOnce(&mut dyn FnMut(usize, usize)) -> Result<Value, String> + Send + 'static,
) -> (u64, bool) {
    let id = {
        let mut jobs = JOBS.lock().unwrap();
        if let Some(job) = jobs.iter().find(|j| j.key == key && active(j.state)) {
            return (job.id, true);
        }
        // Counted from the start time, so a page that outlived a restart
        // never mistakes a new job for the one it was waiting on.
        static START: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
        });
        let id = *START + NEXT_ID.fetch_add(1, Ordering::SeqCst);
        jobs.push_front(Job {
            id,
            kind,
            key,
            label,
            state: State::Queued,
            done: 0,
            total,
            started: now(),
            ended: None,
            result: None,
            error: None,
        });
        // Only finished jobs are forgotten: one still running stays findable.
        while jobs.len() > KEEP {
            match jobs.iter().rposition(|j| !active(j.state)) {
                Some(oldest) => jobs.remove(oldest),
                None => break,
            };
        }
        id
    };
    std::thread::spawn(move || {
        // A job that panicked held the lane; the next one may still run.
        let _held = lane(on).lock().unwrap_or_else(|e| e.into_inner());
        update(id, |j| j.state = State::Running);
        let mut progress = |done: usize, total: usize| update(id, |j| (j.done, j.total) = (done as u64, total as u64));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&mut progress)))
            .unwrap_or_else(|_| Err("the task stopped unexpectedly".into()));
        update(id, |j| {
            j.ended = Some(now());
            match outcome {
                Ok(result) => (j.state, j.result) = (State::Ok, Some(result)),
                Err(error) => (j.state, j.error) = (State::Failed, Some(error)),
            }
        });
    });
    (id, false)
}

/// Every job remembered, newest first.
pub fn list() -> Vec<Job> {
    JOBS.lock().unwrap().iter().cloned().collect()
}

pub fn get(id: u64) -> Option<Job> {
    JOBS.lock().unwrap().iter().find(|j| j.id == id).cloned()
}

/// Hold a lane until the returned value is dropped, so a test can see a job
/// wait in `queued`.
#[doc(hidden)]
pub struct Held(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[doc(hidden)]
pub fn hold(on: Lane) -> Held {
    Held(lane(on).lock().unwrap_or_else(|e| e.into_inner()))
}
