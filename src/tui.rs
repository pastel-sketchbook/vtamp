mod imports;
use crate::{
    artwork::Artwork,
    cli::Art,
    client::Client,
    cover::{Cover, ResizeRequest, ResizeResponse},
    library::decode_image,
    model::*,
    platform,
    settings::Settings,
    spectrum::SpectrumFrame,
    spectrum_view::SpectrumView,
    theme::{Palette, Theme, channels},
    wire,
};
use anyhow::Result;
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_image::{Resize, StatefulImage};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::mpsc as sync_mpsc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

const PAGE_SIZE: usize = 200;

const HELP_TEXT: &str = "ATTACH / DETACH\nq / Esc / Ctrl+C   Close this interface. Music keeps playing.\n\nPLAYBACK\nSpace   Play / pause       n / b   Next / previous\n← / →   Seek 10 seconds    + / -   Volume\ns       Shuffle           R       Cycle repeat\n\nLIBRARY & QUEUE\nTab     Switch panels     j / k   Move selection\ngg / G  First / last      Ctrl-F / Ctrl-B  Page down / up (10)\n/       Search            a       Add music folder\nr       Rescan folders    [ / ]   Library pages\nEnter Play selection   Ctrl-Enter Play without queue\ne Enqueue   x/d Remove   J/K Move queue item up/down\n\nv       Toggle spectrum\nt       Choose theme (preview, then Enter to save)\n\nStop the server explicitly with: vtamp server stop";

#[derive(Default)]
struct HelpScroll {
    offset: u16,
    max: u16,
    page_height: u16,
}

// Even an empty Ratatui diff writes cursor/style escapes through Crossterm.
// Present only changed cells so an idle client doesn't keep waking the terminal.
#[derive(Default)]
struct Presentation {
    previous: Option<Buffer>,
}
impl Presentation {
    fn invalidate(&mut self) {
        self.previous = None;
    }

    fn draw<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        render: impl FnOnce(&mut Frame),
    ) -> std::result::Result<bool, B::Error> {
        terminal.autoresize()?;
        render(&mut terminal.get_frame());
        let next = terminal.current_buffer_mut();
        let changed = self.previous.as_ref().is_none_or(|previous| {
            previous.area != next.area || previous.diff_iter(next).next().is_some()
        });
        if !changed {
            next.reset();
            return Ok(false);
        }
        match &mut self.previous {
            Some(previous) => previous.clone_from(next),
            None => self.previous = Some(next.clone()),
        }
        // The TUI uses an inline text caret; the terminal cursor stays hidden.
        terminal.apply_buffer()?;
        Ok(true)
    }
}

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
    import_ui: imports::ImportUi,
    spectrum: SpectrumView,
    viewport: Rect,
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
    pending_ctrl_w: bool,
    library_jump: Option<ListEdge>,
    input: Option<Input>,
    help: bool,
    help_scroll: HelpScroll,
    connected: bool,
    initial_attachment: bool,
    notice: String,
    notice_at: Instant,
    last_progress: Instant,
    artwork: Artwork,
    cover: Cover,
    cover_key: Option<PathBuf>,
    show_art: bool,
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), event::DisableBracketedPaste);
        if std::env::var_os("TMUX").is_some() {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::style::Print("\x1b[>4;0m"));
        } else {
            let _ = crossterm::execute!(std::io::stdout(), event::PopKeyboardEnhancementFlags);
        }
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
    crossterm::execute!(std::io::stdout(), event::EnableBracketedPaste)?;
    let (artwork, _passthrough) = Artwork::detect(art);
    // Request distinct modified Enter events without changing tmux configuration.
    if std::env::var_os("TMUX").is_some() {
        crossterm::execute!(std::io::stdout(), crossterm::style::Print("\x1b[>4;2m"))?;
    } else {
        crossterm::execute!(
            std::io::stdout(),
            event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )?;
    }
    let (messages, mut incoming) = mpsc::unbounded_channel();
    let (commands, mut requests) = mpsc::channel::<Command>(64);
    let (resize_tx, resize_rx) = sync_mpsc::channel::<ResizeRequest>();
    let resize_messages = messages.clone();
    std::thread::spawn(move || {
        while let Ok(request) = resize_rx.recv() {
            let response = request.resize_encode();
            if resize_messages.send(Message::Resized(response)).is_err() {
                break;
            }
        }
    });
    let cover = Cover::new(resize_tx, None);
    let saved_spectrum = Settings::load(&settings_path)
        .map(|s| s.spectrum)
        .unwrap_or(false);
    let size = terminal.size()?;
    let mut app = App {
        import_ui: imports::ImportUi::default(),
        spectrum: SpectrumView::new(saved_spectrum),
        viewport: Rect::new(0, 0, size.width, size.height),
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
        pending_ctrl_w: false,
        library_jump: None,
        input: None,
        help: false,
        help_scroll: HelpScroll::default(),
        connected: false,
        initial_attachment: true,
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
    let (spectrum_enabled, wanted) = watch::channel(false);
    let (spectrum_frames, mut latest_spectrum) = watch::channel(None);
    let spectrum_task = tokio::spawn(spectrum_stream(client.clone(), wanted, spectrum_frames));
    let reply_messages = messages.clone();
    let command_task = tokio::spawn(async move {
        while let Some(command) = requests.recv().await {
            if matches!(
                command,
                Command::ImportPreview { .. } | Command::LibraryRetag { .. }
            ) {
                let client = client.clone();
                let messages = reply_messages.clone();
                tokio::spawn(async move {
                    let result = if let Command::ImportPreview { request } = &command {
                        async {
                            let mut value = client
                                .request(Command::ImportPreview {
                                    request: request.clone(),
                                })
                                .await?
                                .into_data()?;
                            let ids = value["preview"]["items"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v["video_id"].as_str().map(str::to_owned))
                                .collect();
                            if let Ok(reply) = client
                                .request(Command::ImportLookup { video_ids: ids })
                                .await
                                && let Ok(existing) = reply.into_data()
                            {
                                value["preview"]["existing"] = serde_json::json!(
                                    existing["video_ids"].as_array().map(Vec::len)
                                );
                            }
                            Ok::<_, anyhow::Error>(value)
                        }
                        .await
                        .map_err(|e| e.to_string())
                    } else {
                        client
                            .request(command.clone())
                            .await
                            .and_then(|r| r.into_data().map_err(Into::into))
                            .map_err(|e| e.to_string())
                    };
                    let _ = messages.send(Message::Reply(command, result));
                });
                continue;
            }
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
        let mut presentation = Presentation::default();
        let mut last_draw = Instant::now();
        let mut spectrum_stream_alive = true;
        let mut terminal_events = event::EventStream::new();
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        loop {
            let deadline = if presentation.previous.is_none() {
                Some(Instant::now())
            } else {
                app.next_redraw(last_draw)
            };
            let previous_cover_hidden = app.cover_hidden();
            let terminal_event = tokio::select! {
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline.into()).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => None,
                changed = latest_spectrum.changed(), if spectrum_stream_alive => {
                    if changed.is_err() {
                        spectrum_stream_alive = false;
                        app.spectrum.clear();
                        app.spectrum.error = Some("Spectrum disconnected. Reattach to retry.".into());
                    } else {
                        match latest_spectrum.borrow_and_update().clone() {
                            Some(Ok(frame)) if frame.current_id == app.state.current_id => app.spectrum.accept(frame),
                            Some(Err(error)) => { app.spectrum.clear(); app.spectrum.error = Some(error); },
                            _ => (),
                        }
                    }
                    // Frames wake the animation only while bars/peaks need it.
                    // Keep its 20 Hz deadline independent of stream arrival times.
                    continue;
                },
                Some(message) = incoming.recv() => {
                    app.message(message, &messages, &commands);
                    None
                },
                event = terminal_events.next() => match event {
                    Some(event) => Some(event?),
                    None => return Ok(()),
                },
                _ = terminate.recv() => return Ok::<_, anyhow::Error>(()),
                _ = interrupt.recv() => return Ok::<_, anyhow::Error>(()),
            };
            while let Ok(message) = incoming.try_recv() {
                app.message(message, &messages, &commands);
            }
            if let Some(event) = terminal_event {
                match event {
                    TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        let cover_hidden = app.cover_hidden();
                        let theme = app.theme;
                        let spectrum = app.spectrum.enabled;
                        if app.key(key, &commands)? {
                            return Ok::<_, anyhow::Error>(());
                        }
                        if cover_hidden != app.cover_hidden() || theme != app.theme || spectrum != app.spectrum.enabled {
                            // Sixel pixels aren't represented by individual text
                            // cells. Clear them when opening or closing a dialog.
                            terminal.clear()?;
                            presentation.invalidate();
                        }
                    }
                    TerminalEvent::Paste(text)=>app.import_paste(&text),
                    TerminalEvent::Resize(_, _) => {
                        terminal.clear()?;
                        presentation.invalidate();
                    },
                    _ => (),
                }
            }
            if previous_cover_hidden != app.cover_hidden() {
                terminal.clear()?;
                presentation.invalidate();
            }
            presentation.draw(&mut terminal, |frame| app.draw(frame))?;
            last_draw = Instant::now();
            let wanted = app.spectrum_visible() && app.connected;
            spectrum_enabled.send_if_modified(|value| {
                if *value == wanted { false } else { *value = wanted; true }
            });
        }
    }
    .await;
    spectrum_task.abort();
    watch_task.abort();
    command_task.abort();
    result
}

async fn spectrum_stream(
    client: Client,
    mut wanted: watch::Receiver<bool>,
    frames: watch::Sender<Option<Result<SpectrumFrame, String>>>,
) {
    loop {
        if !*wanted.borrow() {
            if wanted.changed().await.is_err() {
                return;
            }
            continue;
        }
        let connection = tokio::select! {
            changed = wanted.changed() => { if changed.is_err() { return; } continue; },
            result = client.spectrum() => result,
        };
        if let Ok((first, mut stream)) = connection {
            frames.send_replace(Some(Ok(first)));
            loop {
                tokio::select! {
                    changed = wanted.changed() => {
                        if changed.is_err() { return; }
                        break;
                    },
                    reply = tokio::time::timeout(Duration::from_secs(3), wire::read::<_, Reply>(&mut stream)) => {
                        let frame = reply.ok().and_then(Result::ok).and_then(|r| r.into_data().ok())
                            .and_then(|data| serde_json::from_value::<SpectrumFrame>(data).ok());
                        match frame {
                            Some(frame) => { frames.send_replace(Some(Ok(frame))); },
                            None => break,
                        }
                    }
                }
            }
        }
        if !*wanted.borrow() {
            continue;
        }
        frames.send_replace(Some(Err(
            "Spectrum unavailable. Restart the server with this binary. v closes this view.".into(),
        )));
        tokio::select! {
            changed = wanted.changed() => { if changed.is_err() { return; } },
            _ = tokio::time::sleep(Duration::from_secs(1)) => (),
        }
    }
}

impl App {
    fn help_key(&mut self, key: KeyEvent) {
        let page = self.help_scroll.page_height.saturating_sub(1).max(1);
        let offset = self.help_scroll.offset;
        self.help_scroll.offset = match key.code {
            KeyCode::Esc | KeyCode::Char('q' | '?') => {
                self.help = false;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => offset.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => offset.saturating_sub(1),
            KeyCode::PageDown => offset.saturating_add(page),
            KeyCode::PageUp => offset.saturating_sub(page),
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                offset.saturating_add(page)
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                offset.saturating_sub(page)
            }
            KeyCode::Home => 0,
            KeyCode::End => self.help_scroll.max,
            _ => offset,
        }
        .min(self.help_scroll.max);
    }

    fn draw_help(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let popup = centered(area, 78, 26);
        frame.render_widget(Clear, popup);
        let panel = block(p, " vtamp / key reference ", true)
            .style(Style::default().fg(p.text).bg(p.panel));
        let inner = panel.inner(popup);
        frame.render_widget(panel, popup);
        let paragraph = Paragraph::new(if self.import_ui.enabled {format!("{HELP_TEXT}\n\nYOUTUBE IMPORT\na   Add folder or YouTube URL\ni   Import progress / cancel / retry\nm   Edit title / artist / album\no / O   Open video / channel")} else {HELP_TEXT.to_owned()}).wrap(Wrap { trim: false });
        // Count the actual wrapped rows so narrow panes can reach every line.
        let rows = paragraph.line_count(inner.width).min(u16::MAX as usize) as u16;
        let scrollable = rows > inner.height.saturating_sub(1);
        let [body, hint] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(if scrollable { 2 } else { 1 }),
        ])
        .areas(inner);
        self.help_scroll.page_height = body.height;
        self.help_scroll.max = rows.saturating_sub(body.height);
        self.help_scroll.offset = self.help_scroll.offset.min(self.help_scroll.max);
        frame.render_widget(paragraph.scroll((self.help_scroll.offset, 0)), body);
        let start = self.help_scroll.offset.saturating_add(1).min(rows);
        let end = self
            .help_scroll
            .offset
            .saturating_add(body.height)
            .min(rows);
        let hint_text = if scrollable {
            format!("↑/↓ j/k scroll · PgUp/PgDn page\nEsc/q/? close · {start}–{end}/{rows}")
        } else {
            "Esc/q/? close".into()
        };
        frame.render_widget(
            Paragraph::new(hint_text).style(Style::default().fg(p.muted).bg(p.panel)),
            hint,
        );
    }

    fn spectrum_visible(&self) -> bool {
        self.spectrum.enabled
            && !self.cover_hidden()
            && self.theme_picker.is_none()
            && self.viewport.width >= 40
            && self.viewport.height >= 12
    }

    fn next_redraw(&self, last_draw: Instant) -> Option<Instant> {
        let playing = self.connected && self.state.status == PlaybackStatus::Playing;
        let animation = if self.spectrum_visible() && self.spectrum.needs_animation(playing) {
            Some(last_draw + Duration::from_millis(50))
        } else if playing && self.viewport.width >= 40 && self.viewport.height >= 12 {
            // Keep the progress gauge responsive, including short tracks. Identical
            // text/cells are discarded before sending anything to the terminal.
            Some(last_draw + Duration::from_millis(100))
        } else {
            None
        };
        let expiry = self.notice_at + Duration::from_secs(6);
        let notice = (Instant::now() < expiry).then_some(expiry);
        animation.into_iter().chain(notice).min()
    }

    fn spectrum_replaces_list(&self) -> bool {
        self.spectrum.enabled && (self.viewport.height < 28 || self.viewport.width < 72)
    }
    fn toggle_spectrum(&mut self, enabled: bool) {
        self.spectrum.enabled = enabled;
        self.spectrum.clear();
        if let Err(error) = Settings::set_spectrum(&self.settings_path, enabled) {
            self.notice(format!(
                "Spectrum changed for this session; could not save: {error:#}"
            ));
        }
    }

    fn cover_hidden(&self) -> bool {
        // The theme picker stays in the browser area, away from album art.
        // Help and import dialogs can overlap the player and must hide pixels.
        self.help || self.import_ui.modal.is_some()
    }
    fn rebuild_cover(&mut self) {
        let palette = self.theme.palette();
        let Some(image) = self.cover_image.clone() else {
            if self.cover_loading {
                self.cover.retain_visible();
            } else {
                self.cover.empty_protocol();
            }
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
            KeyCode::Enter => match Settings::set_theme(&self.settings_path, self.theme) {
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
        let error_height = if picker.error.is_some() && area.height >= 10 {
            3
        } else {
            0
        };
        // At the minimum size the browser has five rows: one option, two hint
        // rows, and borders. Do not spend that space on an outer margin.
        let popup = if area.height < 7 {
            area
        } else {
            centered(area, 52, 15 + error_height)
        };
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
        if self.state.current_id != state.current_id {
            self.spectrum.clear();
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
                self.spectrum.clear();
                self.connected = true;
                self.state(state, messages);
                if std::mem::take(&mut self.initial_attachment)
                    && self.state.status == PlaybackStatus::Playing
                    && let Some(index) = self.state.current_index()
                {
                    self.focus = Focus::Queue;
                    self.queue_selection.select(Some(index));
                    // Reveal the selected entry even when the saved spectrum view
                    // would hide the list. Keep the saved preference unchanged.
                    if self.spectrum_replaces_list() {
                        self.spectrum.enabled = false;
                    }
                }
                self.notice("Attached. q detaches; music keeps playing.");
                self.refresh(commands);
                self.send(commands, Command::ImportAvailable);
            }
            Message::Disconnected(reason) => {
                self.spectrum.clear();
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
            Message::Event(Event::Imports(jobs)) => {
                self.import_snapshot(jobs);
                if matches!(self.import_ui.modal, Some(imports::Modal::Jobs)) {
                    self.import_detail(commands);
                }
            }
            Message::Event(Event::ImportProgress(job)) => {
                let refresh = job.terminal()
                    || self
                        .import_ui
                        .detail_at
                        .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                let selection_changed = self.import_update(job);
                if (refresh || selection_changed)
                    && matches!(self.import_ui.modal, Some(imports::Modal::Jobs))
                {
                    self.import_detail(commands);
                }
            }
            Message::Reply(command, result) => match result {
                Err(error) => {
                    if matches!(command, Command::LibraryList { ref query, offset, .. }
                    if *query == self.query && offset == self.offset)
                    {
                        self.library_jump = None;
                    }
                    if let Command::ImportPreview { request } = &command {
                        if !matches!(&self.import_ui.modal, Some(imports::Modal::Preview { request: pending, .. }) if pending.url == request.url)
                        {
                            return;
                        }
                        self.import_ui.modal = None;
                    }
                    self.notice(error);
                }
                Ok(value) => {
                    match &command {
                        Command::ImportAvailable => {
                            self.import_ui.enabled = value["available"] == true;
                            if !self.import_ui.enabled {
                                self.import_ui.modal = None;
                                self.import_ui.jobs.clear();
                            }
                            return;
                        }
                        Command::Imports => {
                            if let Ok(jobs) = serde_json::from_value(value) {
                                self.import_snapshot(jobs);
                                self.import_ui.reveal_on_snapshot = false;
                                if matches!(self.import_ui.modal, Some(imports::Modal::Jobs)) {
                                    self.import_detail(commands);
                                }
                            } else {
                                self.notice("Cannot read imports. Close and reopen to retry.");
                            }
                            return;
                        }
                        Command::ImportStatus { id, offset, .. } => {
                            if self.import_ui.offset == *offset
                                && self
                                    .import_ui
                                    .jobs
                                    .get(self.import_ui.selected)
                                    .is_some_and(|j| j.job_id == *id)
                            {
                                self.import_ui.detail = Some(value);
                            }
                            return;
                        }
                        Command::ImportPreview { request } => {
                            if let Some(imports::Modal::Preview {
                                request: pending,
                                result,
                            }) = &mut self.import_ui.modal
                                && pending.url == request.url
                            {
                                *result = serde_json::from_value(value["preview"].clone()).ok();
                            }
                            return;
                        }
                        Command::ImportStart { .. } | Command::ImportRetry { .. } => {
                            self.notice("Import started. Press i for progress.");
                            self.send(commands, Command::Imports);
                            return;
                        }
                        Command::LibraryEdit { .. } | Command::LibraryRetag { .. } => {
                            self.notice("Track metadata updated.");
                            self.refresh(commands);
                            return;
                        }
                        _ => (),
                    }
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
    fn key(&mut self, mut key: KeyEvent, commands: &mpsc::Sender<Command>) -> Result<bool> {
        // Any intervening key (including opening a prompt) cancels a prefix.
        let previous_g = std::mem::take(&mut self.pending_g);
        let previous_ctrl_w = std::mem::take(&mut self.pending_ctrl_w);
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.import_key(key, commands) {
            return Ok(false);
        }
        if self.theme_picker.is_some() {
            self.theme_key(key.code);
            return Ok(false);
        }
        if self.help {
            self.help_key(key);
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
                        if self.import_ui.enabled
                            && (path.trim().starts_with("https://")
                                || path.trim().starts_with("http://"))
                        {
                            let mut request = crate::imports::ImportRequest {
                                url: path,
                                ..Default::default()
                            };
                            match request.validate() {
                                Ok(()) if request.playlist => {
                                    self.import_ui.scroll = 0;
                                    self.import_ui.modal = Some(imports::Modal::Preview {
                                        request: request.clone(),
                                        result: None,
                                    });
                                    self.send(commands, Command::ImportPreview { request });
                                }
                                Ok(()) => self.send(commands, Command::ImportStart { request }),
                                Err(e) => self.notice(e.to_string()),
                            }
                            return Ok(false);
                        }
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
        // Vim accepts both Ctrl-W w and Ctrl-W Ctrl-W. Reuse Tab's behavior,
        // including returning from the spectrum, only outside prompts/overlays.
        if key.code == KeyCode::Char('w')
            && key.modifiers.difference(KeyModifiers::CONTROL).is_empty()
        {
            if previous_ctrl_w {
                key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
            } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                self.pending_ctrl_w = true;
                return Ok(false);
            }
        }
        if self.spectrum_replaces_list() {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    self.toggle_spectrum(false);
                    return Ok(false);
                }
                KeyCode::Char('/') => self.toggle_spectrum(false),
                KeyCode::Down
                | KeyCode::Up
                | KeyCode::PageDown
                | KeyCode::PageUp
                | KeyCode::Enter
                | KeyCode::Char('j' | 'k' | 'g' | 'G' | '[' | ']' | 'e' | 'x' | 'd' | 'J' | 'K') => {
                    return Ok(false);
                }
                KeyCode::Char('f' | 'b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(false);
                }
                _ => (),
            }
        }
        match key.code {
            KeyCode::Char('v') if key.modifiers.is_empty() => {
                self.toggle_spectrum(!self.spectrum.enabled)
            }
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
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = HelpScroll::default();
            }
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
                self.input = Some(Input::Search(String::new()));
            }
            KeyCode::Char('a') => self.input = Some(Input::Folder(String::new())),
            KeyCode::Char('i') if self.import_ui.enabled => {
                self.open_imports(commands);
            }
            KeyCode::Char('m') if self.import_ui.enabled => {
                self.import_ui.scroll = 0;
                if let Some(t) = self.selected_track() {
                    self.import_ui.modal = Some(imports::Modal::Edit {
                        id: t.id.clone(),
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        album: t.album_name().unwrap_or_default().to_owned(),
                        field: 0,
                    });
                }
            }
            KeyCode::Char('o' | 'O') if self.import_ui.enabled => {
                self.open_source(key.code == KeyCode::Char('O'))
            }
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
                    let command =
                        if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::CONTROL {
                            Command::PlayDirect {
                                path: None,
                                track: Some(id),
                            }
                        } else if key.code == KeyCode::Enter {
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
        self.viewport = area;
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
        let embedded_spectrum = self.spectrum.enabled && area.height >= 28 && area.width >= 72;
        self.now_playing(frame, now, embedded_spectrum);
        if self.spectrum_replaces_list() {
            if self.spectrum_visible() {
                self.spectrum.draw(
                    frame,
                    content,
                    p,
                    true,
                    self.connected && self.state.status == PlaybackStatus::Playing,
                );
            }
        } else if !side_by_side && area.width >= 100 {
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
        } else if let Some(error) = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.error.as_ref())
        {
            error.clone()
        } else if let Some(warning) = &self.settings_warning {
            warning.clone()
        } else if self.notice_at.elapsed() < Duration::from_secs(6) {
            self.notice.clone()
        } else if self.state.scanning {
            "Scanning folders… playback stays available.".into()
        } else if self.import_ui.enabled
            && let Some(job) = self.import_ui.jobs.iter().find(|j| !j.terminal())
        {
            format!("{} · i details", job.summary())
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
            " Space play  n/b skip  ←/→ seek  +/- vol  Tab list  / search  v spectrum  t theme  ? help  q detach"
        } else if area.width >= 52 {
            " Space play  Tab list  v spectrum  ? help  q detach"
        } else {
            " Space play  v spectrum  ? help  q detach"
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
                Input::Folder(s) => (
                    if self.import_ui.enabled {
                        " Add folder or YouTube URL · Enter adds · Esc cancels "
                    } else {
                        " Add music folder · Enter scans · Esc cancels "
                    },
                    s,
                ),
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
        self.draw_imports(frame, area);
        if self.help {
            self.draw_help(frame, area);
        }
        if self.theme_picker.is_some() {
            self.draw_theme_picker(frame, content);
        }
    }
    fn now_playing(&mut self, frame: &mut Frame, area: Rect, spectrum: bool) {
        let p = self.theme.palette();
        let panel = block(
            p,
            if self.state.direct.is_some() {
                " NOW PLAYING · NO QUEUE "
            } else {
                " NOW PLAYING "
            },
            false,
        );
        let inner = panel.inner(area);
        frame.render_widget(panel, area);
        let (cover, info) = if spectrum {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(inner);
            if self.spectrum_visible() {
                self.spectrum.draw(
                    frame,
                    right,
                    p,
                    false,
                    self.connected && self.state.status == PlaybackStatus::Playing,
                );
            }
            if self.show_art && left.width >= 28 {
                let cover_width = (left.width.saturating_sub(22)).min(inner.height * 2);
                let cover_height = (cover_width / 2).min(inner.height);
                let cover = Rect::new(left.x, left.y, cover_height * 2, cover_height);
                let info = Rect::new(
                    left.x + cover.width + 2,
                    left.y,
                    left.width - cover.width - 2,
                    left.height,
                );
                (Some(cover), info)
            } else {
                (None, left)
            }
        } else {
            now_playing_regions(inner, self.show_art)
        };
        if let Some(cover) = cover {
            // Pixel payloads cannot be clipped around dialogs. Preserve their
            // space, hide for help/themes, and redraw when they close.
            if !self.cover_hidden() {
                if self.cover.has_image() {
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
        let album = item.map_or(Some("Local files. No account. No permanent pane."), |q| {
            q.track.album_name()
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
        if names.height > 2
            && let Some(album) = album
        {
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
                            t.album_name().map_or_else(
                                || t.artist.clone(),
                                |album| format!("{} · {album}", t.artist),
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

    #[test]
    fn spectrum_layout_and_hidden_list_keys_preserve_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = navigation_app(10);
        app.settings_path = dir.path().join("ui.json");
        app.library_selection.select(Some(4));
        app.queue_selection.select(Some(2));
        let (commands, mut requests) = mpsc::channel(16);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for theme in Theme::ALL {
            app.theme = theme;
            for (width, height) in [(40, 12), (72, 12), (100, 24), (72, 28), (120, 36)] {
                app.spectrum.enabled = true;
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains("SPECTRUM"), "{width}x{height}");
                let replaces = height < 28 || width < 72;
                assert_eq!(text.contains("LIBRARY"), !replaces);
                if replaces {
                    for code in [
                        KeyCode::Enter,
                        KeyCode::Down,
                        KeyCode::Char('e'),
                        KeyCode::Char('x'),
                    ] {
                        app.key(key(code), &commands).unwrap();
                    }
                    assert_eq!(app.library_selection.selected(), Some(4));
                    assert!(requests.try_recv().is_err());
                    app.key(key(KeyCode::Tab), &commands).unwrap();
                    assert!(!app.spectrum.enabled);
                    assert!(app.focus == Focus::Library);
                    assert_eq!(app.queue_selection.selected(), Some(2));
                    app.key(key(KeyCode::Char('v')), &commands).unwrap();
                    app.key(key(KeyCode::Char('/')), &commands).unwrap();
                    assert!(!app.spectrum.enabled);
                    assert!(matches!(&app.input, Some(Input::Search(s)) if s.is_empty()));
                    app.key(key(KeyCode::Esc), &commands).unwrap();
                }
            }
        }
    }

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
                source: None,
            })
            .collect();
        app.total = count;
        app.state.queue = app.tracks.iter().cloned().map(QueueItem::new).collect();
        app.library_selection.select(Some(0));
        app.queue_selection.select(Some(0));
        app
    }

    #[test]
    fn direct_play_requires_ctrl_enter_in_library_and_does_not_focus_queue() {
        let mut app = navigation_app(3);
        let (commands, mut requests) = mpsc::channel(16);
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(requests.try_recv().is_err());
        app.key(key, &commands).unwrap();
        assert!(
            matches!(requests.try_recv().unwrap(), Command::PlayDirect { track: Some(id), path: None } if id == "0")
        );
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Play { track: Some(_), .. }
        ));
        app.viewport = Rect::new(0, 0, 100, 24);
        app.spectrum.enabled = true;
        app.key(key, &commands).unwrap();
        assert!(requests.try_recv().is_err());
        app.spectrum.enabled = false;
        app.help = true;
        app.key(key, &commands).unwrap();
        assert!(requests.try_recv().is_err());
        app.help = false;
        app.library_jump = Some(ListEdge::Last);
        app.tracks.clear();
        app.key(key, &commands).unwrap();
        assert!(requests.try_recv().is_err());

        let mut state = app.state.clone();
        let item = QueueItem::new(state.queue[1].track.clone());
        state.current_id = Some(item.id.clone());
        state.direct = Some(Box::new(item));
        state.queue_cursor = Some(state.queue[1].id.clone());
        state.status = PlaybackStatus::Playing;
        let (messages, _incoming) = mpsc::unbounded_channel();
        app.message(Message::Connected(state), &messages, &commands);
        assert!(app.focus == Focus::Library);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("NOW PLAYING · NO QUEUE"));
        assert!(text.contains("Track 1"));
    }

    #[test]
    fn playing_attachment_reveals_the_current_queue_entry_in_every_layout() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("ui.json");
        for spectrum in [false, true] {
            Settings::set_spectrum(&settings_path, spectrum).unwrap();
            for (width, height) in [(40, 12), (72, 12), (100, 24), (40, 28), (72, 28), (120, 36)] {
                let mut app = navigation_app(100);
                app.settings_path = settings_path.clone();
                app.spectrum.enabled = spectrum;
                app.viewport = Rect::new(0, 0, width, height);
                let mut state = app.state.clone();
                // Two entries share a track; select by queue entry identity.
                state.queue[80].track = state.queue[2].track.clone();
                state.current_id = Some(state.queue[80].id.clone());
                state.status = PlaybackStatus::Playing;
                let (messages, _incoming) = mpsc::unbounded_channel();
                let (commands, _requests) = mpsc::channel(16);
                app.message(Message::Connected(state), &messages, &commands);

                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(app.focus == Focus::Queue);
                assert_eq!(app.queue_selection.selected(), Some(80));
                assert!(app.queue_selection.offset() > 0);
                assert!(text.contains("› ▶ 81  Track 2"), "{width}x{height}: {text}");
                assert_eq!(
                    app.spectrum.enabled,
                    spectrum && width >= 72 && height >= 28
                );
                assert_eq!(Settings::load(&settings_path).unwrap().spectrum, spectrum);
            }
        }
    }

    #[test]
    fn attachment_focus_does_not_follow_updates_or_reconnections() {
        let mut app = navigation_app(10);
        let mut state = app.state.clone();
        state.current_id = Some(state.queue[7].id.clone());
        state.status = PlaybackStatus::Playing;
        let (messages, _incoming) = mpsc::unbounded_channel();
        let (commands, _requests) = mpsc::channel(16);
        app.message(Message::Connected(state.clone()), &messages, &commands);
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        state.current_id = Some(state.queue[8].id.clone());
        app.message(
            Message::Event(Event::State(state.clone())),
            &messages,
            &commands,
        );
        assert!(app.focus == Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(6));
        app.message(
            Message::Disconnected("Retrying…".into()),
            &messages,
            &commands,
        );
        app.message(Message::Connected(state), &messages, &commands);
        assert!(app.focus == Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(6));
    }

    #[test]
    fn attachment_without_a_playing_entry_keeps_library_focus() {
        for status in [
            PlaybackStatus::Paused,
            PlaybackStatus::Stopped,
            PlaybackStatus::Playing,
        ] {
            let mut app = navigation_app(10);
            let mut state = app.state.clone();
            state.status = status;
            if status != PlaybackStatus::Playing {
                state.current_id = Some(state.queue[7].id.clone());
            }
            let (messages, _incoming) = mpsc::unbounded_channel();
            let (commands, _requests) = mpsc::channel(16);
            app.message(Message::Connected(state.clone()), &messages, &commands);
            assert!(app.focus == Focus::Library);
            assert_eq!(app.queue_selection.selected(), Some(0));
            state.status = PlaybackStatus::Playing;
            state.current_id = Some(state.queue[7].id.clone());
            app.message(
                Message::Event(Event::State(state.clone())),
                &messages,
                &commands,
            );
            app.message(Message::Connected(state), &messages, &commands);
            assert!(app.focus == Focus::Library);
            assert_eq!(app.queue_selection.selected(), Some(0));
        }
    }

    #[test]
    fn ctrl_w_sequences_share_tab_behavior_without_changing_selection_or_playback() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = navigation_app(25);
        app.settings_path = dir.path().join("ui.json");
        app.viewport = Rect::new(0, 0, 100, 24);
        app.library_selection.select(Some(4));
        app.queue_selection.select(Some(12));
        let (commands, mut requests) = mpsc::channel(8);
        let prefix = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL] {
            let suffix = KeyEvent::new(KeyCode::Char('w'), modifiers);
            for focus in [Focus::Library, Focus::Queue] {
                app.focus = focus;
                app.key(prefix, &commands).unwrap();
                assert!(app.focus == focus);
                app.key(suffix, &commands).unwrap();
                assert!(app.focus != focus);
            }
            app.spectrum.enabled = true;
            let focus = app.focus;
            app.key(prefix, &commands).unwrap();
            app.key(suffix, &commands).unwrap();
            assert!(!app.spectrum.enabled);
            assert!(app.focus == focus);
            assert_eq!(app.library_selection.selected(), Some(4));
            assert_eq!(app.queue_selection.selected(), Some(12));
        }
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn ctrl_w_prefix_cancels_on_other_keys_and_leaves_prompts_and_overlays_alone() {
        let mut app = navigation_app(25);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let prefix = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        app.key(prefix, &commands).unwrap();
        app.key(key('j'), &commands).unwrap();
        app.key(key('w'), &commands).unwrap();
        assert_eq!(app.library_selection.selected(), Some(1));
        assert!(app.focus == Focus::Library);

        for opener in ['/', 'a', '?', 't'] {
            app.key(prefix, &commands).unwrap();
            app.key(key(opener), &commands).unwrap();
            app.key(prefix, &commands).unwrap();
            app.key(key('w'), &commands).unwrap();
            if matches!(opener, '/' | 'a') {
                assert!(
                    matches!(&app.input, Some(Input::Search(s) | Input::Folder(s)) if s == "w")
                );
            }
            assert!(app.focus == Focus::Library);
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
                .unwrap();
            app.key(key('w'), &commands).unwrap();
            assert!(app.focus == Focus::Library);
        }
        assert!(requests.try_recv().is_err());
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
        app.key(key('g'), &commands).unwrap(); // Ignore list keys inside help.
        assert!(app.help);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
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
                source: None,
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
    fn reopening_search_starts_empty_and_cancel_preserves_applied_query() {
        let mut app = app();
        app.query.clear();
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        for c in "love".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert_eq!(app.query, "love");
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryList { query, offset: 0, .. } if query == "love")
        );

        app.offset = PAGE_SIZE;
        app.library_selection.select(Some(3));
        app.focus = Focus::Queue;
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        assert!(app.focus == Focus::Library);
        assert!(matches!(&app.input, Some(Input::Search(text)) if text.is_empty()));
        app.key(key(KeyCode::Char('x')), &commands).unwrap();
        app.key(key(KeyCode::Esc), &commands).unwrap();
        assert!(app.input.is_none());
        assert_eq!(app.query, "love");
        assert_eq!(app.offset, PAGE_SIZE);
        assert_eq!(app.library_selection.selected(), Some(3));
        assert!(requests.try_recv().is_err());

        // Applying an empty new search clears the filter and returns to page 1.
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(app.query.is_empty());
        assert_eq!(app.offset, 0);
        assert_eq!(app.library_selection.selected(), Some(0));
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryList { query, offset: 0, .. } if query.is_empty())
        );
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
        app.cover = Cover::new(tx, None);
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
                    assert!(app.cover.update_resized_protocol(request.resize_encode()));
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
                for input in [Input::Search(String::new()), Input::Folder(String::new())] {
                    app.input = Some(input);
                    assert!(!app.cover_hidden());
                    terminal.draw(|f| app.draw(f)).unwrap();
                    assert_eq!(
                        terminal
                            .backend()
                            .buffer()
                            .content()
                            .iter()
                            .any(|cell| cell.symbol().contains("\x1bP")),
                        art,
                        "cover with input at {width}x{height}"
                    );
                }
                app.input = None;
                app.open_theme_picker();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert_eq!(
                    text.contains("\x1bP"),
                    art,
                    "theme cover at {width}x{height}"
                );
                assert!(text.contains("COLOR THEME"));
                assert!(text.contains("Enter save"));
                app.theme_key(KeyCode::Esc);
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
            terminal
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
        Settings {
            theme: Theme::Nord,
            ..Settings::default()
        }
        .save(&path)
        .unwrap();
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
        for (width, height) in [(40, 12), (72, 12), (120, 28)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.contains("Save failed"),
                "save error at {width}x{height}"
            );
            assert!(text.contains("Enter save"));
            assert!(text.contains("Esc/q cancel"));
        }
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
            source: None,
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
        app.cover = Cover::new(tx, None);
        app.rebuild_cover();
        app.cover.resize_encode(&Resize::Fit(None), (8, 8).into());
        let old_encoding = rx.recv().unwrap().resize_encode();
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
        let (resize_tx, resize_rx) = sync_mpsc::channel();
        app.cover = Cover::new(resize_tx, None);
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
                while let Ok(request) = resize_rx.try_recv() {
                    app.cover.update_resized_protocol(request.resize_encode());
                }
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
            import_ui: imports::ImportUi::default(),
            spectrum: SpectrumView::new(false),
            viewport: Rect::default(),
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
            pending_ctrl_w: false,
            library_jump: None,
            input: None,
            help: false,
            help_scroll: HelpScroll::default(),
            connected: true,
            initial_attachment: true,
            notice: String::new(),
            notice_at: Instant::now(),
            last_progress: Instant::now(),
            artwork: Artwork::detect(Art::Halfblocks).0,
            cover: Cover::new(tx, None),
            cover_key: None,
            show_art: false,
        }
    }

    #[test]
    fn optional_import_ui_is_silent_without_downloader() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        let (commands, mut requests) = mpsc::channel(16);
        for key in ['i', 'm', 'o', 'O'] {
            app.key(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
        }
        assert!(app.import_ui.modal.is_none());
        assert!(requests.try_recv().is_err());
        app.help = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.to_lowercase().contains("youtube"));
        app.help = false;
        app.key(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("URL"));
        assert!(text.contains("folder"));
    }

    #[test]
    fn dismissed_preview_failure_cannot_close_a_new_dialog() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Edit {
            id: "track".into(),
            title: "Song".into(),
            artist: "Singer".into(),
            album: String::new(),
            field: 0,
        });
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::channel(8);
        let command = Command::ImportPreview {
            request: crate::imports::ImportRequest {
                url: "https://youtube.com/playlist?list=PLold".into(),
                ..Default::default()
            },
        };
        app.message(
            Message::Reply(command, Err("Old preview failed".into())),
            &messages,
            &commands,
        );
        assert!(matches!(
            app.import_ui.modal,
            Some(imports::Modal::Edit { .. })
        ));
        assert!(app.notice.is_empty());
    }

    #[test]
    fn absent_albums_hide_in_player_and_library_and_edit_can_clear_them() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.import_ui.enabled = true;
        let track = Track {
            id: "track".into(),
            path: "/example.m4a".into(),
            title: "Song".into(),
            artist: "Singer".into(),
            album: String::new(),
            track_number: 0,
            duration_ms: 180_000,
            cover: None,
            source: None,
        };
        app.tracks = vec![track.clone()];
        app.total = 1;
        app.state.queue.push(QueueItem::new(track));
        app.state.current_id = Some(app.state.queue[0].id.clone());
        app.library_selection.select(Some(0));
        let (commands, mut requests) = mpsc::channel(16);
        for (width, height) in [(40, 12), (80, 24), (120, 36)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for album in ["", "   ", "Unknown album", "Known album"] {
                app.tracks[0].album = album.into();
                app.state.queue[0].track.album = album.into();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(!text.contains("Unknown album"));
                if width >= 80 {
                    assert_eq!(
                        text.matches("Known album").count(),
                        if album == "Known album" { 2 } else { 0 }
                    );
                }
                terminal.draw(|f| app.library(f, f.area())).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert_eq!(text.contains("Singer ·"), album == "Known album");
            }
            app.key(
                KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
            // Shift-Tab from Title wraps directly to Album.
            app.key(
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                &commands,
            )
            .unwrap();
            app.key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &commands,
            )
            .unwrap();
            app.import_paste("New album\n");
            terminal.draw(|f| app.draw(f)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("› Album (optional)"));
            assert!(text.contains("New album"));
            assert!(text.contains("Enter save"));
            app.key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &commands,
            )
            .unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Leave blank to hide"));
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
                .unwrap();
            assert!(
                matches!(requests.try_recv().unwrap(), Command::LibraryEdit { album: Some(a), .. } if a.is_empty())
            );
            assert!(app.import_ui.modal.is_none());
        }
        // Long earlier fields must not push the active Album field out of view.
        app.tracks[0].title = "Long title ".repeat(20);
        app.key(
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        for _ in 0..2 {
            app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
                .unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("› Album (optional)"));
        assert!(text.contains("Known album"));
    }

    #[test]
    fn imports_reopen_reveals_new_job_and_resets_the_previous_item_page() {
        let mut app = app();
        app.import_ui.enabled = true;
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(32);
        let mut old = crate::imports::ImportJob::new(&Default::default());
        old.title = "Previous import".into();
        old.started_at_ms = 100;
        old.total = Some(10);
        old.finish("completed");
        app.import_ui.jobs = vec![old.clone()];
        app.import_ui.offset = 8;
        app.import_ui.detail = Some(serde_json::json!({"job":old,"items":[]}));
        let mut new = crate::imports::ImportJob::new(&Default::default());
        new.title = "Latest import".into();
        new.started_at_ms = 200;
        new.total = Some(1);
        app.message(
            Message::Event(Event::Imports(vec![new.clone(), old.clone()])),
            &messages,
            &commands,
        );
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            new.job_id
        );
        assert_eq!(app.import_ui.offset, 0);
        assert!(app.import_ui.detail.is_none());
        assert!(matches!(requests.try_recv().unwrap(), Command::Imports));
        app.message(
            Message::Reply(Command::Imports, Ok(serde_json::json!([new, old]))),
            &messages,
            &commands,
        );
        assert!(
            matches!(requests.try_recv().unwrap(), Command::ImportStatus { id, offset: 0, .. } if id == new.job_id)
        );
        new.finish("completed");
        app.message(
            Message::Event(Event::ImportProgress(new.clone())),
            &messages,
            &commands,
        );
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            new.job_id
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].status,
            "completed"
        );
        for (width, height) in [(40, 12), (102, 27)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Latest import"), "{text}");
            assert!(
                text.contains("imports") || text.contains("Imports"),
                "{text}"
            );
            assert!(!text.contains("Job 1/2"), "{text}");
        }
    }

    #[test]
    fn imports_keep_open_job_identity_across_list_replies_and_progress_insertions() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(32);
        let mut selected = crate::imports::ImportJob::new(&Default::default());
        selected.started_at_ms = 100;
        selected.total = Some(9);
        selected.finish("failed");
        app.import_ui.jobs = vec![selected.clone()];
        app.import_ui.offset = 5;
        app.import_ui.scroll = 3;
        app.import_ui.detail = Some(serde_json::json!({"job":selected,"items":[]}));
        let mut newer = crate::imports::ImportJob::new(&Default::default());
        newer.started_at_ms = 200;
        app.message(
            Message::Event(Event::ImportProgress(newer.clone())),
            &messages,
            &commands,
        );
        assert_eq!(app.import_ui.selected, 1);
        assert_eq!(app.import_ui.offset, 5);
        assert_eq!(app.import_ui.scroll, 3);
        assert!(app.import_ui.detail.is_some());
        app.message(
            Message::Reply(Command::Imports, Ok(serde_json::json!([newer, selected]))),
            &messages,
            &commands,
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            selected.job_id
        );
        while requests.try_recv().is_ok() {}
        app.key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(
            matches!(requests.try_recv().unwrap(), Command::ImportRetry { id } if id == selected.job_id)
        );
        // Once an old selected job leaves server retention, discard its page.
        app.message(
            Message::Event(Event::Imports(vec![newer.clone()])),
            &messages,
            &commands,
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            newer.job_id
        );
        assert_eq!(app.import_ui.offset, 0);
        assert!(app.import_ui.detail.is_none());
    }

    #[test]
    fn imports_open_reveals_fresh_jobs_but_late_replies_do_not_undo_navigation() {
        let mut app = app();
        app.import_ui.enabled = true;
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::channel(32);
        let mut old = crate::imports::ImportJob::new(&Default::default());
        old.started_at_ms = 100;
        old.finish("completed");
        app.import_ui.jobs = vec![old.clone()];
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        let mut running = crate::imports::ImportJob::new(&Default::default());
        running.started_at_ms = 200;
        let mut newest = crate::imports::ImportJob::new(&Default::default());
        newest.started_at_ms = 300;
        newest.finish("completed");
        app.message(
            Message::Reply(
                Command::Imports,
                Ok(serde_json::json!([newest, running, old])),
            ),
            &messages,
            &commands,
        );
        // Match the active job shown in the bottom status line, even when a
        // newer completed job appears above it in creation order.
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            running.job_id
        );
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            old.job_id
        );
        app.message(
            Message::Reply(
                Command::Imports,
                Ok(serde_json::json!([newest, running, old])),
            ),
            &messages,
            &commands,
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            old.job_id
        );
    }

    #[test]
    fn imports_stale_snapshots_cannot_erase_new_jobs_or_roll_back_completion() {
        let mut app = app();
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::channel(32);
        let mut old = crate::imports::ImportJob::new(&Default::default());
        old.started_at_ms = 100;
        app.import_ui.jobs = vec![old.clone()];
        let mut new = crate::imports::ImportJob::new(&Default::default());
        new.started_at_ms = 200;
        let stale_new = new.clone();
        new.revision = 10;
        new.finish("completed");
        old.revision = 20;
        old.finish("completed");
        app.message(
            Message::Event(Event::Imports(vec![new.clone(), old.clone()])),
            &messages,
            &commands,
        );
        old.status = "running".into();
        old.revision = 1;
        app.message(
            Message::Reply(Command::Imports, Ok(serde_json::json!([old]))),
            &messages,
            &commands,
        );
        app.message(
            Message::Event(Event::ImportProgress(stale_new)),
            &messages,
            &commands,
        );
        assert_eq!(app.import_ui.jobs.len(), 2);
        assert_eq!(app.import_ui.jobs[0].job_id, new.job_id);
        assert!(app.import_ui.jobs.iter().all(|j| j.status == "completed"));
    }

    #[test]
    fn imports_show_job_list_and_keep_selection_visible_when_space_is_limited() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        for title in [
            "Newest session",
            "Evening collection",
            "Earlier duet",
            "First recording",
        ] {
            let mut job = crate::imports::ImportJob::new(&Default::default());
            job.title = title.into();
            job.total = Some(1);
            job.added = 1;
            job.finish("completed");
            app.import_ui.jobs.push(job);
        }
        for (width, height) in [(40, 12), (72, 20), (102, 27), (120, 40)] {
            app.import_ui.selected = 0;
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            if height >= 20 {
                for job in &app.import_ui.jobs {
                    assert!(text.contains(&job.title), "{text}");
                }
                assert!(text.contains("4 imports · newest first"), "{text}");
                assert!(text.contains("Added 1 track to Library."), "{text}");
                assert!(!text.contains("1/1") && !text.contains("Job 1/4"), "{text}");
            }
            assert!(
                text.contains("Completed") && text.contains("Esc close"),
                "{text}"
            );
            let (commands, mut requests) = mpsc::channel(16);
            for _ in 0..3 {
                app.key(
                    KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                    &commands,
                )
                .unwrap();
            }
            assert_eq!(app.import_ui.selected, 3);
            while let Ok(command) = requests.try_recv() {
                assert!(matches!(command, Command::ImportStatus { .. }));
            }
            terminal.draw(|f| app.draw(f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("› First recording"), "{text}");
        }
    }

    #[test]
    fn imports_separate_source_and_saved_title_and_hide_finished_transfer_stats() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.title = "한글 공연 라이브 · 긴 영상 제목과 여러 곡의 소개가 이어지는 녹화 영상".into();
        job.current_title = Some("Evening session".into());
        job.total = Some(1);
        job.status = "running".into();
        job.stage = "downloading".into();
        job.progress.bytes = Some(512);
        job.progress.total = Some(1024);
        app.import_ui.jobs.push(job.clone());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(72, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains('…') && text.contains("Downloading"), "{text}");
        assert!(
            text.contains("50%") && text.contains("Track: Evening session"),
            "{text}"
        );
        job.added = 1;
        job.progress.bytes = Some(1024);
        job.finish("completed");
        app.import_ui.jobs[0] = job.clone();
        app.import_ui.detail = Some(
            serde_json::json!({"job":job,"items":[{"index":0,"status":"completed","title":"Evening session"}]}),
        );
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.contains("YouTube:") && text.contains("Saved as: Evening session"),
            "{text}"
        );
        assert_eq!(text.matches("Evening session").count(), 1, "{text}");
        assert!(
            !text.contains("100%") && !text.contains("MiB") && !text.contains("Failed 0"),
            "{text}"
        );
    }

    #[test]
    fn imports_show_only_applicable_actions_and_paint_the_light_theme_panel() {
        let mut app = app();
        app.theme = Theme::CatppuccinLatte;
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.title = "Test recording".into();
        job.total = Some(1);
        job.added = 1;
        job.finish("completed");
        app.import_ui.jobs.push(job);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Added 1 track to Library."), "{text}");
        assert!(
            !text.contains("cancel") && !text.contains("retry") && !text.contains("[/]"),
            "{text}"
        );
        let p = app.theme.palette();
        for y in 1..11 {
            for x in 2..38 {
                let bg = terminal.backend().buffer()[(x, y)].bg;
                assert!(
                    bg == p.panel || bg == p.selection,
                    "unpainted panel at {x},{y}"
                );
            }
        }
        let (commands, mut requests) = mpsc::channel(16);
        for code in [KeyCode::Char('c'), KeyCode::Char('r')] {
            app.key(KeyEvent::new(code, KeyModifiers::NONE), &commands)
                .unwrap();
        }
        assert!(
            requests.try_recv().is_err(),
            "completed imports have no cancel/retry action"
        );
        app.import_ui.jobs[0].finish("failed");
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.contains("r retry unfinished tracks") && !text.contains("c cancel"),
            "{text}"
        );
        app.key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::ImportRetry { .. }
        ));
    }

    #[test]
    fn import_details_scroll_on_minimum_terminal_and_do_not_control_playback() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.import_ui.enabled = true;
        let request = crate::imports::ImportRequest {
            url: "https://youtube.com/playlist?list=PLtest".into(),
            ..Default::default()
        };
        let mut job = crate::imports::ImportJob::new(&request);
        job.total = Some(2);
        app.import_ui.detail = Some(
            serde_json::json!({"job":job,"items":[{"index":0,"title":"First","status":"failed","error":format!("{} END-OF-ERROR", "diagnostic ".repeat(40))}]}),
        );
        app.import_ui.jobs.push(job);
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let (commands, mut requests) = mpsc::channel(16);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        for _ in 0..20 {
            app.key(
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
        }
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("END-OF-ERROR"), "{text}");
        let bottom = app.import_ui.scroll;
        let page = app.import_ui.page_height;
        assert!(page <= 3, "minimum-size details must not skip unread rows");
        app.key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(app.import_ui.scroll, bottom.saturating_sub(page));
        app.key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(requests.try_recv().is_err());
        app.key(
            KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::ImportStatus {
                offset: 1,
                limit: 1,
                ..
            }
        ));
    }

    #[test]
    fn help_scrolls_to_the_last_wrapped_line_without_playback_actions() {
        use ratatui::backend::TestBackend;
        let (commands, mut requests) = mpsc::channel(16);
        let draw = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        for (width, height, scrollable) in [
            (40, 12, true),
            (72, 12, true),
            (100, 20, true),
            (100, 24, true),
            (100, 25, false),
            (120, 28, false),
        ] {
            let mut app = app();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let press = |app: &mut App, key| app.key(key, &commands).unwrap();
            let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
            press(&mut app, key(KeyCode::Char('?')));
            let top = draw(&mut app, &mut terminal);
            assert!(top.contains("ATTACH / DETACH"));
            assert_eq!(top.contains("↑/↓ j/k scroll"), scrollable);
            assert_eq!(top.contains("Esc/q/? close ·"), scrollable);
            assert!(top.contains("Esc/q/? close"));
            for code in [KeyCode::Down, KeyCode::Char('j'), KeyCode::PageDown] {
                press(&mut app, key(code));
                draw(&mut app, &mut terminal);
                assert!(app.help);
            }
            press(&mut app, key(KeyCode::End));
            let bottom = draw(&mut app, &mut terminal);
            assert!(bottom.contains("vtamp server stop"), "{width}x{height}");
            for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::Char('j')] {
                press(&mut app, key(code));
                assert_eq!(draw(&mut app, &mut terminal), bottom);
            }
            // Commands behind the modal must neither run nor dismiss it.
            for code in [' ', 'v', 'e', 'r', 'b'] {
                press(&mut app, key(KeyCode::Char(code)));
                assert!(app.help);
            }
            press(
                &mut app,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            );
            if app.help_scroll.max > 0 {
                assert!(app.help_scroll.offset < app.help_scroll.max);
            }
            press(&mut app, key(KeyCode::Home));
            assert_eq!(draw(&mut app, &mut terminal), top);
            press(&mut app, key(KeyCode::End));
            draw(&mut app, &mut terminal);
            press(&mut app, key(KeyCode::Esc));
            assert!(!app.help);
            press(&mut app, key(KeyCode::Char('?')));
            assert_eq!(draw(&mut app, &mut terminal), top);
            press(&mut app, key(KeyCode::Char('q')));
            assert!(!app.help);
        }
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn help_scroll_clamps_after_resize_and_keeps_close_controls_visible() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.help = true;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.help_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert!(app.help_scroll.offset > 0);
        terminal.backend_mut().resize(120, 28);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.help_scroll.offset, 0);
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("ATTACH / DETACH"));
        assert!(text.contains("vtamp server stop"));
        assert!(text.contains("Esc/q/? close"));
        assert!(!text.contains("↑/↓ j/k scroll"));
        assert!(!text.contains("Esc/q/? close ·"));
    }

    #[test]
    fn unchanged_frames_write_no_terminal_escape_sequences() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        let mut output = Vec::new();
        {
            let mut terminal = Terminal::with_options(
                CrosstermBackend::new(&mut output),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
                },
            )
            .unwrap();
            let mut presentation = Presentation::default();
            let draw = |f: &mut Frame| f.render_widget("same screen", f.area());
            assert!(presentation.draw(&mut terminal, draw).unwrap());
            for _ in 0..20 {
                assert!(!presentation.draw(&mut terminal, draw).unwrap());
            }
            assert!(
                presentation
                    .draw(&mut terminal, |f| f.render_widget("changed", f.area()))
                    .unwrap()
            );
        }
        assert_eq!(output.windows(6).filter(|w| *w == b"\x1b[?25l").count(), 2);
    }

    #[test]
    fn clearing_and_resizing_force_presentation_of_identical_content() {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut presentation = Presentation::default();
        let draw = |f: &mut Frame| f.render_widget("same screen", f.area());
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        assert!(!presentation.draw(&mut terminal, draw).unwrap());
        terminal.clear().unwrap();
        presentation.invalidate();
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "s");
        terminal.backend_mut().resize(50, 14);
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "s");
        assert!(!presentation.draw(&mut terminal, draw).unwrap());
    }

    #[test]
    fn redraw_deadlines_sleep_when_idle_and_preserve_notice_expiry() {
        let mut app = app();
        app.viewport = Rect::new(0, 0, 100, 24);
        app.notice_at = Instant::now() - Duration::from_secs(7);
        let now = Instant::now();
        assert_eq!(app.next_redraw(now), None);
        app.notice("Saved");
        assert_eq!(
            app.next_redraw(now),
            Some(app.notice_at + Duration::from_secs(6))
        );
        app.state.status = PlaybackStatus::Playing;
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(100)));
        app.spectrum.enabled = true;
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(50)));
        app.help = true;
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(100)));
        app.connected = false;
        app.notice_at = now - Duration::from_secs(7);
        assert_eq!(app.next_redraw(now), None);
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
    fn pixel_cover_stays_with_search_and_themes_but_hides_under_help() {
        use ratatui_image::{FontSize, ResizeEncodeRender, picker::ProtocolType};

        let mut app = app();
        app.show_art = true;
        app.cover_image = Some(image::DynamicImage::new_rgb8(512, 512));
        app.artwork = Artwork::Native {
            protocol: ProtocolType::Sixel,
            font_size: FontSize::new(10, 20),
            tmux: true,
        };
        let protocol = app.artwork.new_resize_protocol(
            image::DynamicImage::new_rgb8(512, 512),
            cover_background(app.theme.palette()),
        );
        let (tx, rx) = sync_mpsc::channel();
        app.cover = Cover::new(tx, None);
        app.cover.replace_protocol(protocol);
        app.cover.resize_encode(&Resize::Fit(None), (18, 9).into());
        app.cover
            .update_resized_protocol(rx.recv().unwrap().resize_encode());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        for (help, input, visible) in [
            (false, None, true),
            (true, None, false),
            (false, None, true),
            (false, Some(Input::Search(String::new())), true),
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
            terminal.backend().buffer()[(1, 2)]
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
