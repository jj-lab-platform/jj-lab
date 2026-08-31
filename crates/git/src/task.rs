//! Unified in-process background task registry for the `/ops` surface.
//!
//! A build / helm-install / publish step is long-running and async from the
//! HTTP caller. `TaskRegistry` tracks those tasks in memory: state transitions,
//! a bounded log tail, and a broadcast bus so handlers can SSE the live
//! progress. Tasks are process-local and ephemeral by design (a restart drops
//! them); durable state stays in the sqlite run/job tables where it matters.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::broadcast;

/// A task kind (build / helm-install / publish). Used for listing/filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Build,
    Helm,
    Publish,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Build => "build",
            TaskKind::Helm => "helm",
            TaskKind::Publish => "publish",
        }
    }
}

/// One line of task progress (SSE event payload).
#[derive(Debug, Clone, Serialize)]
pub struct TaskEvent {
    /// "log" | "state"
    pub event: String,
    /// state name (for "state"), or the log line (for "log").
    pub data: String,
}

/// An in-flight/finished task tracked by the registry.
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub state: Mutex<TaskState>,
    logs: Mutex<Vec<String>>,
    tx: broadcast::Sender<TaskEvent>,
}

#[derive(Debug, Clone)]
pub struct TaskState {
    /// "running" | "done" | "failed"
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
}

impl Task {
    fn new(id: String, kind: TaskKind) -> Self {
        let (tx, _) = broadcast::channel(256);
        Task {
            id,
            kind,
            state: Mutex::new(TaskState { status: "running".into(), result: None, error: None }),
            logs: Mutex::new(Vec::new()),
            tx,
        }
    }

    /// Append a log line (bounded to the last 500 lines) and broadcast it.
    pub fn log(&self, line: &str) {
        let mut logs = self.logs.lock().expect("task logs poisoned");
        logs.push(line.to_string());
        while logs.len() > 500 {
            logs.remove(0);
        }
        drop(logs);
        let _ = self.tx.send(TaskEvent { event: "log".into(), data: line.to_string() });
    }

    /// Finalize into `done` or `failed` (with an optional result string) and
    /// broadcast the state change.
    pub fn finish(&self, ok: bool, result: Option<String>, error: Option<String>) {
        {
            let mut s = self.state.lock().expect("task state poisoned");
            s.status = if ok { "done" } else { "failed" }.into();
            s.result = result;
            s.error = error;
        }
        let status = self.status();
        let _ = self.tx.send(TaskEvent { event: "state".into(), data: status });
    }

    pub fn status(&self) -> String {
        self.state.lock().expect("task state poisoned").status.clone()
    }

    pub fn snapshot_logs(&self) -> Vec<String> {
        self.logs.lock().expect("task logs poisoned").clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.tx.subscribe()
    }

    /// JSON summary for list/get endpoints.
    pub fn summary(&self) -> serde_json::Value {
        let s = self.state.lock().expect("task state poisoned").clone();
        serde_json::json!({
            "id": self.id,
            "kind": self.kind.as_str(),
            "status": s.status,
            "result": s.result,
            "error": s.error,
        })
    }
}

/// Process-local registry, shared via `AppState`.
pub struct TaskRegistry {
    inner: Mutex<HashMap<String, Arc<Task>>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        TaskRegistry { inner: Mutex::new(HashMap::new()) }
    }

    /// Generate a fresh, collision-resistant task id.
    pub fn new_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Register a new task and return its handle.
    pub fn create(&self, id: String, kind: TaskKind) -> Arc<Task> {
        let task = Arc::new(Task::new(id, kind));
        self.inner.lock().expect("task registry poisoned").insert(task.id.clone(), task.clone());
        task
    }

    pub fn get(&self, id: &str) -> Option<Arc<Task>> {
        self.inner.lock().expect("task registry poisoned").get(id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<Task>> {
        self.inner.lock().expect("task registry poisoned").values().cloned().collect()
    }
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}