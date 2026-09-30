use crate::{
    audio::RodioBackend,
    engine::Engine,
    library::{self, Scan},
    model::*,
    platform::Paths,
    store::Store,
    wire,
};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Notify, Semaphore, broadcast, oneshot},
};

type Answer = oneshot::Sender<Reply>;
enum Work {
    Request(Command, Answer),
    Catalog(Scan),
    Imported {
        scan: Scan,
        play: bool,
        answer: Answer,
    },
}

pub async fn run(paths: Paths) -> Result<()> {
    paths.prepare()?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.runtime.join("server.lock"))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let socket = paths.socket();
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let store = Store::open(&paths.database())?;
    let state = store.restore()?;
    let listener = UnixListener::bind(&socket).context("Cannot bind the control socket")?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let (sender, receiver) = mpsc::sync_channel(64);
    let (events, _) = broadcast::channel(64);
    let media_commands = sender.clone();
    let media = crate::media_controls::Controls::new(move |command| {
        let (answer, _) = oneshot::channel();
        media_commands
            .try_send(Work::Request(command, answer))
            .is_ok()
    });
    let shutdown = Arc::new(Notify::new());
    let thread = {
        let events = events.clone();
        let shutdown = shutdown.clone();
        let tx = sender.clone();
        std::thread::Builder::new()
            .name("vtamp-player".into())
            .spawn(move || {
                let result = worker(
                    paths,
                    store,
                    Engine::new(state, RodioBackend::default()),
                    receiver,
                    tx,
                    &events,
                    media,
                );
                if let Err(error) = result {
                    tracing::error!("Player stopped: {error:#}");
                }
                let _ = events.send(Event::Shutdown);
                shutdown.notify_one();
            })?
    };
    let permits = Arc::new(Semaphore::new(64));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tracing::info!("vtamp server ready");
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = terminate.recv() => { let _ = dispatch(&sender, Command::Shutdown).await; },
            _ = interrupt.recv() => { let _ = dispatch(&sender, Command::Shutdown).await; },
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if let Ok(permit) = permits.clone().try_acquire_owned() {
                    let sender = sender.clone(); let events = events.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = connection(stream, sender, events).await { tracing::debug!("Client disconnected: {error:#}"); }
                    });
                }
            }
        }
    }
    // Give the shutdown acknowledgement and final event time to reach clients.
    tokio::time::sleep(Duration::from_millis(100)).await;
    thread
        .join()
        .map_err(|_| anyhow::anyhow!("Player thread panicked"))?;
    drop(listener);
    let _ = fs::remove_file(&socket);
    drop(lock);
    Ok(())
}

async fn dispatch(sender: &mpsc::SyncSender<Work>, command: Command) -> Reply {
    let (tx, rx) = oneshot::channel();
    if sender.try_send(Work::Request(command, tx)).is_err() {
        return Reply::failure(ApiError::new(
            "server_busy",
            "Server is busy or shutting down; retry later",
        ));
    }
    match tokio::time::timeout(Duration::from_secs(120), rx).await {
        Ok(Ok(reply)) => reply,
        _ => Reply::failure(ApiError::new(
            "timeout",
            "The command outcome is unknown; inspect status before retrying",
        )),
    }
}

async fn connection(
    mut stream: UnixStream,
    sender: mpsc::SyncSender<Work>,
    events: broadcast::Sender<Event>,
) -> Result<()> {
    let request: Request =
        match tokio::time::timeout(Duration::from_secs(5), wire::read(&mut stream)).await {
            Ok(Ok(request)) => request,
            _ => {
                wire::write(
                    &mut stream,
                    &Reply::failure(ApiError::new(
                        "invalid_request",
                        "Expected a bounded JSON request",
                    )),
                )
                .await?;
                return Ok(());
            }
        };
    if request.version != PROTOCOL_VERSION {
        wire::write(
            &mut stream,
            &Reply::failure(ApiError::new(
                "version_mismatch",
                "Client and server protocol versions differ; restart the server with this binary",
            )),
        )
        .await?;
        return Ok(());
    }
    let watch = matches!(request.request, Command::Watch);
    let mut subscription = events.subscribe();
    let reply = dispatch(
        &sender,
        if watch {
            Command::Status
        } else {
            request.request
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &reply)).await??;
    if watch && reply.ok {
        loop {
            let event = match subscription.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let reply = dispatch(&sender, Command::Status).await;
                    Event::State(serde_json::from_value(reply.into_data()?)?)
                }
                Err(_) => break,
            };
            tokio::time::timeout(
                Duration::from_secs(5),
                wire::write(&mut stream, &Reply::success(&event)),
            )
            .await??;
            if matches!(event, Event::Shutdown) {
                break;
            }
        }
    }
    Ok(())
}

fn worker(
    paths: Paths,
    mut store: Store,
    mut engine: Engine<RodioBackend>,
    rx: mpsc::Receiver<Work>,
    tx: mpsc::SyncSender<Work>,
    events: &broadcast::Sender<Event>,
    mut media: crate::media_controls::Controls,
) -> Result<()> {
    let mut last_save = Instant::now();
    let mut last_progress = Instant::now();
    let mut imports = 0usize;
    loop {
        let mut changed = false;
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Work::Request(command, answer)) => match command {
                Command::Shutdown => {
                    store.save(&engine.state)?;
                    engine.stop();
                    let _ = answer.send(Reply::success(json!({"stopped": true})));
                    break;
                }
                Command::Status | Command::Watch | Command::Volume { value: None } => {
                    let _ = answer.send(Reply::success(&engine.state));
                }
                Command::LibraryList {
                    query,
                    offset,
                    limit,
                } => {
                    let reply = match store.search(&query, offset, limit) {
                        Ok((tracks, total)) => Reply::success(
                            json!({"tracks": tracks, "total": total, "offset": offset}),
                        ),
                        Err(e) => failure(e),
                    };
                    let _ = answer.send(reply);
                }
                Command::LibraryRoots => {
                    let reply = match store.roots() {
                        Ok(roots) => Reply::success(roots),
                        Err(e) => failure(e),
                    };
                    let _ = answer.send(reply);
                }
                Command::LibraryAdd { .. }
                | Command::LibraryRemove { .. }
                | Command::LibraryScan => {
                    let result = (|| -> Result<()> {
                        if engine.state.scanning {
                            anyhow::bail!("A library scan is already running");
                        }
                        match &command {
                            Command::LibraryAdd { path } => {
                                let path = path
                                    .canonicalize()
                                    .context("Music directory does not exist")?;
                                if !path.is_dir() {
                                    anyhow::bail!("Library roots must be directories");
                                }
                                store.add_root(&path)?;
                            }
                            Command::LibraryRemove { path } => {
                                store.remove_root(&path.canonicalize().unwrap_or(path.clone()))?;
                            }
                            _ => (),
                        }
                        let roots = store.roots()?;
                        let old = store.records()?;
                        let cache = paths.cache.clone();
                        let tx = tx.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(Work::Catalog(library::scan(&roots, &old, &cache)));
                        });
                        engine.state.scanning = true;
                        Ok(())
                    })();
                    changed = result.is_ok();
                    let reply = match result {
                        Ok(()) => Reply::success(json!({"scanning": true})),
                        Err(e) => failure(e),
                    };
                    let _ = answer.send(reply);
                }
                Command::QueueAdd {
                    paths: ref input_paths,
                    ..
                }
                | Command::Play {
                    paths: ref input_paths,
                    ..
                } if !input_paths.is_empty() => {
                    if imports >= 4 {
                        let _ = answer.send(Reply::failure(ApiError::new(
                            "server_busy",
                            "Too many imports in progress",
                        )));
                        continue;
                    }
                    let inputs = input_paths.clone();
                    let play = matches!(command, Command::Play { .. });
                    let cache = paths.cache.clone();
                    let old = store.records()?;
                    let tx = tx.clone();
                    imports += 1;
                    std::thread::spawn(move || {
                        let scan = library::scan(&inputs, &old, &cache);
                        let _ = tx.send(Work::Imported { scan, play, answer });
                    });
                }
                Command::QueueAdd {
                    track: Some(ref id),
                    ..
                }
                | Command::Play {
                    track: Some(ref id),
                    ..
                } => {
                    let result = (|| -> Result<()> {
                        let track = store.track(id)?.context("Library track not found")?;
                        if matches!(command, Command::Play { .. }) {
                            engine.play_track(track)?;
                        } else {
                            engine.add(vec![track])?;
                        }
                        Ok(())
                    })();
                    finish_command(result, answer, &mut engine, &store, events)?;
                }
                command => {
                    let result = engine.apply(&command);
                    finish_command(result, answer, &mut engine, &store, events)?;
                }
            },
            Ok(Work::Catalog(scan)) => {
                engine.state.scanning = false;
                engine.state.last_error = None;
                match store.replace_catalog(&scan.records) {
                    Ok(()) => {
                        let _ = events.send(Event::LibraryChanged);
                    }
                    Err(e) => engine.state.last_error = Some(format!("Cannot save library: {e:#}")),
                }
                if !scan.warnings.is_empty() {
                    engine.state.last_error = Some(format!(
                        "Scan completed with {} warning(s): {}",
                        scan.warnings.len(),
                        scan.warnings[0]
                    ));
                    for warning in scan.warnings {
                        tracing::warn!("{warning}");
                    }
                }
                changed = true;
            }
            Ok(Work::Imported { scan, play, answer }) => {
                imports = imports.saturating_sub(1);
                let result = (|| -> Result<()> {
                    if scan.records.is_empty() {
                        anyhow::bail!("No supported audio found. {}", scan.warnings.join("; "));
                    }
                    if !scan.warnings.is_empty() {
                        engine.state.last_error = Some(scan.warnings.join("; "));
                    }
                    let id = engine.add(scan.records.into_iter().map(|r| r.track).collect())?;
                    if play {
                        engine.apply(&Command::Play {
                            paths: vec![],
                            track: None,
                            queue_item: id,
                        })?;
                    }
                    Ok(())
                })();
                finish_command(result, answer, &mut engine, &store, events)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
        changed |= engine.tick();
        if changed {
            engine.state.revision += 1;
            store.save(&engine.state)?;
            let _ = events.send(Event::State(engine.state.clone()));
        }
        media.update(&engine.state, engine.output_waiting());
        if last_progress.elapsed() >= Duration::from_secs(1) {
            // Heartbeats also let abandoned watch connections be detected while paused.
            let _ = events.send(Event::Progress {
                position_ms: engine.state.position_ms,
                revision: engine.state.revision,
            });
            last_progress = Instant::now();
        }
        if last_save.elapsed() >= Duration::from_secs(5) {
            store.save(&engine.state)?;
            last_save = Instant::now();
        }
    }
    Ok(())
}

fn finish_command(
    result: Result<()>,
    answer: Answer,
    engine: &mut Engine<RodioBackend>,
    store: &Store,
    events: &broadcast::Sender<Event>,
) -> Result<()> {
    engine.state.revision += 1;
    if let Err(error) = &result {
        engine.state.last_error = Some(format!("{error:#}"));
    }
    store.save(&engine.state)?;
    let _ = events.send(Event::State(engine.state.clone()));
    let reply = match result {
        Ok(()) => Reply::success(&engine.state),
        Err(error) => failure(error),
    };
    let _ = answer.send(reply);
    Ok(())
}
fn failure(error: anyhow::Error) -> Reply {
    Reply::failure(ApiError::new("operation_failed", format!("{error:#}")))
}
