use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    process::{Child, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Update {
    pub generation: u64,
    pub view: Option<View>,
    pub notice: String,
    pub finished: bool,
    pub error: bool,
}

pub struct Session {
    pub updates: watch::Receiver<Arc<Update>>,
    context: watch::Sender<Context>,
    actions: mpsc::SyncSender<HostMessage>,
    stop: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    pub fn start(
        plugin: Plugin,
        command: Command,
        context: Context,
        paths: Paths,
        notify: Arc<Notify>,
    ) -> Self {
        let (contexts, latest_context) = watch::channel(context);
        let (actions, requests) = mpsc::sync_channel(8);
        let (output, updates) = watch::channel(Arc::new(Update::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut update = Update::default();
            let ending_context = latest_context.clone();
            let result = run(
                plugin,
                command,
                paths,
                latest_context,
                requests,
                &stopping,
                |message| {
                    match message {
                        PluginMessage::View { generation, view } => {
                            update.generation = generation;
                            update.view = Some(view);
                            update.notice.clear();
                        }
                        PluginMessage::Notice {
                            generation,
                            message,
                        } => {
                            if update.generation != generation {
                                update.view = None;
                            }
                            update.generation = generation;
                            update.notice = message;
                        }
                        PluginMessage::Done { generation } => {
                            if update.generation != generation {
                                update.view = None;
                            }
                            update.generation = generation;
                        }
                        _ => (),
                    }
                    output.send_replace(Arc::new(update.clone()));
                    notify.notify_one();
                },
            );
            let generation = ending_context.borrow().generation;
            if update.generation != generation {
                update.view = None;
                update.notice.clear();
                update.generation = generation;
            }
            update.finished = true;
            if let Err(error) = result {
                update.error = true;
                update.notice = format!("{error:#}");
            }
            output.send_replace(Arc::new(update));
            notify.notify_one();
        });
        Self {
            updates,
            context: contexts,
            actions,
            stop,
            task: Some(task),
        }
    }
    pub fn context(&self, context: Context) {
        self.context.send_replace(context);
    }
    pub fn action(&self, id: String, generation: u64) -> Result<()> {
        self.actions
            .try_send(HostMessage::Action { id, generation })
            .context("Plugin is busy or has exited")
    }
    pub async fn close(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        // Also reap descendants retaining pipes after their parent exits.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            bail!("Plugin closed stdout mid-message");
        }
        let count = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |at| at + 1);
        if line.len() + count > MAX_MESSAGE {
            bail!("Plugin message exceeds 1 MiB");
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if line.last() == Some(&b'\n') {
            return Ok(Some(line));
        }
    }
}

fn send(writer: &mpsc::SyncSender<Vec<u8>>, message: HostMessage) -> Result<()> {
    let mut bytes = serde_json::to_vec(&message)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_MESSAGE {
        bail!("Host message exceeds 1 MiB");
    }
    writer
        .try_send(bytes)
        .context("Plugin is not reading its input")
}

fn run(
    plugin: Plugin,
    command: Command,
    paths: Paths,
    mut context: watch::Receiver<Context>,
    actions: mpsc::Receiver<HostMessage>,
    stop: &AtomicBool,
    mut publish: impl FnMut(PluginMessage),
) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let directory = plugin.path.parent().context("Missing plugin directory")?;
    let program = &plugin.manifest.exec[0];
    let executable = if Path::new(program).is_absolute() {
        PathBuf::from(program)
    } else if program.contains('/') {
        directory.join(program)
    } else {
        crate::subprocess::executable(None, program)?
    };
    let data_dir = paths.data.join("plugin-data").join(&plugin.manifest.id);
    let cache_dir = paths
        .cache
        .parent()
        .context("Missing cache parent")?
        .join("plugins")
        .join(&plugin.manifest.id);
    platform::private_dir(&data_dir)?;
    platform::private_dir(&cache_dir)?;
    let mut process = Process(
        std::process::Command::new(executable)
            .args(&plugin.manifest.exec[1..])
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("Cannot start plugin")?,
    );
    let mut stdin = process.0.stdin.take().unwrap();
    let stdout = process.0.stdout.take().unwrap();
    let mut stderr = process.0.stderr.take().unwrap();
    let (writer, writes) = mpsc::sync_channel::<Vec<u8>>(8);
    let input_task = thread::spawn(move || {
        while let Ok(bytes) = writes.recv() {
            if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    });
    let (lines, received) = mpsc::sync_channel(8);
    let output_task = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let line = read_line(&mut reader);
            let done = !matches!(&line, Ok(Some(_)));
            if lines.send(line).is_err() || done {
                break;
            }
        }
    });
    let diagnostics = thread::spawn(move || {
        let mut tail = Vec::new();
        let mut bytes = [0; 4096];
        while let Ok(n) = stderr.read(&mut bytes) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&bytes[..n]);
            if tail.len() > 8192 {
                tail.drain(..tail.len() - 8192);
            }
        }
        String::from_utf8_lossy(&tail)
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect::<String>()
    });
    let result = (|| -> Result<()> {
        send(
            &writer,
            HostMessage::Init {
                api_version: API_VERSION,
                data_dir,
                cache_dir,
                vtamp: std::env::current_exe()?,
            },
        )?;
        let started = Instant::now();
        let mut ready = false;
        let mut last_publish = Instant::now() - Duration::from_secs(1);
        let mut pending_view = None;
        let mut pending_notice = None;
        loop {
            if stop.load(Ordering::Relaxed) {
                let _ = send(&writer, HostMessage::Shutdown);
                let deadline = Instant::now() + Duration::from_millis(500);
                while Instant::now() < deadline {
                    if process.0.try_wait()?.is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                return Ok(());
            }
            if !ready && started.elapsed() >= Duration::from_secs(5) {
                bail!("Plugin handshake timed out");
            }
            if ready && context.has_changed().unwrap_or(false) {
                let latest = context.borrow_and_update().clone();
                pending_view = pending_view.take().filter(|message| matches!(message, PluginMessage::View {generation, ..} if *generation == latest.generation));
                pending_notice = pending_notice.take().filter(|message| matches!(message, PluginMessage::Notice {generation, ..} if *generation == latest.generation));
                send(&writer, HostMessage::Context { context: latest })?;
            }
            if ready
                && let Ok(action) = actions.try_recv()
                && matches!(&action, HostMessage::Action {generation, ..} if *generation == context.borrow().generation)
            {
                send(&writer, action)?;
            }
            if last_publish.elapsed() >= Duration::from_millis(50) {
                if let Some(view) = pending_view.take() {
                    publish(view);
                }
                if let Some(notice) = pending_notice.take() {
                    publish(notice);
                }
                last_publish = Instant::now();
            }
            let line = match received.recv_timeout(Duration::from_millis(10)) {
                Ok(line) => line?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => bail!("Plugin output reader stopped"),
            };
            let Some(line) = line else {
                bail!("Plugin exited without a done message");
            };
            let message: PluginMessage =
                serde_json::from_slice(&line).context("Invalid plugin message")?;
            if !ready {
                if !matches!(
                    message,
                    PluginMessage::Ready {
                        api_version: API_VERSION
                    }
                ) {
                    bail!("Expected plugin API 1 ready message");
                }
                ready = true;
                send(
                    &writer,
                    HostMessage::Invoke {
                        command: command.id.clone(),
                        context: context.borrow_and_update().clone(),
                    },
                )?;
                continue;
            }
            let generation = context.borrow().generation;
            match message {
                PluginMessage::Ready { .. } => bail!("Duplicate plugin handshake"),
                PluginMessage::View {
                    generation: incoming,
                    ref view,
                } if incoming == generation => {
                    view.validate()?;
                    pending_view = Some(message);
                    pending_notice = None;
                }
                PluginMessage::Notice {
                    generation: incoming,
                    ref message,
                } if incoming == generation => {
                    if message.chars().any(|c| c.is_control() && c != '\n') {
                        bail!("Control characters in plugin notice");
                    }
                    pending_notice = Some(PluginMessage::Notice {
                        generation: incoming,
                        message: message.clone(),
                    });
                }
                PluginMessage::Done {
                    generation: incoming,
                } if incoming == generation => {
                    if let Some(view) = pending_view.take() {
                        publish(view);
                    }
                    if let Some(notice) = pending_notice.take() {
                        publish(notice);
                    }
                    publish(PluginMessage::Done { generation });
                    return Ok(());
                }
                _ => (), // A previous track's result cannot replace the current panel.
            }
        }
    })();
    drop(process);
    drop(writer);
    drop(received);
    let _ = input_task.join();
    let _ = output_task.join();
    let tail = diagnostics.join().unwrap_or_default();
    result.with_context(|| {
        if tail.is_empty() {
            "Plugin session ended".into()
        } else {
            format!("Plugin session ended: {tail}")
        }
    })
}
