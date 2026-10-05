//! Opt-in timing trace. Never write diagnostic data to the terminal or wait on
//! disk from the UI/decoder threads, and never record keys or media metadata.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static TRACE: OnceLock<Trace> = OnceLock::new();
struct Trace {
    started: Instant,
    sender: SyncSender<Value>,
    dropped: Arc<AtomicU64>,
    next_span: AtomicU64,
}

pub(super) struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        record("session.end", || json!({}));
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) fn start() -> Result<Option<Session>> {
    let Some(path) = std::env::var_os("VTAMP_TUI_TRACE").filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .context("Cannot open VTAMP_TUI_TRACE")?;
    let (sender, receiver) = mpsc::sync_channel(4096);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let dropped = Arc::new(AtomicU64::new(0));
    let lost = dropped.clone();
    let thread = std::thread::Builder::new()
        .name("tui-trace".into())
        .spawn(move || {
            let mut writer = BufWriter::new(file);
            let mut flushed = Instant::now();
            loop {
                if stopped.load(Ordering::Relaxed) {
                    for entry in receiver.try_iter() {
                        if write_entry(&mut writer, &entry).is_err() {
                            break;
                        }
                    }
                    let _ = write_entry(
                        &mut writer,
                        &json!({"event":"trace.dropped", "count":lost.load(Ordering::Relaxed)}),
                    );
                    let _ = writer.flush();
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(200)) {
                    Ok(entry) => {
                        if write_entry(&mut writer, &entry).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                }
                if flushed.elapsed() >= Duration::from_millis(200) {
                    if writer.flush().is_err() {
                        break;
                    }
                    flushed = Instant::now();
                }
            }
        })?;
    let _ = TRACE.set(Trace {
        started: Instant::now(),
        sender,
        dropped,
        next_span: AtomicU64::new(1),
    });
    record("session.start", || {
        json!({
            "pid":std::process::id(),
            "unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "debug":cfg!(debug_assertions),
            "tmux":std::env::var_os("TMUX").is_some(),
        })
    });
    Ok(Some(Session {
        stop,
        thread: Some(thread),
    }))
}

fn write_entry(writer: &mut impl Write, entry: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, entry)?;
    writer.write_all(b"\n")
}

impl Trace {
    fn record(&self, event: &'static str, fields: Value) {
        let entry = json!({"us":self.started.elapsed().as_micros(), "event":event, "data":fields});
        if self.sender.try_send(entry).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
pub(super) fn record(event: &'static str, fields: impl FnOnce() -> Value) {
    if let Some(trace) = TRACE.get() {
        trace.record(event, fields());
    }
}

pub(super) struct Span(Option<(u64, Instant)>);
pub(super) fn span(name: &'static str, fields: impl FnOnce() -> Value) -> Span {
    let Some(trace) = TRACE.get() else {
        return Span(None);
    };
    let id = trace.next_span.fetch_add(1, Ordering::Relaxed);
    trace.record("span.begin", json!({"id":id,"name":name,"fields":fields()}));
    Span(Some((id, Instant::now())))
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some((id, started)) = self.0 {
            record(
                "span.end",
                || json!({"id":id,"duration_us":started.elapsed().as_micros()}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_full_trace_queue_drops_diagnostics_instead_of_blocking_output() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let trace = Trace {
            started: Instant::now(),
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
            next_span: AtomicU64::new(1),
        };
        trace.record("first", json!({"bytes":42}));
        trace.record("second", json!({}));
        assert_eq!(trace.dropped.load(Ordering::Relaxed), 1);
        let entry = receiver.try_recv().unwrap();
        let mut output = vec![];
        write_entry(&mut output, &entry).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["data"]["bytes"],
            42
        );
        assert_eq!(output.last(), Some(&b'\n'));
    }
}
