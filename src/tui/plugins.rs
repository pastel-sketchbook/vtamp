use super::*;
use crate::plugin::{Catalog, Context, Session, Target, Update};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Default)]
pub(super) struct Extensions {
    pub catalog: Catalog,
    pub paths: Option<platform::Paths>,
    pub notify: Arc<Notify>,
    menu: Option<Menu>,
    panel: Option<Panel>,
}

#[derive(Default)]
struct Menu {
    query: String,
    selected: usize,
}

struct Panel {
    session: Option<Session>,
    target: Target,
    pinned: Option<Track>,
    context: Context,
    update: Arc<Update>,
    rows: Vec<(usize, String)>,
    width: usize,
    offset: usize,
    height: usize,
    selected: usize,
    follow: bool,
    actions: Option<usize>,
}

impl Extensions {
    pub fn new(catalog: Catalog, paths: platform::Paths) -> Self {
        Self {
            catalog,
            paths: Some(paths),
            ..Default::default()
        }
    }
    pub fn active(&self) -> bool {
        self.menu.is_some() || self.panel.is_some()
    }
    pub async fn close(&mut self) {
        if let Some(panel) = self.panel.take()
            && let Some(session) = panel.session
        {
            session.close().await;
        }
    }
    fn choices(&self) -> Vec<(String, String)> {
        let query = self
            .menu
            .as_ref()
            .map_or("", |menu| &menu.query)
            .to_lowercase();
        self.catalog
            .plugins
            .iter()
            .flat_map(|plugin| {
                plugin.manifest.commands.iter().map(|command| {
                    (
                        format!("{}:{}", plugin.manifest.id, command.id),
                        format!("{} · {}", plugin.manifest.name, command.title),
                    )
                })
            })
            .filter(|(id, title)| format!("{id} {title}").to_lowercase().contains(&query))
            .collect()
    }
}

impl App {
    pub(super) fn open_extensions(&mut self) {
        self.extensions.panel = None;
        self.extensions.menu = Some(Menu::default());
        self.video_fullscreen = None;
    }
    pub(super) fn start_extension(&mut self, name: &str) {
        let result = self.extensions.catalog.resolve(name);
        let (plugin, command) = match result {
            Ok(pair) => pair,
            Err(error) => {
                self.notice(format!("{error:#}"));
                return;
            }
        };
        let pinned = (command.target == Target::Selected)
            .then(|| self.selected_track().cloned())
            .flatten();
        if command.target == Target::Selected && pinned.is_none() {
            self.notice("Select a track first.");
            return;
        }
        let Some(paths) = self.extensions.paths.clone() else {
            return;
        };
        self.extensions.menu = None;
        self.extensions.panel = None;
        self.video_fullscreen = None;
        let context =
            Context::from_state(&self.state, self.connected, command.target, pinned.as_ref());
        let target = command.target;
        let session = Session::start(
            plugin,
            command,
            context.clone(),
            paths,
            self.extensions.notify.clone(),
        );
        self.extensions.panel = Some(Panel {
            session: Some(session),
            target,
            pinned,
            context,
            update: Arc::new(Update::default()),
            rows: vec![],
            width: 0,
            offset: 0,
            height: 1,
            selected: 0,
            follow: true,
            actions: None,
        });
    }
    pub(super) fn extension_context(&mut self) {
        if let Some(panel) = &mut self.extensions.panel {
            let old = panel.context.generation;
            if panel.context.advance(Context::from_state(
                &self.state,
                self.connected,
                panel.target,
                panel.pinned.as_ref(),
            )) {
                if old != panel.context.generation {
                    let finished = panel.update.finished;
                    panel.update = Arc::new(Update {
                        generation: panel.context.generation,
                        finished,
                        notice: if finished {
                            "Command completed. Reopen it for the current track.".into()
                        } else {
                            String::new()
                        },
                        ..Default::default()
                    });
                    panel.rows.clear();
                    panel.width = 0;
                    panel.offset = 0;
                    panel.selected = 0;
                    panel.follow = true;
                    panel.actions = None;
                }
                if let Some(session) = &panel.session {
                    session.context(panel.context.clone());
                }
            }
        }
    }
    pub(super) fn extension_updates(&mut self) {
        let Some(panel) = &mut self.extensions.panel else {
            return;
        };
        let Some(session) = &mut panel.session else {
            return;
        };
        let update = session.updates.borrow_and_update().clone();
        if update.generation != panel.context.generation && !update.error {
            return;
        }
        let same_text = match (&update.view, &panel.update.view) {
            (Some(next), Some(old)) => {
                next.items.len() == old.items.len()
                    && next
                        .items
                        .iter()
                        .zip(&old.items)
                        .all(|(a, b)| a.text == b.text)
            }
            _ => false,
        };
        if !same_text {
            panel.width = 0;
        }
        panel.update = update;
    }
    pub(super) fn extension_key(&mut self, key: KeyEvent) {
        if self.extensions.menu.is_some() {
            let choices = self.extensions.choices();
            let menu = self.extensions.menu.as_mut().unwrap();
            match key.code {
                KeyCode::Esc => self.extensions.menu = None,
                KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
                KeyCode::Down => {
                    menu.selected = (menu.selected + 1).min(choices.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some((name, _)) = choices.get(menu.selected) {
                        self.start_extension(name);
                    }
                }
                _ => {
                    if (menu.query.len() < 1024 || !matches!(key.code, KeyCode::Char(_)))
                        && edit_line(&mut menu.query, key)
                    {
                        menu.selected = 0;
                    }
                }
            }
            return;
        }
        let Some(panel) = &mut self.extensions.panel else {
            return;
        };
        if let Some(selected) = &mut panel.actions {
            let actions = panel
                .update
                .view
                .as_ref()
                .map(|v| v.actions.as_slice())
                .unwrap_or_default();
            match key.code {
                KeyCode::Esc | KeyCode::Char('q' | 'a') => panel.actions = None,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(actions.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(action) = actions.get(*selected)
                        && let Some(session) = &panel.session
                        && let Err(error) =
                            session.action(action.id.clone(), panel.context.generation)
                    {
                        self.notice(error.to_string());
                    }
                    if let Some(panel) = &mut self.extensions.panel {
                        panel.actions = None;
                    }
                }
                _ => (),
            }
            return;
        }
        if let Some(view) = &panel.update.view
            && view.items.iter().any(|item| item.action.is_some())
        {
            let next = match key.code {
                KeyCode::Up | KeyCode::Char('k') => Some(panel.selected.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => Some(panel.selected.saturating_add(1)),
                KeyCode::PageUp => Some(panel.selected.saturating_sub(panel.height.max(1))),
                KeyCode::PageDown => Some(panel.selected.saturating_add(panel.height.max(1))),
                KeyCode::Home => Some(0),
                KeyCode::End => Some(view.items.len().saturating_sub(1)),
                _ => None,
            };
            if let Some(next) = next {
                panel.selected = next.min(view.items.len().saturating_sub(1));
                panel.follow = false;
                if let Some(first) = panel.rows.iter().position(|row| row.0 == panel.selected) {
                    let last = panel
                        .rows
                        .iter()
                        .rposition(|row| row.0 == panel.selected)
                        .unwrap_or(first);
                    if first < panel.offset {
                        panel.offset = first;
                    } else if last >= panel.offset + panel.height {
                        panel.offset = (last + 1).saturating_sub(panel.height);
                    }
                }
                return;
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.extensions.panel = None;
                return;
            }
            KeyCode::Char(':') => {
                self.open_extensions();
                return;
            }
            KeyCode::Char('a') => {
                panel.actions = Some(0);
                return;
            }
            KeyCode::Char('f') if key.modifiers.is_empty() => {
                panel.follow = true;
                return;
            }
            KeyCode::Enter => {
                if let Some(view) = &panel.update.view
                    && let Some(item) = view.items.get(panel.selected)
                    && let Some(id) = &item.action
                    && let Some(session) = &panel.session
                    && let Err(error) = session.action(id.clone(), panel.context.generation)
                {
                    self.notice(error.to_string());
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => panel.offset = panel.offset.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => panel.offset = panel.offset.saturating_add(1),
            KeyCode::PageUp => {
                panel.offset = panel
                    .offset
                    .saturating_sub(panel.height.saturating_sub(1).max(1))
            }
            KeyCode::PageDown => {
                panel.offset = panel
                    .offset
                    .saturating_add(panel.height.saturating_sub(1).max(1))
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                panel.offset = panel
                    .offset
                    .saturating_sub(panel.height.saturating_sub(1).max(1))
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                panel.offset = panel
                    .offset
                    .saturating_add(panel.height.saturating_sub(1).max(1))
            }
            KeyCode::Home => panel.offset = 0,
            KeyCode::End => panel.offset = panel.rows.len().saturating_sub(panel.height),
            _ => return,
        }
        panel.follow = false;
        panel.offset = panel.offset.min(panel.rows.len().saturating_sub(1));
        panel.selected = panel.rows.get(panel.offset).map_or(0, |row| row.0);
    }
    pub(super) fn extension_paste(&mut self, text: &str) -> bool {
        if let Some(menu) = &mut self.extensions.menu {
            if menu.query.len() < 1024 {
                menu.query
                    .extend(text.chars().filter(|c| !c.is_control()).take(256));
            }
            menu.selected = 0;
            return true;
        }
        self.extensions.panel.is_some()
    }
    pub(super) fn draw_extensions(&mut self, frame: &mut Frame, area: Rect) {
        if !self.extensions.active() {
            return;
        }
        let p = self.theme.palette();
        self.caret = None;
        frame.render_widget(Clear, area);
        if let Some(menu) = &self.extensions.menu {
            let choices = self.extensions.choices();
            let panel =
                block(p, " Extensions ", true).style(Style::default().fg(p.text).bg(p.panel));
            let inner = panel.inner(area);
            frame.render_widget(panel, area);
            let [search, body, footer] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .areas(inner);
            let columns = search.width.saturating_sub(9) as usize;
            let query: String = menu
                .query
                .graphemes(true)
                .rev()
                .scan(0, |used, part| {
                    *used += part.width();
                    (*used <= columns).then_some(part)
                })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            frame.render_widget(
                Paragraph::new(format!("Filter: {query}"))
                    .style(Style::default().fg(p.text).bg(p.panel)),
                search,
            );
            if search.width > 0 && search.height > 0 {
                self.caret = Some(Position::new(search.x + 8 + query.width() as u16, search.y));
            }
            let start = menu
                .selected
                .saturating_sub(body.height.saturating_sub(1) as usize);
            let items: Vec<_> = choices
                .iter()
                .skip(start)
                .take(body.height as usize)
                .enumerate()
                .map(|(i, (_, title))| {
                    Line::styled(
                        format!(
                            "{} {title}",
                            if start + i == menu.selected {
                                "›"
                            } else {
                                " "
                            }
                        ),
                        Style::default()
                            .fg(if start + i == menu.selected {
                                p.accent
                            } else {
                                p.text
                            })
                            .bg(p.panel),
                    )
                })
                .collect();
            frame.render_widget(Paragraph::new(items), body);
            let hint = if choices.is_empty() {
                if self.extensions.catalog.warnings.is_empty() {
                    "Esc close · No matching commands".into()
                } else {
                    self.extensions.catalog.warnings.join("; ")
                }
            } else {
                "↑/↓ choose · Enter run · Esc close".into()
            };
            frame.render_widget(
                Paragraph::new(hint).style(Style::default().fg(p.muted)),
                footer,
            );
            return;
        }
        let position = self.position();
        let Some(panel) = &mut self.extensions.panel else {
            return;
        };
        let title = panel
            .update
            .view
            .as_ref()
            .map_or("Extension", |v| v.title.as_str());
        let border =
            block(p, &format!(" {title} "), true).style(Style::default().fg(p.text).bg(p.panel));
        let inner = border.inner(area);
        frame.render_widget(border, area);
        let [body, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(inner);
        panel.height = body.height as usize;
        let Some(view) = &panel.update.view else {
            let message = if panel.update.notice.is_empty() {
                if panel.update.finished {
                    "Command completed."
                } else {
                    "Loading…"
                }
            } else {
                &panel.update.notice
            };
            frame.render_widget(
                Paragraph::new(message)
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(if panel.update.error { p.error } else { p.text })),
                body,
            );
            frame.render_widget(
                Paragraph::new("Esc/q close · : extensions").style(Style::default().fg(p.muted)),
                footer,
            );
            return;
        };
        if let Some(selected) = panel.actions {
            let lines: Vec<_> = view
                .actions
                .iter()
                .enumerate()
                .map(|(i, action)| {
                    Line::styled(
                        format!("{} {}", if i == selected { "›" } else { " " }, action.title),
                        Style::default().fg(if i == selected { p.accent } else { p.text }),
                    )
                })
                .collect();
            frame.render_widget(
                Paragraph::new(if lines.is_empty() {
                    vec![Line::from("No actions")]
                } else {
                    lines
                })
                .scroll((
                    selected.saturating_sub(body.height.saturating_sub(1) as usize) as u16,
                    0,
                )),
                body,
            );
            frame.render_widget(
                Paragraph::new("Esc back · ↑/↓ choose · Enter run")
                    .style(Style::default().fg(p.muted)),
                footer,
            );
            return;
        }
        let width = usize::from(body.width).max(1);
        if panel.width != width {
            panel.rows = view
                .items
                .iter()
                .enumerate()
                .flat_map(|(i, item)| wrap(&item.text, width).into_iter().map(move |row| (i, row)))
                .collect();
            panel.width = width;
        }
        let active =
            (panel.follow && panel.context.connected && panel.context.playback_id.is_some())
                .then(|| {
                    view.items.iter().rposition(|item| {
                        item.start_ms.is_some_and(|start| start <= position)
                            && item.end_ms.is_none_or(|end| position < end)
                    })
                })
                .flatten();
        if let Some(active) = active
            && let Some(first) = panel.rows.iter().position(|row| row.0 == active)
        {
            let band = panel.height / 4;
            if first < panel.offset + band
                || first >= panel.offset + panel.height.saturating_sub(band)
            {
                panel.offset = first.saturating_sub(panel.height / 2);
            }
        }
        panel.offset = panel
            .offset
            .min(panel.rows.len().saturating_sub(panel.height));
        let lines: Vec<_> = panel
            .rows
            .iter()
            .skip(panel.offset)
            .take(panel.height)
            .map(|(item, row)| {
                let selected = *item == panel.selected && view.items[*item].action.is_some();
                Line::styled(
                    row.clone(),
                    if active == Some(*item) || selected {
                        Style::default().fg(p.bg).bg(p.accent)
                    } else {
                        Style::default().fg(p.text).bg(p.panel)
                    },
                )
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
        let status = if !panel.update.notice.is_empty() {
            &panel.update.notice
        } else {
            &view.subtitle
        };
        frame.render_widget(
            Paragraph::new(format!("{status}\nEsc/q close · a actions · f follow"))
                .style(Style::default().fg(if panel.update.error { p.error } else { p.muted })),
            footer,
        );
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut cells = 0;
        for word in line.split_whitespace() {
            let word_width = word.width();
            if cells > 0 && cells + 1 + word_width <= width {
                row.push(' ');
                cells += 1;
            } else if cells > 0 {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            for part in word.graphemes(true) {
                let size = part.width();
                if size > width {
                    continue;
                }
                if cells + size > width {
                    rows.push(std::mem::take(&mut row));
                    cells = 0;
                }
                row.push_str(part);
                cells += size;
            }
        }
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{Action, Item, View};
    use ratatui::backend::TestBackend;

    fn panel_app(list: bool) -> App {
        let mut app = super::super::tests::app();
        let context = Context::from_state(&app.state, true, Target::Playing, None);
        app.extensions.panel = Some(Panel {
            session: None,
            target: Target::Playing,
            pinned: None,
            context,
            update: Arc::new(Update {
                generation: 1,
                view: Some(View {
                    title: "Example panel".into(),
                    subtitle: "Fixture only".into(),
                    items: (0..30)
                        .map(|i| Item {
                            text: format!("Line {i}: 한글 lyrics and a longer sentence to wrap."),
                            action: list.then(|| format!("item-{i}")),
                            start_ms: Some(i * 1000),
                            end_ms: None,
                        })
                        .collect(),
                    actions: vec![Action {
                        id: "retry".into(),
                        title: "Retry".into(),
                    }],
                }),
                ..Default::default()
            }),
            rows: vec![],
            width: 0,
            offset: 0,
            height: 1,
            selected: 0,
            follow: true,
            actions: None,
        });
        app
    }
    fn key(app: &mut App, code: KeyCode) {
        app.extension_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn panel_wraps_and_closes_at_all_layout_breakpoints() {
        for (width, height) in [(40, 12), (59, 16), (80, 24), (100, 24), (120, 40)] {
            let mut app = panel_app(false);
            let before = app.library_selection.selected();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert!(app.extensions.panel.as_ref().unwrap().height > 0);
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Example panel"), "{width}×{height}");
            assert!(!app.cover_hidden());
            key(&mut app, KeyCode::End);
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert!(app.extensions.panel.as_ref().unwrap().offset > 0);
            assert!(!app.extensions.panel.as_ref().unwrap().follow);
            key(&mut app, KeyCode::Char('f'));
            assert!(app.extensions.panel.as_ref().unwrap().follow);
            key(&mut app, KeyCode::Esc);
            assert!(!app.extensions.active());
            assert_eq!(app.library_selection.selected(), before);
        }
    }

    #[test]
    fn every_list_item_remains_reachable_near_the_last_page() {
        let mut app = panel_app(true);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        for i in 0..30 {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert_eq!(app.extensions.panel.as_ref().unwrap().selected, i);
            key(&mut app, KeyCode::Down);
        }
        key(&mut app, KeyCode::Char('a'));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.extensions.panel.as_ref().unwrap().actions, Some(0));
        key(&mut app, KeyCode::Esc);
        assert!(app.extensions.panel.as_ref().unwrap().actions.is_none());
        assert!(app.extensions.active());
    }

    #[test]
    fn resize_rewraps_and_new_track_clears_old_document() {
        let mut app = panel_app(false);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let old_width = app.extensions.panel.as_ref().unwrap().width;
        terminal.backend_mut().resize(40, 12);
        terminal.autoresize().unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(app.extensions.panel.as_ref().unwrap().width < old_width);
        app.extensions.panel.as_mut().unwrap().context.playback_id = Some("old-entry".into());
        app.extension_context();
        let panel = app.extensions.panel.as_ref().unwrap();
        assert!(panel.update.view.is_none());
        assert_eq!(panel.context.generation, 2);
    }

    #[test]
    fn menu_has_a_real_cursor_and_does_not_apply_text_to_library_search() {
        let mut app = super::super::tests::app();
        let query = app.library_query.clone();
        app.open_extensions();
        app.extension_paste("한글 search");
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(app.caret.is_some());
        assert_eq!(app.library_query, query);
        assert_eq!(app.extensions.menu.as_ref().unwrap().query, "한글 search");
        key(&mut app, KeyCode::Esc);
        assert!(!app.extensions.active());
    }

    #[test]
    fn wrapping_preserves_words_and_graphemes_in_narrow_panels() {
        for width in [1, 2, 6, 12, 40] {
            let rows = wrap(
                "한글 가사 e\u{301} 👩‍💻 superlongwordwithnospaces\nNext paragraph",
                width,
            );
            assert!(rows.iter().all(|row| row.width() <= width));
        }
        assert_eq!(wrap("abc def", 7), ["abc def"]);
        assert_eq!(wrap("abc def", 6), ["abc", "def"]);
    }

    #[test]
    fn empty_panel_cells_use_the_theme_and_completed_commands_stop_loading() {
        let mut app = panel_app(false);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| app.draw_extensions(frame, frame.area()))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(38, 8)].bg,
            app.theme.palette().panel
        );
        app.extensions.panel.as_mut().unwrap().update = Arc::new(Update {
            generation: 1,
            finished: true,
            ..Default::default()
        });
        terminal
            .draw(|frame| app.draw_extensions(frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Command completed."));
        assert!(!text.contains("Loading"));
        app.extensions.panel.as_mut().unwrap().context.playback_id = Some("previous-entry".into());
        app.extension_context();
        terminal
            .draw(|frame| app.draw_extensions(frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Command completed."));
        assert!(!text.contains("Loading"));
        app.open_extensions();
        terminal
            .draw(|frame| app.draw_extensions(frame, frame.area()))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(38, 8)].bg,
            app.theme.palette().panel
        );
    }
}
