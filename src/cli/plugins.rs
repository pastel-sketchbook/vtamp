use super::*;
use crate::plugin::{Catalog, Context, Registry, Session, Target};
use std::sync::Arc;
use tokio::sync::{Notify, watch};

#[derive(Debug, Subcommand)]
pub enum PluginAction {
    /// Register a local manifest without executing the plugin.
    Add { path: PathBuf },
    /// Inspect registered plugins and configuration diagnostics.
    List,
    /// Unregister a plugin; leave its files and data intact.
    Remove { id: String },
    /// Run a command without a TUI; --json streams result snapshots as NDJSON.
    Run {
        command: String,
        #[arg(long)]
        track: Option<String>,
    },
}

pub async fn run(paths: Paths, action: PluginAction, json: bool) -> Result<()> {
    match action {
        PluginAction::Add { path } => {
            let id = Registry::add(&paths, &platform::absolute(&path)?)?;
            output(
                Reply::success(json!({"registered":id,"applies_to":"future_attachments"})),
                json,
            )
        }
        PluginAction::Remove { id } => {
            Registry::remove(&paths, &id)?;
            output(
                Reply::success(json!({"removed":id,"applies_to":"future_attachments"})),
                json,
            )
        }
        PluginAction::List => {
            let catalog = Catalog::load(&paths);
            let plugins: Vec<_> = catalog
                .plugins
                .iter()
                .map(|p| json!({"manifest":p.manifest,"path":p.path}))
                .collect();
            output(
                Reply::success(
                    json!({"plugins":plugins,"warnings":catalog.warnings,"bindings":catalog.bindings}),
                ),
                json,
            )
        }
        PluginAction::Run { command, track } => run_command(paths, &command, track, json).await,
    }
}

async fn run_command(paths: Paths, name: &str, track: Option<String>, json: bool) -> Result<()> {
    let (plugin, command) = Catalog::load(&paths).resolve(name)?;
    if (command.target == Target::Selected) != track.is_some() {
        bail!("--track is required only for selected-track commands");
    }
    let client = Client::new(paths.clone());
    let selected: Option<Track> = if let Some(id) = track {
        Some(serde_json::from_value(
            client
                .request(Command::LibraryTrack { id })
                .await?
                .into_data()?,
        )?)
    } else {
        None
    };
    let target = command.target;
    let mut context = Context::from_state(&State::default(), false, target, selected.as_ref());
    let (snapshots, mut state) = watch::channel((false, State::default()));
    let watching = (target != Target::None).then(|| {
        tokio::spawn(async move {
            loop {
                if let Ok((initial, mut stream)) = client.watch().await {
                    let mut current = initial;
                    snapshots.send_replace((true, current.clone()));
                    while let Ok(Ok(reply)) = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        wire::read::<_, Reply>(&mut stream),
                    )
                    .await
                    {
                        let Ok(event) = reply.into_data().and_then(|data| {
                            serde_json::from_value(data)
                                .map_err(|e| ApiError::new("invalid_event", e.to_string()))
                        }) else {
                            break;
                        };
                        match event {
                            Event::State(next) if next.revision >= current.revision => {
                                current = next
                            }
                            Event::Progress {
                                revision,
                                position_ms,
                            } if revision == current.revision => current.position_ms = position_ms,
                            Event::Shutdown => break,
                            _ => continue,
                        }
                        snapshots.send_replace((true, current.clone()));
                    }
                    snapshots.send_replace((false, current));
                }
                if snapshots.is_closed() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        })
    });
    let mut session = Session::start(
        plugin,
        command,
        context.clone(),
        paths,
        Arc::new(Notify::new()),
    );
    let result = async {
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(()),
                changed = state.changed(), if watching.is_some() => {
                    if changed.is_err() { bail!("Playback observer stopped"); }
                    let (connected, state) = state.borrow_and_update().clone();
                    if context.advance(Context::from_state(&state, connected, target, selected.as_ref())) {
                        session.context(context.clone());
                    }
                }
                changed = session.updates.changed() => {
                    if changed.is_err() { bail!("Plugin session stopped"); }
                    let update = session.updates.borrow_and_update().clone();
                    if update.generation == context.generation || update.error {
                        if json { output(Reply::success(update.as_ref()), true)?; }
                        else {
                            if let Some(view) = &update.view {
                                println!("{}", view.title);
                                for item in &view.items { println!("{}", item.text); }
                            }
                            if !update.notice.is_empty() { println!("{}", update.notice); }
                        }
                    }
                    if update.finished {
                        if update.error { bail!("{}", update.notice); }
                        return Ok(());
                    }
                }
            }
        }
    }.await;
    if let Some(watching) = watching {
        watching.abort();
    }
    session.close().await;
    result
}
