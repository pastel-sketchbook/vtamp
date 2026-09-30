use crate::{
    client::Client,
    model::*,
    platform::{self, Paths},
    wire,
};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Music stays. Your terminal moves on.",
    long_about = "A detachable terminal music player. Run vtamp to attach; q closes the interface and keeps music playing."
)]
pub struct Args {
    /// Emit structured JSON (watch emits newline-delimited JSON).
    #[arg(long, global = true)]
    pub json: bool,
    /// Cover rendering; auto uses halfblocks inside tmux.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub art: Art,
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Art {
    #[default]
    Auto,
    Halfblocks,
    Kitty,
    None,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Attach the TUI, starting the server if needed.
    Attach,
    /// Resume playback, or append inputs and play the first new entry.
    Play {
        #[arg(conflicts_with_all = ["track", "queue_item"])]
        paths: Vec<PathBuf>,
        #[arg(long, conflicts_with = "queue_item")]
        track: Option<String>,
        #[arg(long)]
        queue_item: Option<String>,
    },
    /// Pause playback (idempotent).
    Pause,
    /// Resume playback (idempotent).
    Resume,
    /// Toggle between playing and paused.
    Toggle,
    /// Stop playback and reset position, keeping the queue.
    Stop,
    Next,
    Prev,
    /// Seek to seconds; +10 and -10 move relative to the current position.
    Seek {
        #[arg(allow_hyphen_values = true, value_parser = parse_seek)]
        seconds: Seek,
    },
    /// Read or set volume, from 0 to 100.
    Volume {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        value: Option<u8>,
    },
    Shuffle {
        #[arg(value_enum)]
        mode: Switch,
    },
    Repeat {
        #[arg(value_enum)]
        mode: Repeat,
    },
    /// Inspect or edit the shared queue.
    Queue {
        #[command(subcommand)]
        command: Queue,
    },
    /// Register music folders and search their catalog.
    Library {
        #[command(subcommand)]
        command: Library,
    },
    /// Read playback state without starting a server.
    Status,
    /// Subscribe to playback events without starting a server.
    Watch,
    /// Manage the background playback server.
    Server {
        #[command(subcommand)]
        command: Server,
    },
    /// Inspect runtime paths, server health, and the default audio device.
    Doctor,
}

#[derive(Debug, Clone)]
pub struct Seek {
    pub milliseconds: i64,
    pub relative: bool,
}
fn parse_seek(input: &str) -> Result<Seek, String> {
    let seconds: f64 = input
        .parse()
        .map_err(|_| "Expected seconds, +seconds, or -seconds")?;
    if !seconds.is_finite() || seconds.abs() > 315_360_000.0 {
        return Err("Seek value must be finite and within ten years".into());
    }
    Ok(Seek {
        milliseconds: (seconds * 1000.0).round() as i64,
        relative: input.starts_with(['+', '-']),
    })
}

#[derive(Debug, Subcommand)]
pub enum Queue {
    List,
    Add {
        #[arg(required_unless_present = "track", conflicts_with = "track")]
        paths: Vec<PathBuf>,
        #[arg(long)]
        track: Option<String>,
    },
    Remove {
        id: String,
    },
    /// Move an entry to a zero-based position.
    Move {
        id: String,
        index: usize,
    },
    Clear,
}
#[derive(Debug, Subcommand)]
pub enum Library {
    Add {
        path: PathBuf,
    },
    /// Unregister a folder without deleting any music files.
    Remove {
        path: PathBuf,
    },
    Scan,
    List {
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    Search {
        query: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    Roots,
}
#[derive(Debug, Subcommand)]
pub enum Server {
    Start,
    Status,
    Stop,
    #[command(hide = true)]
    Run,
}

pub async fn run(args: Args) -> Result<()> {
    let paths = Paths::discover()?;
    let client = Client::new(paths.clone());
    let action = args.command.unwrap_or(Action::Attach);
    match action {
        Action::Attach => {
            if args.json {
                bail!("Use vtamp status --json or vtamp watch --json for machine-readable output");
            }
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                bail!("The TUI needs an interactive terminal. Try vtamp status --json");
            }
            client.ensure().await?;
            crate::tui::run(client, args.art).await?;
            return Ok(());
        }
        Action::Server {
            command: Server::Run,
        } => {
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "vtamp=info,rodio=warn".into()),
                )
                .init();
            return crate::daemon::run(paths).await;
        }
        Action::Server {
            command: Server::Start,
        } => {
            client.ensure().await?;
            return output(
                Reply::success(json!({"running": true, "socket": paths.socket()})),
                args.json,
            );
        }
        Action::Doctor => {
            use rodio::cpal::traits::{DeviceTrait, HostTrait};
            let device = rodio::cpal::default_host()
                .default_output_device()
                .and_then(|d| d.description().ok())
                .map(|d| d.to_string());
            let status = client.request(Command::Status).await;
            return output(
                Reply::success(
                    json!({"data_directory": paths.data, "socket": paths.socket(), "log": paths.log(),
                "server_reachable": status.as_ref().is_ok_and(|r| r.ok), "server_error": status.err().map(|e| e.to_string()),
                "default_output_device": device, "term": std::env::var("TERM").ok(), "tmux": std::env::var_os("TMUX").is_some(),
                "protocol_version": PROTOCOL_VERSION, "version": env!("CARGO_PKG_VERSION")}),
                ),
                args.json,
            );
        }
        Action::Watch => {
            let (state, mut stream) = client.watch().await?;
            output(Reply::success(Event::State(state)), args.json)?;
            loop {
                let reply: Reply = tokio::select! {
                    reply = wire::read(&mut stream) => reply?,
                    _ = tokio::signal::ctrl_c() => break,
                };
                let stopped = reply
                    .data
                    .as_ref()
                    .and_then(|d| d.get("event"))
                    .and_then(Value::as_str)
                    == Some("shutdown");
                output(reply, args.json)?;
                if stopped {
                    break;
                }
            }
            return Ok(());
        }
        _ => (),
    }
    let read_only = matches!(
        action,
        Action::Status
            | Action::Volume { value: None }
            | Action::Queue {
                command: Queue::List
            }
            | Action::Library {
                command: Library::List { .. } | Library::Search { .. } | Library::Roots
            }
            | Action::Server {
                command: Server::Status | Server::Stop
            }
    );
    let queue_only = matches!(
        action,
        Action::Queue {
            command: Queue::List
        }
    );
    let command = match action {
        Action::Status
        | Action::Server {
            command: Server::Status,
        }
        | Action::Queue {
            command: Queue::List,
        } => Command::Status,
        Action::Server {
            command: Server::Stop,
        } => Command::Shutdown,
        Action::Play {
            paths,
            track,
            queue_item,
        } => Command::Play {
            paths: absolute_paths(paths)?,
            track,
            queue_item,
        },
        Action::Pause => Command::Pause,
        Action::Resume => Command::Resume,
        Action::Toggle => Command::Toggle,
        Action::Stop => Command::Stop,
        Action::Next => Command::Next,
        Action::Prev => Command::Prev,
        Action::Seek { seconds } => Command::Seek {
            milliseconds: seconds.milliseconds,
            relative: seconds.relative,
        },
        Action::Volume { value } => Command::Volume { value },
        Action::Shuffle { mode } => Command::Shuffle {
            enabled: matches!(mode, Switch::On),
        },
        Action::Repeat { mode } => Command::Repeat { mode },
        Action::Queue { command } => match command {
            Queue::Add { paths, track } => Command::QueueAdd {
                paths: absolute_paths(paths)?,
                track,
            },
            Queue::Remove { id } => Command::QueueRemove { id },
            Queue::Move { id, index } => Command::QueueMove { id, index },
            Queue::Clear => Command::QueueClear,
            Queue::List => unreachable!(),
        },
        Action::Library { command } => match command {
            Library::Add { path } => Command::LibraryAdd {
                path: platform::absolute(&path)?,
            },
            Library::Remove { path } => Command::LibraryRemove {
                path: platform::absolute(&path)?,
            },
            Library::Scan => Command::LibraryScan,
            Library::Roots => Command::LibraryRoots,
            Library::List { offset, limit } => Command::LibraryList {
                query: String::new(),
                offset,
                limit: limit.into(),
            },
            Library::Search {
                query,
                offset,
                limit,
            } => Command::LibraryList {
                query,
                offset,
                limit: limit.into(),
            },
        },
        _ => unreachable!(),
    };
    if !read_only {
        client.ensure().await?;
    }
    let stopping = matches!(command, Command::Shutdown);
    let mut reply = client.request(command).await?;
    if stopping && reply.ok {
        // A completed stop must be safe to follow immediately with a new start.
        for _ in 0..100 {
            if !client.paths.socket().exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        if client.paths.socket().exists() {
            bail!("Server acknowledged stop but has not released its socket yet");
        }
    }
    if queue_only && reply.ok {
        reply.data = reply.data.and_then(|s| s.get("queue").cloned());
    }
    output(reply, args.json)
}

fn absolute_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    paths.iter().map(|p| platform::absolute(p)).collect()
}

fn output(reply: Reply, json: bool) -> Result<()> {
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| ApiError::new("protocol_error", "Missing error details"))
            .into());
    }
    let mut out = io::stdout().lock();
    if json {
        writeln!(out, "{}", serde_json::to_string(&reply)?)?;
        return Ok(());
    }
    let data = reply.data.unwrap_or(Value::Null);
    if let Ok(state) = serde_json::from_value::<State>(data.clone())
        && data.get("status").is_some()
    {
        if let Some(item) = state.current() {
            writeln!(
                out,
                "{:?}  {} — {}",
                state.status, item.track.artist, item.track.title
            )?;
            writeln!(
                out,
                "{} / {} · volume {}% · {} queued",
                display_time(state.position_ms),
                display_time(item.track.duration_ms),
                state.volume,
                state.queue.len()
            )?;
        } else {
            writeln!(
                out,
                "Stopped · {} queued · volume {}%",
                state.queue.len(),
                state.volume
            )?;
        }
        if let Some(error) = &state.last_error {
            writeln!(out, "Last warning: {error}")?;
        }
    } else if let Some(tracks) = data.get("tracks").and_then(Value::as_array) {
        for track in tracks {
            let track: Track = serde_json::from_value(track.clone())?;
            writeln!(
                out,
                "{}  {} — {} [{}]",
                track.id, track.artist, track.title, track.album
            )?;
        }
        writeln!(
            out,
            "{} of {} tracks (offset {})",
            tracks.len(),
            data["total"],
            data["offset"]
        )?;
    } else if let Some(items) = data.as_array() {
        for (i, value) in items.iter().enumerate() {
            if let Ok(item) = serde_json::from_value::<QueueItem>(value.clone()) {
                writeln!(
                    out,
                    "{i:>4}  {}  {} — {}",
                    item.id, item.track.artist, item.track.title
                )?;
            } else {
                writeln!(out, "{}", value.as_str().unwrap_or(""))?;
            }
        }
        if items.is_empty() {
            writeln!(out, "Empty")?;
        }
    } else {
        writeln!(out, "{}", serde_json::to_string_pretty(&data)?)?;
    }
    Ok(())
}

pub fn report(error: anyhow::Error, json: bool) -> i32 {
    if error
        .downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
    {
        return 0;
    }
    let api = error
        .downcast_ref::<ApiError>()
        .cloned()
        .unwrap_or_else(|| ApiError::new("client_error", format!("{error:#}")));
    if json {
        let _ = writeln!(
            io::stdout(),
            "{}",
            serde_json::to_string(&Reply::failure(api)).unwrap()
        );
    } else {
        eprintln!("vtamp: {api}");
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_seek_and_exclusive_play_inputs() {
        assert_eq!(parse_seek("-1.5").unwrap().milliseconds, -1500);
        assert!(parse_seek("+10").unwrap().relative);
        assert!(!parse_seek("10").unwrap().relative);
        assert!(parse_seek("NaN").is_err());
        assert!(Args::try_parse_from(["vtamp", "play", "a.m4a", "--track", "abc"]).is_err());
        assert!(Args::try_parse_from(["vtamp", "queue", "add"]).is_err());
        assert!(Args::try_parse_from(["vtamp", "seek", "-10", "--json"]).is_ok());
    }
}
