use crate::{cli::Art, client::Client, library::decode_image, model::*, platform, wire};
use anyhow::Result;
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_image::{
    Resize, StatefulImage,
    picker::{Picker, ProtocolType},
    thread::{ResizeRequest, ResizeResponse, ThreadProtocol},
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::mpsc as sync_mpsc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const GREEN: Color = Color::Rgb(180, 246, 118);
const TEXT: Color = Color::Rgb(238, 234, 224);
const MUTED: Color = Color::Rgb(164, 177, 155);
const BG: Color = Color::Rgb(24, 28, 25);
const PANEL: Color = Color::Rgb(32, 37, 33);
const BORDER: Color = Color::Rgb(77, 88, 72);
const AMBER: Color = Color::Rgb(239, 188, 114);
const PAGE_SIZE: usize = 200;

enum Message {
    Connected(State),
    Disconnected(String),
    Event(Event),
    Reply(Command, Result<Value, String>),
    Cover(Option<PathBuf>, image::DynamicImage),
    Resized(ResizeResponse),
}
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Library,
    Queue,
}
enum Input {
    Search(String),
    Folder(String),
}

struct App {
    state: State,
    tracks: Vec<Track>,
    total: usize,
    offset: usize,
    query: String,
    library_selection: ListState,
    queue_selection: ListState,
    focus: Focus,
    input: Option<Input>,
    help: bool,
    connected: bool,
    notice: String,
    notice_at: Instant,
    last_progress: Instant,
    picker: Picker,
    cover: ThreadProtocol,
    cover_key: Option<PathBuf>,
    show_art: bool,
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub async fn run(client: Client, art: Art) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    let _guard = TerminalGuard;
    let mut picker = match art {
        Art::None | Art::Halfblocks => Picker::halfblocks(),
        Art::Auto
            if std::env::var_os("TMUX").is_some()
                || std::env::var("TERM")
                    .is_ok_and(|s| s.starts_with("tmux") || s.starts_with("screen")) =>
        {
            Picker::halfblocks()
        }
        _ => Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks()),
    };
    if matches!(art, Art::Kitty) {
        picker.set_protocol_type(ProtocolType::Kitty);
    }
    picker.set_background_color(Some(image::Rgba([32, 37, 33, 255])));
    let (messages, mut incoming) = mpsc::unbounded_channel();
    let (commands, mut requests) = mpsc::channel::<Command>(64);
    let (resize_tx, resize_rx) = sync_mpsc::channel::<ResizeRequest>();
    let resize_messages = messages.clone();
    std::thread::spawn(move || {
        while let Ok(request) = resize_rx.recv() {
            if let Ok(response) = request.resize_encode()
                && resize_messages.send(Message::Resized(response)).is_err()
            {
                break;
            }
        }
    });
    let cover = ThreadProtocol::new(resize_tx, Some(picker.new_resize_protocol(placeholder())));
    let mut app = App {
        state: State::default(),
        tracks: vec![],
        total: 0,
        offset: 0,
        query: String::new(),
        library_selection: ListState::default().with_selected(Some(0)),
        queue_selection: ListState::default().with_selected(Some(0)),
        focus: Focus::Library,
        input: None,
        help: false,
        connected: false,
        notice: "Connecting…".into(),
        notice_at: Instant::now(),
        last_progress: Instant::now(),
        picker,
        cover,
        cover_key: None,
        show_art: !matches!(art, Art::None),
    };
    let watch_client = client.clone();
    let watch_messages = messages.clone();
    let watch_task = tokio::spawn(async move {
        loop {
            let failure = match watch_client.watch().await {
                Ok((state, mut stream)) => {
                    if watch_messages.send(Message::Connected(state)).is_err() {
                        break;
                    }
                    loop {
                        let reply = tokio::time::timeout(
                            Duration::from_secs(5),
                            wire::read::<_, Reply>(&mut stream),
                        )
                        .await;
                        match reply {
                            Ok(Ok(reply)) => match reply
                                .into_data()
                                .ok()
                                .and_then(|v| serde_json::from_value::<Event>(v).ok())
                            {
                                Some(Event::Shutdown) => {
                                    break "Server stopped. Run vtamp server start to reconnect."
                                        .to_string();
                                }
                                Some(event) => {
                                    if watch_messages.send(Message::Event(event)).is_err() {
                                        return;
                                    }
                                }
                                None => break "Invalid server event".into(),
                            },
                            _ => break "Disconnected. Waiting for the server…".into(),
                        }
                    }
                }
                Err(_) => "Disconnected. Waiting for the server…".into(),
            };
            if watch_messages.send(Message::Disconnected(failure)).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let reply_messages = messages.clone();
    let command_task = tokio::spawn(async move {
        while let Some(command) = requests.recv().await {
            let result = match client.request(command.clone()).await {
                Ok(reply) => reply.into_data().map_err(|e| e.to_string()),
                Err(error) => Err(format!("{error:#}")),
            };
            if reply_messages
                .send(Message::Reply(command, result))
                .is_err()
            {
                break;
            }
        }
    });
    let result = async {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        loop {
            tokio::select! {
                _ = tick.tick() => (),
                _ = terminate.recv() => return Ok::<_, anyhow::Error>(()),
                _ = interrupt.recv() => return Ok::<_, anyhow::Error>(()),
            }
            while let Ok(message) = incoming.try_recv() {
                app.message(message, &messages, &commands);
            }
            while event::poll(Duration::ZERO)? {
                match event::read()? {
                    TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        if app.key(key, &commands)? {
                            return Ok::<_, anyhow::Error>(());
                        }
                    }
                    TerminalEvent::Resize(_, _) => terminal.clear()?,
                    _ => (),
                }
            }
            terminal.draw(|frame| app.draw(frame))?;
        }
    }
    .await;
    watch_task.abort();
    command_task.abort();
    result
}

impl App {
    fn notice(&mut self, text: impl Into<String>) {
        self.notice = text.into();
        self.notice_at = Instant::now();
    }
    fn send(&mut self, commands: &mpsc::Sender<Command>, command: Command) {
        if !self.connected {
            self.notice("Disconnected. Commands are not queued for replay.");
            return;
        }
        if commands.try_send(command).is_err() {
            self.notice("Too many pending commands; try again shortly.");
        }
    }
    fn refresh(&mut self, commands: &mpsc::Sender<Command>) {
        self.send(
            commands,
            Command::LibraryList {
                query: self.query.clone(),
                offset: self.offset,
                limit: PAGE_SIZE,
            },
        );
    }
    fn state(&mut self, state: State, messages: &mpsc::UnboundedSender<Message>) {
        let cover_key = state.current().and_then(|q| q.track.cover.clone());
        if self.cover_key != cover_key {
            self.cover_key = cover_key.clone();
            self.cover
                .replace_protocol(self.picker.new_resize_protocol(placeholder()));
            if self.show_art {
                let sender = messages.clone();
                tokio::task::spawn_blocking(move || {
                    let image = cover_key
                        .as_ref()
                        .and_then(|p| {
                            if p.metadata().ok()?.len() > 16 * 1024 * 1024 {
                                return None;
                            }
                            decode_image(&std::fs::read(p).ok()?).ok()
                        })
                        .unwrap_or_else(placeholder);
                    let _ = sender.send(Message::Cover(cover_key, image));
                });
            }
        }
        self.state = state;
        self.last_progress = Instant::now();
        clamp_selection(&mut self.queue_selection, self.state.queue.len());
    }
    fn message(
        &mut self,
        message: Message,
        messages: &mpsc::UnboundedSender<Message>,
        commands: &mpsc::Sender<Command>,
    ) {
        match message {
            Message::Connected(state) => {
                self.connected = true;
                self.state(state, messages);
                self.notice("Attached. q detaches; music keeps playing.");
                self.refresh(commands);
            }
            Message::Disconnected(reason) => {
                self.connected = false;
                self.notice(reason);
            }
            Message::Event(Event::State(state)) if state.revision >= self.state.revision => {
                self.state(state, messages)
            }
            Message::Event(Event::Progress {
                position_ms,
                revision,
            }) if revision == self.state.revision => {
                self.state.position_ms = position_ms;
                self.last_progress = Instant::now();
            }
            Message::Event(Event::LibraryChanged) => self.refresh(commands),
            Message::Reply(command, result) => match result {
                Err(error) => self.notice(error),
                Ok(value) => {
                    if let Command::LibraryList { query, offset, .. } = command {
                        if query == self.query && offset == self.offset {
                            self.tracks =
                                serde_json::from_value(value["tracks"].clone()).unwrap_or_default();
                            self.total = value["total"].as_u64().unwrap_or(0) as usize;
                            clamp_selection(&mut self.library_selection, self.tracks.len());
                        }
                    } else if value.get("status").is_some() {
                        if let Ok(state) = serde_json::from_value::<State>(value)
                            && state.revision >= self.state.revision
                        {
                            self.state(state, messages);
                        }
                    } else if value.get("scanning").is_some() {
                        self.notice("Scanning music folders in the background…");
                    }
                }
            },
            Message::Cover(key, image) if key == self.cover_key => self
                .cover
                .replace_protocol(self.picker.new_resize_protocol(image)),
            Message::Resized(response) => {
                self.cover.update_resized_protocol(response);
            }
            _ => (),
        }
    }
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.help {
            self.help = false;
            return Ok(false);
        }
        if let Some(input) = &mut self.input {
            let text = match input {
                Input::Search(text) | Input::Folder(text) => text,
            };
            match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
                KeyCode::Enter => match self.input.take().unwrap() {
                    Input::Search(query) => {
                        self.query = query;
                        self.offset = 0;
                        self.library_selection.select(Some(0));
                        self.refresh(commands);
                    }
                    Input::Folder(path) if !path.trim().is_empty() => {
                        match platform::absolute(&PathBuf::from(path)) {
                            Ok(path) => self.send(commands, Command::LibraryAdd { path }),
                            Err(error) => self.notice(error.to_string()),
                        }
                    }
                    _ => (),
                },
                _ => (),
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Library {
                    Focus::Queue
                } else {
                    Focus::Library
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Char('/') => {
                self.focus = Focus::Library;
                self.input = Some(Input::Search(self.query.clone()));
            }
            KeyCode::Char('a') => self.input = Some(Input::Folder(String::new())),
            KeyCode::Char('r') => self.send(commands, Command::LibraryScan),
            KeyCode::Char(' ') => self.send(commands, Command::Toggle),
            KeyCode::Char('n') => self.send(commands, Command::Next),
            KeyCode::Char('b') => self.send(commands, Command::Prev),
            KeyCode::Char('s') => self.send(
                commands,
                Command::Shuffle {
                    enabled: !self.state.shuffle,
                },
            ),
            KeyCode::Char('R') => self.send(
                commands,
                Command::Repeat {
                    mode: match self.state.repeat {
                        Repeat::Off => Repeat::All,
                        Repeat::All => Repeat::One,
                        Repeat::One => Repeat::Off,
                    },
                },
            ),
            KeyCode::Left => self.send(
                commands,
                Command::Seek {
                    milliseconds: -10_000,
                    relative: true,
                },
            ),
            KeyCode::Right => self.send(
                commands,
                Command::Seek {
                    milliseconds: 10_000,
                    relative: true,
                },
            ),
            KeyCode::Char('+') | KeyCode::Char('=') => self.send(
                commands,
                Command::Volume {
                    value: Some(self.state.volume.saturating_add(5).min(100)),
                },
            ),
            KeyCode::Char('-') => self.send(
                commands,
                Command::Volume {
                    value: Some(self.state.volume.saturating_sub(5)),
                },
            ),
            KeyCode::Char(']')
                if self.focus == Focus::Library && self.offset + PAGE_SIZE < self.total =>
            {
                self.offset += PAGE_SIZE;
                self.library_selection.select(Some(0));
                self.refresh(commands);
            }
            KeyCode::Char('[') if self.focus == Focus::Library => {
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.library_selection.select(Some(0));
                self.refresh(commands);
            }
            KeyCode::Enter | KeyCode::Char('e') if self.focus == Focus::Library => {
                if let Some(track) = self
                    .library_selection
                    .selected()
                    .and_then(|i| self.tracks.get(i))
                {
                    let id = track.id.clone();
                    let command = if key.code == KeyCode::Enter {
                        Command::Play {
                            paths: vec![],
                            track: Some(id),
                            queue_item: None,
                        }
                    } else {
                        Command::QueueAdd {
                            paths: vec![],
                            track: Some(id),
                        }
                    };
                    self.send(commands, command);
                }
            }
            KeyCode::Enter if self.focus == Focus::Queue => {
                if let Some(item) = self
                    .queue_selection
                    .selected()
                    .and_then(|i| self.state.queue.get(i))
                {
                    self.send(
                        commands,
                        Command::Play {
                            paths: vec![],
                            track: None,
                            queue_item: Some(item.id.clone()),
                        },
                    );
                }
            }
            KeyCode::Char('d') if self.focus == Focus::Queue => {
                if let Some(item) = self
                    .queue_selection
                    .selected()
                    .and_then(|i| self.state.queue.get(i))
                {
                    self.send(
                        commands,
                        Command::QueueRemove {
                            id: item.id.clone(),
                        },
                    );
                }
            }
            KeyCode::Char('J' | 'K') if self.focus == Focus::Queue => {
                if let Some(i) = self.queue_selection.selected()
                    && let Some(item) = self.state.queue.get(i)
                {
                    let index = if key.code == KeyCode::Char('J') {
                        (i + 1).min(self.state.queue.len() - 1)
                    } else {
                        i.saturating_sub(1)
                    };
                    self.send(
                        commands,
                        Command::QueueMove {
                            id: item.id.clone(),
                            index,
                        },
                    );
                    self.queue_selection.select(Some(index));
                }
            }
            _ => (),
        }
        Ok(false)
    }
    fn move_selection(&mut self, delta: isize) {
        let (state, len) = if self.focus == Focus::Library {
            (&mut self.library_selection, self.tracks.len())
        } else {
            (&mut self.queue_selection, self.state.queue.len())
        };
        if len > 0 {
            state.select(Some(
                state
                    .selected()
                    .unwrap_or(0)
                    .saturating_add_signed(delta)
                    .min(len - 1),
            ));
        }
    }
    fn position(&self) -> u64 {
        let elapsed = if self.state.status == PlaybackStatus::Playing && self.connected {
            self.last_progress.elapsed().as_millis() as u64
        } else {
            0
        };
        let duration = self.state.current().map_or(0, |q| q.track.duration_ms);
        (self.state.position_ms + elapsed).min(duration)
    }
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 40 || area.height < 12 {
            frame.render_widget(
                Paragraph::new(
                    "vtamp\nPane is too small (40 × 12 minimum).\nPlayback continues. q detaches.",
                )
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let now_height = if area.height >= 28 { 11 } else { 6 };
        let [header, now, content, status, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(now_height),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " vtamp ",
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" / virtual terminal amplifier", Style::default().fg(MUTED)),
            ])),
            header,
        );
        self.now_playing(frame, now);
        if area.width >= 100 {
            let [library, queue] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(content);
            self.library(frame, library);
            self.queue(frame, queue);
        } else if self.focus == Focus::Library {
            self.library(frame, content);
        } else {
            self.queue(frame, content);
        }
        let message = if !self.connected {
            self.notice.clone()
        } else if self.state.scanning {
            "Scanning folders… playback stays available.".into()
        } else if self.notice_at.elapsed() < Duration::from_secs(6) {
            self.notice.clone()
        } else {
            self.state
                .last_error
                .clone()
                .unwrap_or_else(|| "Music stays. Your terminal moves on.".into())
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(if self.connected {
                MUTED
            } else {
                AMBER
            })),
            status,
        );
        let keys = if area.width >= 85 {
            " Space play/pause  n/b skip  ←/→ seek  +/- vol  Tab switch  / search  a folder  ? help  q detach"
        } else {
            " Space play  Tab switch  / search  ? help  q detach"
        };
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().bg(PANEL).fg(GREEN)),
            hints,
        );
        if let Some(input) = &self.input {
            let (label, text) = match input {
                Input::Search(s) => (
                    " Search title / artist / album · Enter applies · Esc cancels ",
                    s,
                ),
                Input::Folder(s) => (" Add music folder · Enter scans · Esc cancels ", s),
            };
            let popup = Rect::new(
                area.x + 2,
                area.y + area.height.saturating_sub(6),
                area.width.saturating_sub(4),
                3,
            );
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(format!("{text}█"))
                    .block(block(label, true))
                    .style(Style::default().fg(TEXT).bg(PANEL)),
                popup,
            );
        }
        if self.help {
            let popup = centered(area, 78, 22);
            frame.render_widget(Clear, popup);
            let text = "ATTACH / DETACH\nq / Esc / Ctrl+C   Close this interface. Music keeps playing.\n\nPLAYBACK\nSpace   Play / pause       n / b   Next / previous\n← / →   Seek 10 seconds    + / -   Volume\ns       Shuffle           R       Cycle repeat\n\nLIBRARY & QUEUE\nTab     Switch panels     j / k   Move selection\n/       Search            a       Add music folder\nr       Rescan folders    [ / ]   Library pages\nEnter   Play selection    e       Enqueue library selection\nd       Remove queue item J / K   Move queue item down / up\n\nStop the server explicitly with: vtamp server stop\nAny key closes help.";
            frame.render_widget(
                Paragraph::new(text)
                    .block(block(" vtamp / key reference ", true))
                    .style(Style::default().fg(TEXT).bg(PANEL))
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
    }
    fn now_playing(&mut self, frame: &mut Frame, area: Rect) {
        let panel = block(" NOW PLAYING ", false);
        let inner = panel.inner(area);
        frame.render_widget(panel, area);
        let show_cover = self.show_art && inner.height >= 7 && inner.width >= 64;
        let info = if show_cover {
            let [cover, gap, info] = Layout::horizontal([
                Constraint::Length(inner.height * 2),
                Constraint::Length(2),
                Constraint::Min(0),
            ])
            .areas(inner);
            let _ = gap;
            frame.render_stateful_widget(
                StatefulImage::new().resize(Resize::Fit(None)),
                cover,
                &mut self.cover,
            );
            info
        } else {
            inner
        };
        let item = self.state.current();
        let title = item.map_or("Your music, your terminal.", |q| q.track.title.as_str());
        let artist = item.map_or("Press a to add a music folder, then Enter to play.", |q| {
            q.track.artist.as_str()
        });
        let album = item.map_or("Local files. No account. No permanent pane.", |q| {
            q.track.album.as_str()
        });
        let label = if !self.connected {
            "DISCONNECTED"
        } else {
            match self.state.status {
                PlaybackStatus::Playing => "PLAYING",
                PlaybackStatus::Paused => "PAUSED",
                PlaybackStatus::Stopped => "STOPPED",
            }
        };
        let duration = item.map_or(0, |q| q.track.duration_ms);
        let pos = self.position();
        let [names, progress, controls] = Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(info);
        let mut lines = vec![
            Line::styled(
                title,
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Line::styled(artist, Style::default().fg(TEXT)),
        ];
        if names.height > 2 {
            lines.push(Line::styled(album, Style::default().fg(MUTED)));
        }
        if names.height > 4 {
            lines.push(Line::from(""));
            lines.push(Line::styled(label, Style::default().fg(GREEN)));
        }
        frame.render_widget(Paragraph::new(lines), names);
        let ratio = if duration == 0 {
            0.0
        } else {
            (pos as f64 / duration as f64).clamp(0.0, 1.0)
        };
        frame.render_widget(
            Gauge::default()
                .ratio(ratio)
                .gauge_style(Style::default().fg(GREEN).bg(PANEL))
                .label(format!(
                    "{} / {}",
                    display_time(pos),
                    display_time(duration)
                )),
            progress,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{}  VOL {:3}%  SHUF {}  REPEAT {:?}",
                label,
                self.state.volume,
                if self.state.shuffle { "ON" } else { "OFF" },
                self.state.repeat
            ))
            .style(Style::default().fg(MUTED)),
            controls,
        );
    }
    fn library(&mut self, frame: &mut Frame, area: Rect) {
        let title = format!(
            " LIBRARY · {} tracks{} · Tab / queue ",
            self.total,
            if self.query.is_empty() {
                String::new()
            } else {
                format!(" · {}", self.query)
            }
        );
        let panel = block(&title, self.focus == Focus::Library);
        if self.tracks.is_empty() {
            frame.render_widget(Paragraph::new(if self.query.is_empty() { "\n  Start with a folder of music.\n\n  Press a to add ~/Music\n  Or: vtamp library add ~/Music\n\n  Already added a folder? Press r to rescan." } else { "\n  No matching tracks.\n  Press / to change or clear the search." }).block(panel).style(Style::default().fg(MUTED)).wrap(Wrap { trim: false }), area);
        } else {
            let items: Vec<_> = self
                .tracks
                .iter()
                .map(|t| {
                    ListItem::new(vec![
                        Line::from(t.title.clone()),
                        Line::styled(
                            format!("{} · {}", t.artist, t.album),
                            Style::default().fg(MUTED),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel)
                    .highlight_style(
                        Style::default()
                            .fg(GREEN)
                            .bg(PANEL)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                area,
                &mut self.library_selection,
            );
        }
    }
    fn queue(&mut self, frame: &mut Frame, area: Rect) {
        let title = format!(
            " QUEUE · {} entries · Tab / library ",
            self.state.queue.len()
        );
        let panel = block(&title, self.focus == Focus::Queue);
        if self.state.queue.is_empty() {
            frame.render_widget(Paragraph::new("\n  Nothing queued yet.\n\n  Enter plays a library track.\n  e adds it without interrupting playback.\n\n  Or: vtamp play /path/to/music").block(panel).style(Style::default().fg(MUTED)).wrap(Wrap { trim: false }), area);
        } else {
            let items: Vec<_> = self
                .state
                .queue
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let current = Some(&q.id) == self.state.current_id.as_ref();
                    ListItem::new(vec![
                        Line::styled(
                            format!(
                                "{} {:02}  {}",
                                if current { "▶" } else { " " },
                                i + 1,
                                q.track.title
                            ),
                            Style::default().fg(if current { GREEN } else { TEXT }),
                        ),
                        Line::styled(
                            format!(
                                "       {} · {}",
                                q.track.artist,
                                display_time(q.track.duration_ms)
                            ),
                            Style::default().fg(MUTED),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel)
                    .highlight_style(Style::default().bg(PANEL).add_modifier(Modifier::BOLD))
                    .highlight_symbol("› "),
                area,
                &mut self.queue_selection,
            );
        }
    }
}

fn block(title: &str, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title.to_owned())
        .border_style(Style::default().fg(if active { GREEN } else { BORDER }))
}
fn clamp_selection(state: &mut ListState, len: usize) {
    state.select(if len == 0 {
        None
    } else {
        Some(state.selected().unwrap_or(0).min(len - 1))
    });
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn placeholder() -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 64, |x, y| {
        let stripe = (x + y) / 9 % 4;
        image::Rgb(match stripe {
            0 => [180, 246, 118],
            1 => [88, 130, 72],
            2 => [45, 66, 43],
            _ => [27, 38, 27],
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_empty_and_tiny_layouts_render_without_overflow() {
        let (tx, _rx) = sync_mpsc::channel();
        let mut app = App {
            state: State::default(),
            tracks: vec![],
            total: 0,
            offset: 0,
            query: "가 음악 🎵".into(),
            library_selection: ListState::default(),
            queue_selection: ListState::default(),
            focus: Focus::Library,
            input: None,
            help: false,
            connected: true,
            notice: String::new(),
            notice_at: Instant::now(),
            last_progress: Instant::now(),
            picker: Picker::halfblocks(),
            cover: ThreadProtocol::new(tx, None),
            cover_key: None,
            show_art: false,
        };
        for (width, height) in [(1, 1), (30, 8), (40, 12), (80, 24), (120, 36)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            app.help = true;
            terminal.draw(|f| app.draw(f)).unwrap();
            app.help = false;
        }
    }
}
