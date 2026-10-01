//! Terminal-only animation and drawing; no playback controls.
use crate::{
    spectrum::{BANDS, SpectrumFrame},
    theme::Palette,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use std::time::{Duration, Instant};

pub(crate) struct SpectrumView {
    pub enabled: bool,
    pub error: Option<String>,
    frame: Option<SpectrumFrame>,
    received: Instant,
    updated: Instant,
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    hold: [Instant; BANDS],
    redraw: bool,
}
impl SpectrumView {
    pub fn new(enabled: bool) -> Self {
        let now = Instant::now();
        Self {
            enabled,
            error: None,
            frame: None,
            received: now,
            updated: now,
            levels: [0.0; BANDS],
            peaks: [0.0; BANDS],
            hold: [now; BANDS],
            redraw: true,
        }
    }
    pub fn clear(&mut self) {
        self.frame = None;
        self.error = None;
        self.levels.fill(0.0);
        self.peaks.fill(0.0);
        self.redraw = true;
    }
    pub fn needs_animation(&self, playing: bool) -> bool {
        self.redraw
            || (self.error.is_none()
                && (self.levels.iter().chain(&self.peaks).any(|v| *v > 0.0)
                    || (playing
                        && self.received.elapsed() < Duration::from_millis(300)
                        && self
                            .frame
                            .as_ref()
                            .is_some_and(|f| f.active && f.levels.iter().any(|v| *v > 0.0)))))
    }
    pub fn accept(&mut self, frame: SpectrumFrame) {
        if self
            .frame
            .as_ref()
            .is_some_and(|f| f.generation > frame.generation)
        {
            return;
        }
        if self
            .frame
            .as_ref()
            .is_some_and(|f| f.generation != frame.generation || f.current_id != frame.current_id)
        {
            self.clear();
        }
        self.redraw |= self.error.is_some();
        self.frame = Some(frame);
        self.error = None;
        self.received = Instant::now();
    }
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        p: Palette,
        bordered: bool,
        playing: bool,
    ) {
        self.redraw = false;
        let inner = if bordered {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Line::styled(
                    " SPECTRUM · v close ",
                    Style::default().fg(p.muted),
                ))
                .border_style(Style::default().fg(p.border));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            inner
        } else {
            frame.render_widget(
                Paragraph::new("SPECTRUM").style(Style::default().fg(p.muted)),
                Rect {
                    height: area.height.min(1),
                    ..area
                },
            );
            Rect {
                y: area.y.saturating_add(1),
                height: area.height.saturating_sub(1),
                ..area
            }
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(p.muted))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                inner,
            );
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.updated).as_secs_f32().min(0.2);
        self.updated = now;
        let live = playing && self.received.elapsed() < Duration::from_millis(300);
        for i in 0..BANDS {
            let target = self
                .frame
                .as_ref()
                .filter(|f| live && f.active)
                .map_or(0.0, |f| f.levels[i].clamp(0.0, 1.0));
            self.levels[i] = target.max(self.levels[i] - dt * 1.8);
            if self.levels[i] >= self.peaks[i] {
                self.peaks[i] = self.levels[i];
                self.hold[i] = now + Duration::from_millis(180);
            } else if now >= self.hold[i] {
                self.peaks[i] = self.levels[i].max(self.peaks[i] - dt * 0.8);
            }
        }
        let height = inner.height.saturating_sub(1);
        let count = BANDS.min((inner.width as usize).div_ceil(2));
        let step = (inner.width as usize + 1) / count;
        let width = step.saturating_sub(1).max(1);
        let offset = (inner.width as usize - (count * step - (step - width))) / 2;
        let blocks = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let buf = frame.buffer_mut();
        for bar in 0..count {
            let start = bar * BANDS / count;
            let end = ((bar + 1) * BANDS / count).max(start + 1);
            let level = self.levels[start..end].iter().copied().fold(0.0, f32::max) * height as f32;
            let peak = self.peaks[start..end].iter().copied().fold(0.0, f32::max) * height as f32;
            for row in 0..height {
                let units = ((level - row as f32) * 8.0).ceil().clamp(0.0, 8.0) as usize;
                let color = p.spectrum[if row as f32 / (height.max(1) as f32) < 0.55 {
                    0
                } else if row as f32 / (height.max(1) as f32) < 0.8 {
                    1
                } else {
                    2
                }];
                let glyph =
                    if units == 0 && peak > 0.05 && row == (peak.ceil() as u16).saturating_sub(1) {
                        '▔'
                    } else {
                        blocks[units]
                    };
                for col in 0..width {
                    buf[(
                        inner.x + (offset + bar * step + col) as u16,
                        inner.y + height - 1 - row,
                    )]
                        .set_char(glyph)
                        .set_fg(color)
                        .set_bg(p.bg);
                }
            }
        }
        frame.render_widget(
            Paragraph::new("LOW").style(Style::default().fg(p.muted)),
            Rect::new(inner.x, inner.y + height, inner.width.min(3), 1),
        );
        if inner.width >= 9 {
            frame.render_widget(
                Paragraph::new("HIGH").style(Style::default().fg(p.muted)),
                Rect::new(inner.right() - 4, inner.y + height, 4, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn animation_sleeps_after_decay_and_wakes_for_new_audio() {
        let mut view = SpectrumView::new(true);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        let mut draw = |view: &mut SpectrumView, playing| {
            terminal
                .draw(|f| view.draw(f, f.area(), Theme::default().palette(), true, playing))
                .unwrap();
        };
        draw(&mut view, false);
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame {
            active: true,
            levels: [1.0; BANDS],
            ..SpectrumFrame::default()
        });
        assert!(view.needs_animation(true));
        draw(&mut view, true);
        assert!(view.needs_animation(false), "pause must let peaks fall");
        for _ in 0..10 {
            view.updated = Instant::now() - Duration::from_millis(200);
            view.hold.fill(Instant::now() - Duration::from_secs(1));
            draw(&mut view, false);
        }
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame::default());
        assert!(!view.needs_animation(true), "silent frames stay idle");
        view.accept(SpectrumFrame {
            active: true,
            levels: [0.5; BANDS],
            ..SpectrumFrame::default()
        });
        assert!(view.needs_animation(true), "resume wakes animation");
        view.received = Instant::now() - Duration::from_secs(1);
        assert!(!view.needs_animation(true), "stale audio cannot wake it");
        view.clear();
        view.error = Some("Disconnected".into());
        draw(&mut view, false);
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame::default());
        assert!(
            view.needs_animation(false),
            "recovery clears a visible error"
        );
    }

    #[test]
    fn colors_peaks_staleness_and_generation_follow_actual_frames() {
        let mut view = SpectrumView::new(true);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        let draw =
            |view: &mut SpectrumView,
             terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
                terminal
                    .draw(|f| view.draw(f, f.area(), Theme::default().palette(), true, true))
                    .unwrap();
            };
        view.accept(SpectrumFrame {
            generation: 1,
            active: true,
            levels: [1.0; BANDS],
            ..SpectrumFrame::default()
        });
        draw(&mut view, &mut terminal);
        for color in Theme::default().palette().spectrum {
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|c| c.fg == color && c.symbol() == "█")
            );
        }
        // An odd inner width must still leave one empty column between bars.
        terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(61, 12)).unwrap();
        draw(&mut view, &mut terminal);
        for x in 1..60 {
            assert_eq!(
                terminal.backend().buffer()[(x, 9)].symbol(),
                if x % 2 == 1 { "█" } else { " " }
            );
        }
        view.received = Instant::now() - Duration::from_secs(1);
        for _ in 0..10 {
            view.updated = Instant::now() - Duration::from_millis(200);
            view.hold.fill(Instant::now() - Duration::from_secs(1));
            draw(&mut view, &mut terminal);
        }
        assert_eq!(view.levels, [0.0; BANDS]);
        assert_eq!(view.peaks, [0.0; BANDS]);
        view.accept(SpectrumFrame {
            generation: 2,
            ..SpectrumFrame::default()
        });
        view.accept(SpectrumFrame {
            generation: 1,
            active: true,
            levels: [1.0; BANDS],
            ..SpectrumFrame::default()
        });
        draw(&mut view, &mut terminal);
        assert_eq!(view.levels, [0.0; BANDS]);
    }
}
