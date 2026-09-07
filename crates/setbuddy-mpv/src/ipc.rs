//! JSON IPC transport for a single mpv process.
//!
//! mpv speaks newline-delimited JSON over a unix socket. Replies are correlated
//! by `request_id`; everything else is an asynchronous event, including the
//! `property-change` notifications we subscribe to with `observe_property`.
//!
//! One mutex guards the whole bus and one condvar wakes every waiter. That is
//! deliberate: replies, property updates, and event generations all arrive on the
//! same reader thread, so splitting them across locks would buy nothing and make
//! the wait logic subtly wrong.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Properties we ask mpv to push at us. Everything the UI needs to render a
/// now-playing row is here, so the common path never round-trips.
pub(crate) const OBSERVED: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "eof-reached",
    "path",
    "vid",
    "current-vo",
    "core-idle",
    "idle-active",
    "speed",
    "volume",
];

#[derive(Default)]
struct Bus {
    replies: HashMap<u64, Value>,
    props: HashMap<String, Value>,
    /// Bumped on each `file-loaded`, so a loader can wait for *its* load.
    file_loaded_gen: u64,
    /// Bumped on each `end-file`.
    end_file_gen: u64,
    connected: bool,
    /// Why the connection ended, if it did.
    disconnect_reason: Option<String>,
    /// Request ids whose caller gave up. A reply arriving for one of these is
    /// discarded rather than stored for a reader that will never come.
    abandoned: HashSet<u64>,
}

impl Bus {
    /// Stop waiting for `rid`: drop anything already buffered for it, and
    /// remember to drop a reply that arrives later.
    fn abandon(&mut self, rid: u64) {
        self.replies.remove(&rid);
        self.abandoned.insert(rid);
    }
}

pub(crate) struct Ipc {
    writer: Mutex<UnixStream>,
    bus: Mutex<Bus>,
    cv: Condvar,
    next_id: AtomicU64,
    reader: Mutex<Option<JoinHandle<()>>>,
}

/// A reply that carries mpv's `error` field verbatim.
pub(crate) struct Reply {
    pub data: Value,
    pub error: String,
}

impl Reply {
    pub fn ok(&self) -> bool {
        self.error == "success"
    }
}

impl Ipc {
    /// Connect to an already-listening mpv socket and start the reader thread.
    pub fn connect(stream: UnixStream) -> std::io::Result<Arc<Self>> {
        let read_half = stream.try_clone()?;
        let ipc = Arc::new(Ipc {
            writer: Mutex::new(stream),
            bus: Mutex::new(Bus {
                connected: true,
                ..Bus::default()
            }),
            cv: Condvar::new(),
            next_id: AtomicU64::new(1),
            reader: Mutex::new(None),
        });

        let weak = Arc::downgrade(&ipc);
        let handle = std::thread::Builder::new()
            .name("setbuddy-mpv-ipc".into())
            .spawn(move || {
                let mut lines = BufReader::new(read_half).lines();
                let reason = loop {
                    match lines.next() {
                        None => break "mpv closed the socket".to_string(),
                        Some(Err(e)) => break format!("socket read failed: {e}"),
                        Some(Ok(line)) => {
                            let Some(ipc) = weak.upgrade() else { return };
                            if line.trim().is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<Value>(&line) {
                                Ok(msg) => ipc.dispatch(msg),
                                // A malformed line is mpv's problem, not a reason
                                // to tear down a working connection.
                                Err(_) => continue,
                            }
                        }
                    }
                };
                if let Some(ipc) = weak.upgrade() {
                    ipc.mark_disconnected(reason);
                }
            })?;
        *ipc.reader.lock().expect("ipc reader lock poisoned") = Some(handle);
        Ok(ipc)
    }

    fn dispatch(&self, msg: Value) {
        let mut bus = self.bus.lock().expect("ipc bus poisoned");
        if let Some(rid) = msg.get("request_id").and_then(Value::as_u64) {
            // A reply to a request nobody is waiting for any more is dropped.
            // Keeping it would grow `replies` for the life of the process, one
            // entry per timeout — and mpv answers late exactly when it is
            // struggling, which is when a leak matters most.
            if bus.abandoned.remove(&rid) {
                return;
            }
            bus.replies.insert(rid, msg);
            self.cv.notify_all();
            return;
        }
        match msg.get("event").and_then(Value::as_str) {
            Some("property-change") => {
                if let Some(name) = msg.get("name").and_then(Value::as_str) {
                    // A missing `data` key means the property became unavailable
                    // (e.g. `current-vo` when video is switched off) — record it
                    // as null rather than leaving a stale value behind.
                    let data = msg.get("data").cloned().unwrap_or(Value::Null);
                    bus.props.insert(name.to_string(), data);
                }
            }
            Some("file-loaded") => bus.file_loaded_gen += 1,
            Some("end-file") => bus.end_file_gen += 1,
            _ => {}
        }
        self.cv.notify_all();
    }

    fn mark_disconnected(&self, reason: String) {
        let mut bus = self.bus.lock().expect("ipc bus poisoned");
        bus.connected = false;
        bus.disconnect_reason = Some(reason);
        // Anyone blocked on a reply that will now never arrive must wake up.
        self.cv.notify_all();
    }

    pub fn is_connected(&self) -> bool {
        self.bus.lock().expect("ipc bus poisoned").connected
    }

    pub fn disconnect_reason(&self) -> Option<String> {
        self.bus
            .lock()
            .expect("ipc bus poisoned")
            .disconnect_reason
            .clone()
    }

    /// Send a command and block until mpv replies or `timeout` elapses.
    pub fn request(&self, args: Vec<Value>, timeout: Duration) -> Result<Reply, IpcError> {
        let rid = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = serde_json::to_string(&json!({ "command": args, "request_id": rid }))
            .map_err(|e| IpcError::Encode(e.to_string()))?;

        {
            let mut w = self.writer.lock().expect("ipc writer poisoned");
            w.write_all(line.as_bytes())
                .and_then(|_| w.write_all(b"\n"))
                .and_then(|_| w.flush())
                .map_err(|e| IpcError::Disconnected(format!("write failed: {e}")))?;
        }

        let deadline = Instant::now() + timeout;
        let mut bus = self.bus.lock().expect("ipc bus poisoned");
        loop {
            if let Some(msg) = bus.replies.remove(&rid) {
                return Ok(Reply {
                    data: msg.get("data").cloned().unwrap_or(Value::Null),
                    error: msg
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("success")
                        .to_string(),
                });
            }
            if !bus.connected {
                bus.abandon(rid);
                return Err(IpcError::Disconnected(
                    bus.disconnect_reason
                        .clone()
                        .unwrap_or_else(|| "mpv is gone".into()),
                ));
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                bus.abandon(rid);
                return Err(IpcError::Timeout);
            };
            let (guard, wait) = self
                .cv
                .wait_timeout(bus, remaining)
                .expect("ipc bus poisoned");
            bus = guard;
            if wait.timed_out() && !bus.replies.contains_key(&rid) {
                bus.abandon(rid);
                return Err(IpcError::Timeout);
            }
        }
    }

    /// Record a value we just set successfully.
    ///
    /// mpv writes the command reply before the corresponding `property-change`
    /// event, so between the two there is a window where the observed value is
    /// stale — long enough for a UI that polls straight after a pause to draw
    /// "playing". A successful `set_property` reply means mpv has applied the
    /// change, so seeding the cache with it is accurate, not optimistic.
    pub fn cache_prop(&self, name: &str, value: Value) {
        self.bus
            .lock()
            .expect("ipc bus poisoned")
            .props
            .insert(name.to_string(), value);
    }

    /// Latest observed value of a property, or `Value::Null`.
    pub fn prop(&self, name: &str) -> Value {
        self.bus
            .lock()
            .expect("ipc bus poisoned")
            .props
            .get(name)
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub fn file_loaded_generation(&self) -> u64 {
        self.bus.lock().expect("ipc bus poisoned").file_loaded_gen
    }

    /// Block until `file-loaded` fires past `since`. Returns false on timeout.
    pub fn wait_file_loaded(&self, since: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut bus = self.bus.lock().expect("ipc bus poisoned");
        loop {
            if bus.file_loaded_gen > since {
                return true;
            }
            if !bus.connected {
                return false;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (guard, wait) = self
                .cv
                .wait_timeout(bus, remaining)
                .expect("ipc bus poisoned");
            bus = guard;
            if wait.timed_out() && bus.file_loaded_gen <= since {
                return false;
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum IpcError {
    #[error("mpv did not reply in time")]
    Timeout,
    #[error("{0}")]
    Disconnected(String),
    #[error("could not encode command: {0}")]
    Encode(String),
}
