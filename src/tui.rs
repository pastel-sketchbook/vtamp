use crate::{
    artwork::Artwork,
    cli::Art,
    client::Client,
    library::decode_image,
    model::*,
    platform,
    settings::Settings,
    theme::{Palette, Theme, channels},
    wire,
};
use anyhow::Result;
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_image::{
    Resize, StatefulImage,
    thread::{ResizeRequest, ResizeResponse, ThreadProtocol},
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::mpsc as sync_mpsc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const PAGE_SIZE: usize = 200;

enum Message {
    Connected(State),
    Disconnected(String),
    Event(Event),
    Reply(Command, Result<Value, String>),
    Cover(Option<PathBuf>, Option<image::DynamicImage>),
    Resized(ResizeResponse),
}
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Library,
    Queue,
}
#[derive(Clone, Copy, PartialEq)]
enum ListEdge {
    First,
    Last,
}
impl ListEdge {
    fn index(self, len: usize) -> Option<usize> {
        len.checked_sub(1).map(|last| match self {
            Self::First => 0,
            Self::Last => last,
        })
    }
}
enum Input {
    Search(String),
    Folder(String),
}

struct ThemePicker {
    original: Theme,
    selection: ListState,
    error: Option<String>,
}

struct App {
    theme: Theme,
    theme_picker: Option<ThemePicker>,
    settings_path: PathBuf,
    settings_warning: Option<String>,
    cover_image: Option<image::DynamicImage>,
    cover_loading: bool,
    state: State,
    tracks: Vec<Track>,
    total: usize,
    offset: usize,
    query: String,
    library_selection: ListState,
    queue_selection: ListState,
    focus: Focus,
    pending_g: bool,
    library_jump: Option<ListEdge>,
    input: Option<Input>,
    help: bool,
    connected: bool,
    notice: String,
    notice_at: Instant,
    last_progress: Instant,
    artwork: Artwork,
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

fn attachment_theme(
    path: &std::path::Path,
    override_theme: Option<Theme>,
) -> (Theme, Option<String>) {
    let (saved, settings_warning) = match Settings::load(path) {
        Ok(settings) => (settings.theme, None),
        Err(error) => (
            Theme::default(),
            Some(format!("{error:#}; press t to choose and save a theme.")),
        ),
    };
    let theme = override_theme.unwrap_or(saved);
    (theme, settings_warning)
}

pub async fn run(
    client: Client,
    art: Art,
    override_theme: Option<Theme>,
    settings_path: PathBuf,
) -> Result<()> {
    let (theme, settings_warning) = attachment_theme(&settings_path, override_theme);
    let mut terminal = ratatui::try_init()?;
    let _guard = TerminalGuard;
    let (artwork, _passthrough) = Artwork::detect(art);
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
    let cover = ThreadProtocol::new(resize_tx, None);
    let mut app = App {
        theme,
        theme_picker: None,
        settings_path,
        settings_warning,
        cover_image: None,
        cover_loading: false,
        state: State::default(),
        tracks: vec![],
        total: 0,
        offset: 0,
        query: String::new(),
        library_selection: ListState::default().with_selected(Some(0)),
        queue_selection: ListState::default().with_selected(Some(0)),
        focus: Focus::Library,
        pending_g: false,
        library_jump: None,
        input: None,
        help: false,
        connected: false,
        notice: "Connecting…".into(),
        notice_at: Instant::now(),
        last_progress: Instant::now(),
        artwork,
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
                        let overlay = app.overlay();
                        let theme = app.theme;
                        if app.key(key, &commands)? {
                            return Ok::<_, anyhow::Error>(());
                        }
                        if overlay != app.overlay() || theme != app.theme {
                            // Sixel pixels aren't represented by individual text
                            // cells. Clear them when opening or closing a dialog.
                            terminal.clear()?;
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
    fn overlay(&self) -> bool {
        self.help || self.input.is_some() || self.theme_picker.is_some()
    }
    fn rebuild_cover(&mut self) {
        let palette = self.theme.palette();
        let Some(image) = self.cover_image.clone() else {
            self.cover.empty_protocol();
            return;
        };
        self.cover.replace_protocol(
            self.artwork
                .new_resize_protocol(image, cover_background(palette)),
        );
    }
    fn apply_theme(&mut self, theme: Theme) {
        if self.theme != theme {
            self.theme = theme;
            self.rebuild_cover();
        }
    }
    fn open_theme_picker(&mut self) {
        self.theme_picker = Some(ThemePicker {
            original: self.theme,
            selection: ListState::default()
                .with_selected(Theme::ALL.iter().position(|t| *t == self.theme)),
            error: None,
        });
    }
    fn theme_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Char('q') => {
                let original = self.theme_picker.take().unwrap().original;
                self.apply_theme(original);
            }
            KeyCode::Enter => match (Settings { theme: self.theme }).save(&self.settings_path) {
                Ok(()) => {
                    self.theme_picker = None;
                    self.settings_warning = None;
                    self.notice(format!(
                        "{} saved for future attachments.",
                        self.theme.name()
                    ));
                }
                Err(error) => {
                    self.theme_picker.as_mut().unwrap().error = Some(format!(
                        "Save failed: {error:#}. Enter retries; Esc cancels."
                    ))
                }
            },
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::Home
            | KeyCode::End => {
                let picker = self.theme_picker.as_mut().unwrap();
                let selected = picker.selection.selected().unwrap_or(0);
                let index = match key {
                    KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
                    KeyCode::Home => 0,
                    KeyCode::End => Theme::ALL.len() - 1,
                    _ => (selected + 1).min(Theme::ALL.len() - 1),
                };
                picker.selection.select(Some(index));
                self.apply_theme(Theme::ALL[index]);
            }
            _ => (),
        }
    }

    fn draw_theme_picker(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let picker = self.theme_picker.as_mut().unwrap();
        let error_height = if picker.error.is_some() { 3 } else { 0 };
        let popup = centered(area, 52, 15 + error_height);
        frame.render_widget(Clear, popup);
        let panel = block(p, " COLOR THEME · preview ", true)
            .style(Style::default().fg(p.text).bg(p.panel));
        let inner = panel.inner(popup);
        frame.render_widget(panel, popup);
        let [list, error, hint] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(error_height),
            Constraint::Length(2),
        ])
        .areas(inner);
        let items = Theme::ALL
            .iter()
            .map(|theme| {
                let palette = theme.palette();
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<19} {:<5} ", theme.name(), theme.mode())),
                    Span::styled("██", Style::default().fg(palette.accent)),
                    Span::styled("██", Style::default().fg(palette.text)),
                    Span::styled("██", Style::default().fg(palette.bg)),
                ]))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items).highlight_symbol("› ").highlight_style(
                Style::default()
                    .fg(p.text)
                    .bg(p.selection)
                    .add_modifier(Modifier::BOLD),
            ),
            list,
            &mut picker.selection,
        );
        if let Some(message) = &picker.error {
            frame.render_widget(
                Paragraph::new(message.as_str())
                    .style(Style::default().fg(p.error))
                    .wrap(Wrap { trim: false }),
                error,
            );
        }
        frame.render_widget(
            Paragraph::new("↑/↓ j/k preview · Enter save\nEsc/q cancel · saved for next attach")
                .style(Style::default().fg(p.muted)),
            hint,
        );
    }

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
            self.cover_image = None;
            self.cover_loading = self.show_art && cover_key.is_some();
            self.rebuild_cover();
            if self.cover_loading {
                let sender = messages.clone();
                tokio::task::spawn_blocking(move || {
                    let image = cover_key.as_ref().and_then(|p| {
                        if p.metadata().ok()?.len() > 16 * 1024 * 1024 {
                            return None;
                        }
                        decode_image(&std::fs::read(p).ok()?).ok()
                    });
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
                Err(error) => {
                    if matches!(command, Command::LibraryList { ref query, offset, .. }
                        if *query == self.query && offset == self.offset)
                    {
                        self.library_jump = None;
                    }
                    self.notice(error);
                }
                Ok(value) => {
                    if let Command::LibraryList { query, offset, .. } = command {
                        if query == self.query && offset == self.offset {
                            self.tracks =
                                serde_json::from_value(value["tracks"].clone()).unwrap_or_default();
                            self.total = value["total"].as_u64().unwrap_or(0) as usize;
                            if let Some(edge) = self.library_jump.take() {
                                // A scan may have changed the last page while it was loading.
                                self.jump_library(edge, commands);
                            } else {
                                clamp_selection(&mut self.library_selection, self.tracks.len());
                            }
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
            Message::Cover(key, image) if key == self.cover_key => {
                self.cover_loading = false;
                self.cover_image = image;
                self.rebuild_cover();
            }
            Message::Resized(response) => {
                self.cover.update_resized_protocol(response);
            }
            _ => (),
        }
    }
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> Result<bool> {
        // Any intervening key (including Tab or opening a prompt) cancels gg.
        let previous_g = std::mem::take(&mut self.pending_g);
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.theme_picker.is_some() {
            self.theme_key(key.code);
            return Ok(false);
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
                        self.library_jump = None;
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
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                if previous_g {
                    self.jump_selection(ListEdge::First, commands);
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('G') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.jump_selection(ListEdge::Last, commands);
            }
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('t') => self.open_theme_picker(),
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
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(10);
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-10);
            }
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
                self.library_jump = None;
                self.library_selection.select(Some(0));
                self.refresh(commands);
            }
            KeyCode::Char('[') if self.focus == Focus::Library => {
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.library_jump = None;
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
            KeyCode::Char('x' | 'd') if self.focus == Focus::Queue => {
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
    fn jump_selection(&mut self, edge: ListEdge, commands: &mpsc::Sender<Command>) {
        if self.focus == Focus::Queue {
            self.queue_selection
                .select(edge.index(self.state.queue.len()));
            return;
        }
        self.jump_library(edge, commands);
    }
    fn jump_library(&mut self, edge: ListEdge, commands: &mpsc::Sender<Command>) {
        let offset = match edge {
            ListEdge::First => 0,
            ListEdge::Last => self.total.saturating_sub(1) / PAGE_SIZE * PAGE_SIZE,
        };
        if offset != self.offset {
            if !self.connected {
                self.notice("Disconnected. Commands are not queued for replay.");
                return;
            }
            if commands
                .try_send(Command::LibraryList {
                    query: self.query.clone(),
                    offset,
                    limit: PAGE_SIZE,
                })
                .is_err()
            {
                self.notice("Too many pending commands; try again shortly.");
                return;
            }
            self.offset = offset;
            self.library_jump = Some(edge);
            // Never let Enter play an old page's row while the destination loads.
            self.tracks.clear();
            self.library_selection.select(None);
        } else if self.library_jump.is_some() {
            self.library_jump = Some(edge);
        } else {
            self.library_selection.select(edge.index(self.tracks.len()));
        }
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
        let p = self.theme.palette();
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(p.bg).fg(p.text)),
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
        let [header, body, status, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let side_by_side = area.height < 28 && area.width >= 72;
        let (now, content) = if side_by_side {
            // Give the browser most of the width; the player stacks its cover
            // above metadata instead of spending a full-width horizontal strip.
            let player_width = (u32::from(area.width) * 2 / 5).clamp(30, 44) as u16;
            let [now, content] =
                Layout::horizontal([Constraint::Length(player_width), Constraint::Min(1)])
                    .areas(body);
            (now, content)
        } else {
            let now_height = if area.height >= 28 {
                11
            } else if area.height >= 14 {
                6
            } else {
                4
            };
            let [now, content] =
                Layout::vertical([Constraint::Length(now_height), Constraint::Min(3)]).areas(body);
            (now, content)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " vtamp ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " / virtual terminal amplifier",
                    Style::default().fg(p.muted),
                ),
                Span::styled(
                    if area.width >= 65 {
                        format!(" · {}", self.theme.name())
                    } else {
                        String::new()
                    },
                    Style::default().fg(p.muted),
                ),
            ])),
            header,
        );
        self.now_playing(frame, now);
        if !side_by_side && area.width >= 100 {
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
        } else if let Some(warning) = &self.settings_warning {
            warning.clone()
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
            Paragraph::new(message).style(Style::default().fg(
                if self.connected
                    && self.settings_warning.is_none()
                    && self.state.last_error.is_none()
                {
                    p.muted
                } else {
                    p.warning
                },
            )),
            status,
        );
        let keys = if area.width >= 100 {
            " Space play/pause  n/b skip  ←/→ seek  +/- vol  Tab switch  / search  t theme  ? help  q detach"
        } else if area.width >= 52 {
            " Space play  Tab switch  t theme  ? help  q detach"
        } else {
            " Space play  t theme  ? help  q detach"
        };
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().bg(p.panel).fg(p.muted)),
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
                    .block(block(p, label, true))
                    .style(Style::default().fg(p.text).bg(p.panel)),
                popup,
            );
        }
        if self.help {
            let popup = centered(area, 78, 24);
            frame.render_widget(Clear, popup);
            let text = "ATTACH / DETACH\nq / Esc / Ctrl+C   Close this interface. Music keeps playing.\n\nPLAYBACK\nSpace   Play / pause       n / b   Next / previous\n← / →   Seek 10 seconds    + / -   Volume\ns       Shuffle           R       Cycle repeat\n\nLIBRARY & QUEUE\nTab     Switch panels     j / k   Move selection\ngg / G  First / last      Ctrl-F / Ctrl-B  Page down / up (10)\n/       Search            a       Add music folder\nr       Rescan folders    [ / ]   Library pages\nEnter   Play selection    e       Enqueue library selection\nx / d   Remove queue item J / K   Move queue item down / up\n\nt       Choose theme (preview, then Enter to save)\n\nStop the server explicitly with: vtamp server stop\nAny key closes help.";
            frame.render_widget(
                Paragraph::new(text)
                    .block(block(p, " vtamp / key reference ", true))
                    .style(Style::default().fg(p.text).bg(p.panel))
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        if self.theme_picker.is_some() {
            self.draw_theme_picker(frame, area);
        }
    }
    fn now_playing(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let panel = block(p, " NOW PLAYING ", false);
        let inner = panel.inner(area);
        frame.render_widget(panel, area);
        let (cover, info) = now_playing_regions(inner, self.show_art);
        if let Some(cover) = cover {
            // Pixel payloads cannot be clipped around dialogs. Preserve their
            // space, hide while an overlay is open, and redraw when it closes.
            if !self.overlay() {
                if self.cover_image.is_some() {
                    frame.render_stateful_widget(
                        StatefulImage::new().resize(Resize::Fit(None)),
                        cover,
                        &mut self.cover,
                    );
                } else if !self.cover_loading {
                    // A short stacked player can reserve only four columns for
                    // a square image. Let the label use the full player width.
                    let (x, width) = if info.y > cover.y {
                        (inner.x, inner.width)
                    } else {
                        (cover.x, cover.width)
                    };
                    frame.render_widget(
                        Paragraph::new("No album art")
                            .centered()
                            .style(Style::default().fg(p.muted)),
                        Rect::new(x, cover.y + cover.height.saturating_sub(1) / 2, width, 1),
                    );
                }
            }
        }
        let item = self.state.current();
        let title = item.map_or("Your music, your terminal.", |q| q.track.title.as_str());
        if inner.height < 4 {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        title,
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        format!("{:?} · VOL {}%", self.state.status, self.state.volume),
                        Style::default().fg(p.muted),
                    ),
                ]),
                info,
            );
            return;
        }
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
        let compact_controls = info.width < 52;
        let [names, progress, controls] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if compact_controls { 2 } else { 1 }),
        ])
        .areas(info);
        let mut lines = vec![
            Line::styled(
                title,
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Line::styled(artist, Style::default().fg(p.text)),
        ];
        if names.height > 2 {
            lines.push(Line::styled(album, Style::default().fg(p.muted)));
        }
        if names.height > 4 && !compact_controls {
            lines.push(Line::from(""));
            lines.push(Line::styled(label, Style::default().fg(p.accent)));
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
                .gauge_style(Style::default().fg(p.accent).bg(p.panel))
                .label(format!(
                    "{} / {}",
                    display_time(pos),
                    display_time(duration)
                )),
            progress,
        );
        let shuffle = if self.state.shuffle { "ON" } else { "OFF" };
        let repeat = match self.state.repeat {
            Repeat::Off => "OFF",
            Repeat::All => "ALL",
            Repeat::One => "ONE",
        };
        let control_text = if compact_controls {
            format!(
                "{label}  VOL {}%\nSHUF {shuffle}  REPEAT {repeat}",
                self.state.volume
            )
        } else {
            format!(
                "{label}  VOL {:3}%  SHUF {shuffle}  REPEAT {repeat}",
                self.state.volume
            )
        };
        frame.render_widget(
            Paragraph::new(control_text).style(Style::default().fg(p.muted)),
            controls,
        );
    }

    fn library(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let title = format!(
            " LIBRARY · {}{}{} · Tab / queue ",
            self.total,
            if area.width >= 50 { " tracks" } else { "" },
            if self.query.is_empty() {
                String::new()
            } else {
                format!(" · {}", self.query)
            }
        );
        let panel = block(p, &title, self.focus == Focus::Library);
        if self.tracks.is_empty() {
            frame.render_widget(Paragraph::new(if self.library_jump.is_some() { "\n  Loading library…" } else if self.query.is_empty() { "\n  Start with a folder of music.\n\n  Press a to add ~/Music\n  Or: vtamp library add ~/Music\n\n  Already added a folder? Press r to rescan." } else { "\n  No matching tracks.\n  Press / to change or clear the search." }).block(panel).style(Style::default().fg(p.muted)).wrap(Wrap { trim: false }), area);
        } else {
            let items: Vec<_> = self
                .tracks
                .iter()
                .map(|t| {
                    ListItem::new(vec![
                        Line::from(t.title.clone()),
                        Line::styled(
                            format!("{} · {}", t.artist, t.album),
                            Style::default().fg(p.muted),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel)
                    .highlight_style(
                        Style::default()
                            .fg(p.text)
                            .bg(p.selection)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                area,
                &mut self.library_selection,
            );
        }
    }
    fn queue(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let title = format!(
            " QUEUE · {}{} · Tab / library ",
            self.state.queue.len(),
            if area.width >= 50 { " entries" } else { "" },
        );
        let panel = block(p, &title, self.focus == Focus::Queue);
        if self.state.queue.is_empty() {
            frame.render_widget(Paragraph::new("\n  Nothing queued yet.\n\n  Enter plays a library track.\n  e adds it without interrupting playback.\n\n  Or: vtamp play /path/to/music").block(panel).style(Style::default().fg(p.muted)).wrap(Wrap { trim: false }), area);
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
                            Style::default().fg(if current { p.accent } else { p.text }),
                        ),
                        Line::styled(
                            format!(
                                "       {} · {}",
                                q.track.artist,
                                display_time(q.track.duration_ms)
                            ),
                            Style::default().fg(p.muted),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel)
                    .highlight_style(
                        Style::default()
                            .fg(p.text)
                            .bg(p.selection)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                area,
                &mut self.queue_selection,
            );
        }
    }
}

/// Retain artwork in a narrow, tall player by stacking it above the text.
/// Reserve room for title, progress, volume, shuffle, and repeat even at 12 rows.
fn now_playing_regions(inner: Rect, show_art: bool) -> (Option<Rect>, Rect) {
    if !show_art || inner.height < 7 || inner.width < 18 {
        return (None, inner);
    }
    if inner.width >= 64 {
        let [cover, _, info] = Layout::horizontal([
            Constraint::Length(inner.height.min(9) * 2),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .areas(inner);
        return (Some(cover), info);
    }
    let cover_height = inner.height.saturating_sub(7).max(2).min(inner.width / 2);
    let [slot, _, info] = Layout::vertical([
        Constraint::Length(cover_height),
        Constraint::Length(1),
        Constraint::Min(4),
    ])
    .areas(inner);
    let cover_width = slot.height * 2;
    let cover = Rect::new(
        slot.x + (slot.width - cover_width) / 2,
        slot.y,
        cover_width,
        slot.height,
    );
    (Some(cover), info)
}

fn block(p: Palette, title: &str, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Line::styled(
            title.to_owned(),
            Style::default().fg(if active { p.accent } else { p.muted }),
        ))
        .border_style(Style::default().fg(if active { p.accent } else { p.border }))
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
fn cover_background(p: Palette) -> image::Rgba<u8> {
    let [r, g, b] = channels(p.bg);
    image::Rgba([r, g, b, 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn navigation_app(count: usize) -> App {
        let mut app = app();
        app.tracks = (0..count)
            .map(|i| Track {
                id: i.to_string(),
                path: format!("/{i}.m4a").into(),
                title: format!("Track {i}"),
                artist: "Artist".into(),
                album: "Album".into(),
                track_number: i as u32,
                duration_ms: 180_000,
                cover: None,
            })
            .collect();
        app.total = count;
        app.state.queue = app.tracks.iter().cloned().map(QueueItem::new).collect();
        app.library_selection.select(Some(0));
        app.queue_selection.select(Some(0));
        app
    }

    #[test]
    fn gg_and_uppercase_g_jump_only_the_focused_list_without_playing() {
        let mut app = navigation_app(25);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        for focus in [Focus::Library, Focus::Queue] {
            app.focus = focus;
            app.library_selection.select(Some(7));
            app.queue_selection.select(Some(7));
            app.key(key('g'), &commands).unwrap();
            assert_eq!(app.library_selection.selected(), Some(7));
            assert_eq!(app.queue_selection.selected(), Some(7));
            app.key(key('g'), &commands).unwrap();
            let selected = if focus == Focus::Library {
                app.library_selection.selected()
            } else {
                app.queue_selection.selected()
            };
            assert_eq!(selected, Some(0));
            app.key(
                KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
                &commands,
            )
            .unwrap();
            let (active, inactive) = if focus == Focus::Library {
                (&app.library_selection, &app.queue_selection)
            } else {
                (&app.queue_selection, &app.library_selection)
            };
            assert_eq!(active.selected(), Some(24));
            assert_eq!(inactive.selected(), Some(7));
        }
        assert!(requests.try_recv().is_err());

        app = navigation_app(0);
        for focus in [Focus::Library, Focus::Queue] {
            app.focus = focus;
            for c in ['G', 'g', 'g'] {
                app.key(key(c), &commands).unwrap();
            }
        }
        assert_eq!(app.library_selection.selected(), None);
        assert_eq!(app.queue_selection.selected(), None);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn gg_prefix_cancels_on_other_keys_and_does_not_consume_prompt_text() {
        let mut app = navigation_app(25);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(key('G'), &commands).unwrap();
        for c in ['g', 'k', 'g'] {
            app.key(key(c), &commands).unwrap();
        }
        assert_eq!(app.library_selection.selected(), Some(23));
        app.queue_selection.select(Some(12));
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(key('g'), &commands).unwrap();
        assert_eq!(app.queue_selection.selected(), Some(12));
        app.key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            &commands,
        )
        .unwrap();
        app.key(key('g'), &commands).unwrap();
        assert_eq!(app.queue_selection.selected(), Some(12));
        app.key(key('?'), &commands).unwrap();
        app.key(key('g'), &commands).unwrap(); // Close help, without starting gg.
        app.key(key('g'), &commands).unwrap();
        assert_eq!(app.queue_selection.selected(), Some(12));

        for input in [Input::Search(String::new()), Input::Folder(String::new())] {
            app.input = Some(input);
            for c in ['g', 'g', 'G'] {
                app.key(key(c), &commands).unwrap();
            }
            assert!(
                matches!(app.input, Some(Input::Search(ref text) | Input::Folder(ref text)) if text == "ggG")
            );
        }
        assert_eq!(app.queue_selection.selected(), Some(12));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn library_edge_jumps_load_the_destination_and_ignore_stale_pages() {
        let mut app = navigation_app(PAGE_SIZE);
        app.total = 450;
        app.queue_selection.select(Some(7));
        let rows = app.tracks.clone();
        let query = app.query.clone();
        let (commands, mut requests) = mpsc::channel(8);
        let (messages, _) = mpsc::unbounded_channel();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(key('G'), &commands).unwrap();
        let request = requests.try_recv().unwrap();
        assert!(
            matches!(&request, Command::LibraryList { query: q, offset: 400, limit: PAGE_SIZE } if q == &query)
        );
        assert!(app.tracks.is_empty());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(
            requests.try_recv().is_err(),
            "Loading must not play a stale row"
        );
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        app.message(
            Message::Reply(
                Command::LibraryList {
                    query: query.clone(),
                    offset: 0,
                    limit: PAGE_SIZE,
                },
                Ok(serde_json::json!({"tracks": rows, "total": 450})),
            ),
            &messages,
            &commands,
        );
        assert!(app.tracks.is_empty());
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({"tracks": &rows[..50], "total": 450})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.library_selection.selected(), Some(49));
        assert_eq!(
            app.queue_selection.selected(),
            Some(7),
            "A reply must not move the newly focused queue"
        );

        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        for c in ['g', 'g'] {
            app.key(key(c), &commands).unwrap();
        }
        let request = requests.try_recv().unwrap();
        assert!(
            matches!(&request, Command::LibraryList { query: q, offset: 0, .. } if q == &query)
        );
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({"tracks": rows, "total": 450})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.library_selection.selected(), Some(0));

        // Removing tracks during a request can move the last page backwards.
        app.key(key('G'), &commands).unwrap();
        let request = requests.try_recv().unwrap();
        app.message(
            Message::Reply(request, Ok(serde_json::json!({"tracks": [], "total": 350}))),
            &messages,
            &commands,
        );
        let request = requests.try_recv().unwrap();
        assert!(matches!(&request, Command::LibraryList { offset: 200, .. }));
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({"tracks": &rows[..150], "total": 350})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.library_selection.selected(), Some(149));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn control_f_and_b_page_lists_without_sending_previous_track() {
        let mut app = app();
        app.tracks = (0..25)
            .map(|i| Track {
                id: i.to_string(),
                path: format!("/{i}.m4a").into(),
                title: format!("Track {i}"),
                artist: "Artist".into(),
                album: "Album".into(),
                track_number: i,
                duration_ms: 180_000,
                cover: None,
            })
            .collect();
        app.state.queue = app.tracks.iter().cloned().map(QueueItem::new).collect();
        let (commands, mut requests) = mpsc::channel(8);
        for focus in [Focus::Library, Focus::Queue] {
            app.focus = focus;
            app.library_selection.select(Some(0));
            app.queue_selection.select(Some(0));
            for (code, modifiers, expected) in [
                (KeyCode::Char('f'), KeyModifiers::CONTROL, 10),
                (KeyCode::PageDown, KeyModifiers::NONE, 20),
                (KeyCode::Char('f'), KeyModifiers::CONTROL, 24),
                (KeyCode::Char('b'), KeyModifiers::CONTROL, 14),
                (KeyCode::PageUp, KeyModifiers::NONE, 4),
                (KeyCode::Char('b'), KeyModifiers::CONTROL, 0),
            ] {
                assert!(!app.key(KeyEvent::new(code, modifiers), &commands).unwrap());
                let (active, inactive) = if focus == Focus::Library {
                    (&app.library_selection, &app.queue_selection)
                } else {
                    (&app.queue_selection, &app.library_selection)
                };
                assert_eq!(active.selected(), Some(expected));
                assert_eq!(inactive.selected(), Some(0));
            }
        }
        assert!(requests.try_recv().is_err());
        app.key(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(matches!(requests.try_recv().unwrap(), Command::Prev));
        app.input = Some(Input::Search("music".into()));
        for code in [KeyCode::Char('f'), KeyCode::Char('b')] {
            app.key(KeyEvent::new(code, KeyModifiers::CONTROL), &commands)
                .unwrap();
        }
        assert!(matches!(app.input, Some(Input::Search(ref text)) if text == "music"));
        assert_eq!(app.queue_selection.selected(), Some(0));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn short_panes_keep_pixel_art_beside_the_active_browser_through_resizes() {
        use ratatui_image::{FontSize, picker::ProtocolType};
        let mut app = app();
        app.show_art = true;
        app.artwork = Artwork::Native {
            protocol: ProtocolType::Sixel,
            font_size: FontSize::new(10, 20),
            tmux: true,
        };
        let (tx, rx) = sync_mpsc::channel();
        app.cover = ThreadProtocol::new(tx, None);
        app.cover_image = Some(image::DynamicImage::new_rgb8(64, 64));
        app.rebuild_cover();
        let (commands, mut requests) = mpsc::channel(8);
        // Cross each breakpoint in both directions while keeping the same
        // image protocol and input state, as a real tmux resize would.
        for (width, height, columns, art) in [
            (99, 28, false, true),
            (99, 27, true, true),
            (99, 24, true, true),
            (99, 20, true, true),
            (99, 14, true, true),
            (99, 12, true, true),
            (72, 24, true, true),
            (71, 24, false, false),
            (40, 12, false, false),
            (120, 20, true, true),
            (120, 28, false, true),
            (99, 24, true, true),
        ] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            for _ in 0..2 {
                terminal.draw(|f| app.draw(f)).unwrap();
                while let Ok(request) = rx.try_recv() {
                    assert!(
                        app.cover
                            .update_resized_protocol(request.resize_encode().unwrap())
                    );
                }
                terminal.draw(|f| app.draw(f)).unwrap();
                let buffer = terminal.backend().buffer();
                let row = (0..width)
                    .map(|x| buffer[(x, 1)].symbol())
                    .collect::<String>();
                assert!(row.contains("NOW PLAYING"));
                let label = if app.focus == Focus::Library {
                    "LIBRARY"
                } else {
                    "QUEUE"
                };
                assert_eq!(row.contains(label), columns, "{width}x{height}: {row}");
                assert_eq!(
                    buffer
                        .content()
                        .iter()
                        .any(|cell| cell.symbol().contains("\x1bP")),
                    art,
                    "cover at {width}x{height}"
                );
                if columns {
                    let text = buffer
                        .content()
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>();
                    for required in ["VOL", "SHUF", "REPEAT", "0:00 / 0:00"] {
                        assert!(
                            text.contains(required),
                            "missing {required} at {width}x{height}"
                        );
                    }
                }
                app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
                    .unwrap();
            }
        }
        assert!(
            requests.try_recv().is_err(),
            "layout switching must not mutate playback"
        );
        app.open_theme_picker();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(99, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(
            !terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol().contains("\x1bP"))
        );
        app.theme_key(KeyCode::Esc);
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol().contains("\x1bP"))
        );
        app.show_art = false;
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(
            !terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol().contains("\x1bP"))
        );
    }

    #[test]
    fn theme_preview_cancel_save_and_input_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        app.settings_path = dir.path().join("ui.json");
        let (tx, mut rx) = mpsc::channel(16);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('t')), &tx).unwrap();
        app.key(key(KeyCode::Down), &tx).unwrap();
        assert_eq!(app.theme, Theme::CatppuccinLatte);
        for code in [
            KeyCode::Char('x'),
            KeyCode::Char(' '),
            KeyCode::Char('n'),
            KeyCode::Tab,
        ] {
            app.key(key(code), &tx).unwrap();
        }
        assert!(rx.try_recv().is_err());
        assert!(!app.settings_path.exists());
        assert!(!app.key(key(KeyCode::Esc), &tx).unwrap());
        assert_eq!(app.theme, Theme::CatppuccinMocha);
        assert!(app.theme_picker.is_none());
        assert!(!app.settings_path.exists());
        app.key(key(KeyCode::Char('t')), &tx).unwrap();
        app.key(key(KeyCode::Down), &tx).unwrap();
        app.key(key(KeyCode::Enter), &tx).unwrap();
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().theme,
            Theme::CatppuccinLatte
        );
        assert!(app.theme_picker.is_none());
        app.open_theme_picker();
        app.theme_key(KeyCode::End);
        assert_eq!(app.theme, Theme::Classic);
        assert!(!app.key(key(KeyCode::Char('q')), &tx).unwrap());
        assert_eq!(app.theme, Theme::CatppuccinLatte);
        app.open_theme_picker();
        app.theme_key(KeyCode::End);
        assert!(
            app.key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &tx
            )
            .unwrap()
        );
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().theme,
            Theme::CatppuccinLatte
        );
    }

    #[test]
    fn attachment_override_and_broken_settings_are_non_destructive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        assert_eq!(
            attachment_theme(&path, None),
            (Theme::CatppuccinMocha, None)
        );
        Settings { theme: Theme::Nord }.save(&path).unwrap();
        assert_eq!(
            attachment_theme(&path, Some(Theme::Dracula)),
            (Theme::Dracula, None)
        );
        assert_eq!(attachment_theme(&path, None), (Theme::Nord, None));
        std::fs::write(&path, "broken").unwrap();
        let (theme, warning) = attachment_theme(&path, None);
        assert_eq!(theme, Theme::CatppuccinMocha);
        assert!(warning.unwrap().contains("Invalid UI settings"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
    }

    #[test]
    fn failed_theme_save_keeps_preview_open_and_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        app.settings_path = dir.path().join("ui.json");
        std::fs::create_dir(&app.settings_path).unwrap();
        app.open_theme_picker();
        app.theme_key(KeyCode::Down);
        app.theme_key(KeyCode::Enter);
        assert!(app.theme_picker.as_ref().unwrap().error.is_some());
        assert_eq!(app.theme, Theme::CatppuccinLatte);
        app.theme_key(KeyCode::Esc);
        assert_eq!(app.theme, Theme::CatppuccinMocha);
        assert!(app.settings_path.is_dir());
    }

    #[test]
    fn themes_render_lists_and_scrolling_picker_at_supported_sizes() {
        let mut app = app();
        let track = Track {
            id: "track".into(),
            path: "/example.m4a".into(),
            title: "음악 · After Hours".into(),
            artist: "The Night Shift".into(),
            album: "Terminal Sessions".into(),
            track_number: 1,
            duration_ms: 180_000,
            cover: None,
        };
        app.tracks = vec![track.clone()];
        app.total = 1;
        app.state.queue.push(QueueItem::new(track));
        app.state.current_id = Some(app.state.queue[0].id.clone());
        app.library_selection.select(Some(0));
        app.queue_selection.select(Some(0));
        for theme in Theme::ALL {
            app.apply_theme(theme);
            for (width, height) in [(40, 12), (80, 24), (120, 36)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                for focus in [Focus::Library, Focus::Queue] {
                    app.focus = focus;
                    terminal.draw(|f| app.draw(f)).unwrap();
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(0, 0)].bg, theme.palette().bg);
                    assert!(
                        buffer
                            .content()
                            .iter()
                            .any(|cell| cell.bg == theme.palette().selection)
                    );
                }
                app.open_theme_picker();
                app.theme_key(KeyCode::End);
                terminal.draw(|f| app.draw(f)).unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(
                    text.contains("Classic"),
                    "last option must scroll into view at {width}x{height}"
                );
                app.theme_key(KeyCode::Esc);
                app.help = true;
                terminal.draw(|f| app.draw(f)).unwrap();
                app.help = false;
                app.input = Some(Input::Search("음악".into()));
                terminal.draw(|f| app.draw(f)).unwrap();
                app.input = None;
            }
        }
    }

    #[test]
    fn cover_theme_change_rejects_stale_encoding_and_preserves_source_pixels() {
        use ratatui_image::ResizeEncodeRender;
        let mut app = app();
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            10,
            20,
            image::Rgb([200, 30, 80]),
        ));
        app.cover_image = Some(source.clone());
        let (tx, rx) = sync_mpsc::channel();
        app.cover = ThreadProtocol::new(tx, None);
        app.rebuild_cover();
        app.cover.resize_encode(&Resize::Fit(None), (8, 8).into());
        let old_encoding = rx.recv().unwrap().resize_encode().unwrap();
        app.apply_theme(Theme::CatppuccinLatte);
        assert!(!app.cover.update_resized_protocol(old_encoding));
        assert_eq!(
            app.cover.background_color(),
            Some(cover_background(app.theme.palette()))
        );
        assert_eq!(
            app.cover_image.as_ref().unwrap().as_bytes(),
            source.as_bytes()
        );
    }

    #[tokio::test]
    async fn track_changes_do_not_label_loading_covers_as_missing() {
        let cache = tempfile::tempdir().unwrap();
        let first = cache.path().join("first.png");
        let second = cache.path().join("second.png");
        let source = image::DynamicImage::new_rgb8(16, 16);
        source.save(&first).unwrap();
        source.save(&second).unwrap();
        let mut app = navigation_app(4);
        app.show_art = true;
        app.state.queue[0].track.cover = Some(first.clone());
        app.state.queue[1].track.cover = Some(second);
        app.state.queue[3].track.cover = Some(cache.path().join("missing.png"));
        let (messages, mut incoming) = mpsc::unbounded_channel();
        let (commands, _requests) = mpsc::channel(8);
        let check_label = |app: &mut App, expected| {
            for (width, height) in [(120, 36), (100, 24), (72, 12)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert_eq!(text.contains("No album art"), expected, "{width}x{height}");
            }
        };
        for index in [0, 1, 2, 3] {
            let mut state = app.state.clone();
            state.current_id = Some(state.queue[index].id.clone());
            app.state(state, &messages);
            // Render before delivering the asynchronous decode result, even
            // when the worker happens to finish immediately.
            check_label(&mut app, index == 2);
            if index == 2 {
                assert!(!app.cover_loading);
                continue;
            }
            assert!(app.cover_loading);
            if index == 1 {
                // A late failure for the old song must not end the new load.
                app.message(
                    Message::Cover(Some(first.clone()), None),
                    &messages,
                    &commands,
                );
                assert!(app.cover_loading);
                check_label(&mut app, false);
            }
            let decoded = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                .await
                .unwrap()
                .unwrap();
            app.message(decoded, &messages, &commands);
            assert!(!app.cover_loading);
            check_label(&mut app, index == 3);
        }
    }

    fn app() -> App {
        let (tx, _rx) = sync_mpsc::channel();
        App {
            theme: Theme::default(),
            theme_picker: None,
            settings_path: PathBuf::new(),
            settings_warning: None,
            cover_image: None,
            cover_loading: false,
            state: State::default(),
            tracks: vec![],
            total: 0,
            offset: 0,
            query: "가 음악 🎵".into(),
            library_selection: ListState::default(),
            queue_selection: ListState::default(),
            focus: Focus::Library,
            pending_g: false,
            library_jump: None,
            input: None,
            help: false,
            connected: true,
            notice: String::new(),
            notice_at: Instant::now(),
            last_progress: Instant::now(),
            artwork: Artwork::detect(Art::Halfblocks).0,
            cover: ThreadProtocol::new(tx, None),
            cover_key: None,
            show_art: false,
        }
    }

    #[test]
    fn unicode_empty_and_tiny_layouts_render_without_overflow() {
        let mut app = app();
        for (width, height) in [(1, 1), (30, 8), (40, 12), (80, 24), (120, 36)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            app.help = true;
            terminal.draw(|f| app.draw(f)).unwrap();
            app.help = false;
        }
    }

    #[test]
    fn pixel_cover_is_hidden_under_dialogs_and_restored_afterward() {
        use ratatui_image::{FontSize, ResizeEncodeRender, picker::ProtocolType};

        let mut app = app();
        app.show_art = true;
        app.cover_image = Some(image::DynamicImage::new_rgb8(512, 512));
        app.artwork = Artwork::Native {
            protocol: ProtocolType::Sixel,
            font_size: FontSize::new(10, 20),
            tmux: true,
        };
        let mut protocol = app.artwork.new_resize_protocol(
            image::DynamicImage::new_rgb8(512, 512),
            cover_background(app.theme.palette()),
        );
        protocol.resize_encode(&Resize::Fit(None), (18, 9).into());
        protocol.last_encoding_result().unwrap().unwrap();
        app.cover.replace_protocol(protocol);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        for (help, input, visible) in [
            (false, None, true),
            (true, None, false),
            (false, None, true),
            (false, Some(Input::Search(String::new())), false),
            (false, None, true),
        ] {
            app.help = help;
            app.input = input;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert_eq!(
                terminal.backend().buffer()[(1, 2)]
                    .symbol()
                    .contains("\x1bP"),
                visible
            );
        }
        app.open_theme_picker();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(
            !terminal.backend().buffer()[(1, 2)]
                .symbol()
                .contains("\x1bP")
        );
        app.theme_key(KeyCode::Esc);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(
            terminal.backend().buffer()[(1, 2)]
                .symbol()
                .contains("\x1bP")
        );
    }
}
