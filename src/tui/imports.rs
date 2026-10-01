use super::*;
use crate::{
    imports::{ImportJob, ImportRequest},
    youtube::Preview,
};
#[derive(Default)]
pub(super) struct ImportUi {
    pub enabled: bool,
    pub jobs: Vec<ImportJob>,
    pub modal: Option<Modal>,
    pub selected: usize,
    pub offset: usize,
    pub scroll: u16,
    pub detail_at: Option<Instant>,
    pub detail: Option<Value>,
}
pub(super) enum Modal {
    Jobs,
    Preview {
        request: ImportRequest,
        result: Option<Preview>,
    },
    Edit {
        id: String,
        title: String,
        artist: String,
        field: bool,
    },
}
impl App {
    pub(super) fn import_key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        let Some(modal) = self.import_ui.modal.as_mut() else {
            return false;
        };
        if matches!(key.code, KeyCode::Esc) {
            self.import_ui.modal = None;
            return true;
        }
        match key.code {
            KeyCode::PageDown => {
                self.import_ui.scroll = self.import_ui.scroll.saturating_add(4);
                return true;
            }
            KeyCode::PageUp => {
                self.import_ui.scroll = self.import_ui.scroll.saturating_sub(4);
                return true;
            }
            _ => (),
        }
        match modal {
            Modal::Preview { request, result } => {
                if key.code == KeyCode::Enter
                    && let Some(preview) = result
                {
                    let mut request = request.clone();
                    request.video_ids =
                        Some(preview.items.iter().map(|i| i.video_id.clone()).collect());
                    self.import_ui.modal = None;
                    self.send(commands, Command::ImportStart { request });
                } else if key.code == KeyCode::Char('q') {
                    self.import_ui.modal = None;
                }
            }
            Modal::Edit {
                id,
                title,
                artist,
                field,
            } => match key.code {
                KeyCode::Tab | KeyCode::BackTab => *field = !*field,
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if *field {
                        artist.push(c);
                    } else {
                        title.push(c);
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if *field {
                        artist.clear();
                    } else {
                        title.clear();
                    }
                }
                KeyCode::Backspace => {
                    if *field {
                        artist.pop();
                    } else {
                        title.pop();
                    }
                }
                KeyCode::Enter => {
                    let command = Command::LibraryEdit {
                        id: id.clone(),
                        title: Some(title.clone()),
                        artist: Some(artist.clone()),
                    };
                    self.import_ui.modal = None;
                    self.send(commands, command);
                }
                _ => (),
            },
            Modal::Jobs => match key.code {
                KeyCode::Char('q' | 'i') => self.import_ui.modal = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.import_ui.selected = (self.import_ui.selected + 1)
                        .min(self.import_ui.jobs.len().saturating_sub(1));
                    self.import_ui.offset = 0;
                    self.import_ui.scroll = 0;
                    self.import_detail(commands);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.import_ui.selected = self.import_ui.selected.saturating_sub(1);
                    self.import_ui.offset = 0;
                    self.import_ui.scroll = 0;
                    self.import_detail(commands);
                }
                KeyCode::Char('c') => {
                    if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected) {
                        self.send(
                            commands,
                            Command::ImportCancel {
                                id: job.job_id.clone(),
                            },
                        );
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected) {
                        self.send(
                            commands,
                            Command::ImportRetry {
                                id: job.job_id.clone(),
                            },
                        );
                    }
                }
                KeyCode::Char(']') => {
                    if let Some(j) = self.import_ui.jobs.get(self.import_ui.selected)
                        && self.import_ui.offset + 1 < j.total.unwrap_or(0)
                    {
                        self.import_ui.offset += 1;
                        self.import_ui.scroll = 0;
                        self.import_detail(commands);
                    }
                }
                KeyCode::Char('[') => {
                    self.import_ui.offset = self.import_ui.offset.saturating_sub(1);
                    self.import_ui.scroll = 0;
                    self.import_detail(commands);
                }
                _ => (),
            },
        }
        true
    }
    pub(super) fn import_detail(&mut self, commands: &mpsc::Sender<Command>) {
        if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected) {
            let _ = commands.try_send(Command::ImportStatus {
                id: job.job_id.clone(),
                offset: self.import_ui.offset,
                limit: 1,
            });
            self.import_ui.detail_at = Some(Instant::now());
        }
    }
    pub(super) fn import_update(&mut self, job: ImportJob) {
        if let Some(old) = self
            .import_ui
            .jobs
            .iter_mut()
            .find(|j| j.job_id == job.job_id)
        {
            if job.revision >= old.revision {
                *old = job;
            }
        } else {
            self.import_ui.jobs.insert(0, job);
        }
        self.import_ui.jobs.truncate(132);
    }
    pub(super) fn selected_track(&self) -> Option<&Track> {
        if self.spectrum_replaces_list() {
            return None;
        }
        match self.focus {
            Focus::Library if self.library_jump.is_none() => self
                .library_selection
                .selected()
                .and_then(|i| self.tracks.get(i)),
            Focus::Queue => self
                .queue_selection
                .selected()
                .and_then(|i| self.state.queue.get(i))
                .map(|q| &q.track),
            _ => None,
        }
    }
    pub(super) fn open_source(&mut self, channel: bool) {
        let url = self
            .selected_track()
            .and_then(|t| t.source.as_ref())
            .and_then(|s| {
                if channel {
                    s.channel_url.clone()
                } else {
                    Some(crate::youtube::video_url(&s.video_id))
                }
            });
        let Some(url) = url else {
            self.notice("This track has no YouTube source link.");
            return;
        };
        if !(url.starts_with("https://www.youtube.com/watch?v=")
            || url.starts_with("https://www.youtube.com/channel/"))
        {
            self.notice("Invalid source URL");
            return;
        }
        match std::process::Command::new("/usr/bin/open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notice("Opened YouTube in your browser.");
            }
            Err(e) => self.notice(format!("Cannot open browser: {e}")),
        }
    }
    pub(super) fn import_paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(8192)
            .collect();
        if let Some(Modal::Edit {
            title,
            artist,
            field,
            ..
        }) = &mut self.import_ui.modal
        {
            if *field {
                artist.push_str(&text);
            } else {
                title.push_str(&text);
            }
        } else if let Some(Input::Search(s) | Input::Folder(s)) = &mut self.input {
            s.push_str(&text);
        }
    }
    pub(super) fn draw_imports(&self, frame: &mut Frame, area: Rect) {
        if !self.import_ui.enabled {
            return;
        }
        let Some(modal) = &self.import_ui.modal else {
            return;
        };
        let p = self.theme.palette();
        let width = area.width.saturating_sub(4).min(90);
        let height = area.height.saturating_sub(2).min(24);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let border = block(
            p,
            match modal {
                Modal::Jobs => " IMPORTS ",
                Modal::Preview { .. } => " IMPORT YOUTUBE PLAYLIST ",
                Modal::Edit { .. } => " EDIT TRACK ",
            },
            true,
        );
        let inner = border.inner(rect);
        frame.render_widget(border, rect);
        let mut lines = Vec::new();
        let hint = match modal {
            Modal::Preview { result, .. } => {
                if let Some(v) = result {
                    lines.push(
                        Line::from(v.title.clone())
                            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
                    );
                    lines.push(Line::from(format!(
                        "{} videos · {} already in library",
                        v.items.len(),
                        v.existing
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    )));
                    lines.push(Line::from(""));
                    for item in &v.items {
                        lines.push(Line::from(item.title.clone()));
                    }
                    "Enter add all · Esc cancel\nPgUp/Dn scroll"
                } else {
                    lines.push(Line::from("Looking up playlist…"));
                    "Esc cancel"
                }
            }
            Modal::Edit {
                title,
                artist,
                field,
                ..
            } => {
                lines.push(
                    Line::from(format!("{} Title", if !field { "›" } else { " " }))
                        .style(Style::default().fg(p.accent)),
                );
                lines.push(Line::from(title.clone()));
                lines.push(Line::from(""));
                lines.push(
                    Line::from(format!("{} Artist", if *field { "›" } else { " " }))
                        .style(Style::default().fg(p.accent)),
                );
                lines.push(Line::from(artist.clone()));
                "Tab field · Ctrl-U clear\nEnter save · Esc cancel"
            }
            Modal::Jobs => {
                if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected) {
                    lines.push(
                        Line::from(format!(
                            "Job {}/{} · {}",
                            self.import_ui.selected + 1,
                            self.import_ui.jobs.len(),
                            job.title
                        ))
                        .style(Style::default().fg(p.accent)),
                    );
                    lines.push(Line::from(job.summary()));
                    if let Some(e) = &job.error {
                        lines.push(Line::from(e.clone()).style(Style::default().fg(p.warning)));
                    }
                    if let Some(detail) = &self.import_ui.detail
                        && detail["job"]["job_id"].as_str() == Some(&job.job_id)
                        && let Some(items) = detail["items"].as_array()
                    {
                        for item in items {
                            lines.push(Line::from(format!(
                                "{} · {} {}",
                                item["index"].as_u64().unwrap_or(0) + 1,
                                item["status"].as_str().unwrap_or(""),
                                item["title"].as_str().unwrap_or("")
                            )));
                            if let Some(e) = item["error"].as_str() {
                                lines.push(
                                    Line::from(e.to_owned()).style(Style::default().fg(p.warning)),
                                );
                            }
                            if let Some(e) = item["metadata"]["warning"].as_str() {
                                lines.push(
                                    Line::from(e.to_owned()).style(Style::default().fg(p.warning)),
                                );
                            }
                        }
                    }
                } else {
                    lines.push(Line::from("No imports yet. Press a to add a YouTube URL."));
                }
                "j/k jobs · [/] items\nc cancel · r retry · Esc close\nPgUp/Dn scroll"
            }
        };
        let hint = Paragraph::new(hint)
            .style(Style::default().fg(p.muted).bg(p.panel))
            .wrap(Wrap { trim: false });
        let footer_height =
            (hint.line_count(inner.width) as u16).min(inner.height.saturating_sub(1));
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(footer_height),
        );
        let footer = Rect::new(inner.x, inner.y + body.height, inner.width, footer_height);
        let content = Paragraph::new(lines)
            .style(Style::default().fg(p.text).bg(p.panel))
            .wrap(Wrap { trim: false });
        let max_scroll = content
            .line_count(body.width)
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(
            content.scroll((self.import_ui.scroll.min(max_scroll), 0)),
            body,
        );
        frame.render_widget(hint, footer);
    }
}
