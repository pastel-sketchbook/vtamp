//! Terminal-only animation and drawing; no playback controls.
use crate::{
    settings::SpectrumStyle,
    spectrum::{BANDS, SpectrumFrame},
    theme::{Palette, spectrum_gradient},
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Rows the waterfall remembers; more than any pane shows, bounded for memory.
const HISTORY: usize = 256;

pub(crate) struct SpectrumView {
    pub enabled: bool,
    pub error: Option<String>,
    style: SpectrumStyle,
    frame: Option<SpectrumFrame>,
    received: Instant,
    updated: Instant,
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    hold: [Instant; BANDS],
    /// Raw levels of recent active frames, oldest first; the waterfall draws these.
    history: VecDeque<[f32; BANDS]>,
    redraw: bool,
}
impl SpectrumView {
    pub fn new(enabled: bool, style: SpectrumStyle) -> Self {
        let now = Instant::now();
        Self {
            enabled,
            error: None,
            style,
            frame: None,
            received: now,
            updated: now,
            levels: [0.0; BANDS],
            peaks: [0.0; BANDS],
            hold: [now; BANDS],
            history: VecDeque::with_capacity(HISTORY),
            redraw: true,
        }
    }
    pub fn style(&self) -> SpectrumStyle {
        self.style
    }
    /// Switches the rendering only; levels, peaks, and history carry over.
    pub fn set_style(&mut self, style: SpectrumStyle) {
        self.style = style;
        self.redraw = true;
    }
    pub fn clear(&mut self) {
        self.frame = None;
        self.error = None;
        self.history.clear();
        self.reset_levels();
    }
    /// Drops the bar state for a new logical stream (seek, pause, resume) while
    /// keeping the waterfall's past, which was genuinely heard.
    fn reset_levels(&mut self) {
        self.levels.fill(0.0);
        self.peaks.fill(0.0);
        self.redraw = true;
    }
    pub fn needs_animation(&self, playing: bool) -> bool {
        if self.style == SpectrumStyle::Waterfall {
            // Rows only appear with frames; nothing moves between them.
            return self.redraw;
        }
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
        let (new_track, new_generation) = self.frame.as_ref().map_or((false, false), |current| {
            (
                current.current_id != frame.current_id,
                current.generation != frame.generation,
            )
        });
        if new_track {
            self.clear();
        } else if new_generation {
            self.reset_levels();
        }
        self.redraw |= self.error.is_some();
        if frame.active {
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(frame.levels);
            self.redraw |= self.style == SpectrumStyle::Waterfall;
        }
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
        let inner = self.header(frame, area, &p, bordered);
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
        let body = Rect {
            height: inner.height.saturating_sub(1),
            ..inner
        };
        if self.style == SpectrumStyle::Waterfall {
            self.draw_waterfall(frame.buffer_mut(), body, &p);
        } else {
            self.advance(playing);
            self.draw_bars(frame.buffer_mut(), body, &p);
        }
        frame.render_widget(
            Paragraph::new("LOW").style(Style::default().fg(p.muted)),
            Rect::new(inner.x, inner.y + body.height, inner.width.min(3), 1),
        );
        if inner.width >= 9 {
            frame.render_widget(
                Paragraph::new("HIGH").style(Style::default().fg(p.muted)),
                Rect::new(inner.right() - 4, inner.y + body.height, 4, 1),
            );
        }
    }

    fn header(&self, frame: &mut Frame, area: Rect, p: &Palette, bordered: bool) -> Rect {
        let name = self.style.id();
        if bordered {
            let full = format!(" SPECTRUM · {name} · v close · V style ");
            let title =
                if Line::from(full.as_str()).width() <= usize::from(area.width).saturating_sub(2) {
                    full
                } else {
                    " SPECTRUM · v close ".to_string()
                };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Line::styled(title, Style::default().fg(p.muted)))
                .border_style(Style::default().fg(p.border));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            inner
        } else {
            frame.render_widget(
                Paragraph::new(format!("SPECTRUM · {name}")).style(Style::default().fg(p.muted)),
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
        }
    }

    /// Bar decay and peak hold, shared by every style with falling peaks.
    fn advance(&mut self, playing: bool) {
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
    }

    fn draw_bars(&self, buf: &mut Buffer, body: Rect, p: &Palette) {
        let height = body.height;
        let count = BANDS.min((body.width as usize).div_ceil(2));
        let step = (body.width as usize + 1) / count;
        let width = step.saturating_sub(1).max(1);
        let offset = (body.width as usize - (count * step - (step - width))) / 2;
        let rows = if self.style == SpectrumStyle::Mirror {
            height & !1
        } else {
            height
        };
        let half = rows / 2;
        let scale = f32::from(if self.style == SpectrumStyle::Mirror {
            half
        } else {
            rows
        });
        let bottom = body.y + height;
        for bar in 0..count {
            let start = bar * BANDS / count;
            let end = ((bar + 1) * BANDS / count).max(start + 1);
            let level = self.levels[start..end].iter().copied().fold(0.0, f32::max) * scale;
            let peak = self.peaks[start..end].iter().copied().fold(0.0, f32::max) * scale;
            for col in 0..width {
                let x = body.x + (offset + bar * step + col) as u16;
                match self.style {
                    SpectrumStyle::Bars | SpectrumStyle::Gradient | SpectrumStyle::Mono => {
                        for row in 0..rows {
                            let (glyph, color) = self.bar_cell(p, row, rows, level, peak);
                            buf[(x, bottom - 1 - row)]
                                .set_char(glyph)
                                .set_fg(color)
                                .set_bg(p.bg);
                        }
                    }
                    SpectrumStyle::Mirror => {
                        for row in 0..half {
                            let (glyph, color) = self.bar_cell(p, row, half, level, peak);
                            buf[(x, bottom - half - 1 - row)]
                                .set_char(glyph)
                                .set_fg(color)
                                .set_bg(p.bg);
                            let cell = &mut buf[(x, bottom - half + row)];
                            match (glyph, Self::units(level, row)) {
                                (' ', _) => cell.set_char(' ').set_fg(p.bg).set_bg(p.bg),
                                ('▔', _) => cell.set_char('▁').set_fg(color).set_bg(p.bg),
                                (_, 8) => cell.set_char('█').set_fg(color).set_bg(p.bg),
                                (_, units) => {
                                    cell.set_char(BLOCKS[8 - units]).set_fg(p.bg).set_bg(color)
                                }
                            };
                        }
                        if rows < height {
                            buf[(x, body.y)].set_char(' ').set_fg(p.bg).set_bg(p.bg);
                        }
                    }
                    SpectrumStyle::Dots => {
                        let lit = level.round() as u16;
                        let peak_row = (peak.round() as u16).checked_sub(1);
                        for row in 0..rows {
                            let cell = &mut buf[(x, bottom - 1 - row)];
                            if row < lit || peak_row == Some(row) {
                                cell.set_char('●').set_fg(Self::zone(p, row, rows));
                            } else {
                                cell.set_char('·').set_fg(p.selection);
                            }
                            cell.set_bg(p.bg);
                        }
                    }
                    SpectrumStyle::Waterfall => unreachable!("drawn by draw_waterfall"),
                }
            }
        }
    }

    /// Eighths of the cell at `row` covered by a bar of `level` rows.
    fn units(level: f32, row: u16) -> usize {
        ((level - row as f32) * 8.0).ceil().clamp(0.0, 8.0) as usize
    }

    fn zone(p: &Palette, row: u16, rows: u16) -> Color {
        let position = row as f32 / rows.max(1) as f32;
        p.spectrum[if position < 0.55 {
            0
        } else if position < 0.8 {
            1
        } else {
            2
        }]
    }

    /// Glyph and color of one cell in a vertical bar, including the peak marker.
    fn bar_cell(&self, p: &Palette, row: u16, rows: u16, level: f32, peak: f32) -> (char, Color) {
        let units = Self::units(level, row);
        let glyph = if units == 0 && peak > 0.05 && row == (peak.ceil() as u16).saturating_sub(1) {
            '▔'
        } else {
            BLOCKS[units]
        };
        let color = match self.style {
            SpectrumStyle::Gradient => {
                spectrum_gradient(p, row as f32 / rows.saturating_sub(1).max(1) as f32)
            }
            SpectrumStyle::Mono if glyph == '▔' => p.text,
            SpectrumStyle::Mono => p.accent,
            _ => Self::zone(p, row, rows),
        };
        (glyph, color)
    }

    fn draw_waterfall(&self, buf: &mut Buffer, body: Rect, p: &Palette) {
        let width = usize::from(body.width);
        if width == 0 {
            return;
        }
        let bottom = body.y + body.height;
        for row in 0..body.height {
            let y = bottom - 1 - row;
            let levels = self
                .history
                .len()
                .checked_sub(1 + usize::from(row))
                .map(|index| self.history[index]);
            for column in 0..width {
                // Every column shows a band, merged in narrow panes and repeated
                // in wide ones, so the history fills the width without gaps.
                let x = body.x + column as u16;
                let start = column * BANDS / width;
                let end = ((column + 1) * BANDS / width).max(start + 1);
                let level = levels
                    .map_or(0.0, |levels| {
                        levels[start..end].iter().copied().fold(0.0, f32::max)
                    })
                    .clamp(0.0, 1.0);
                let cell = &mut buf[(x, y)];
                if level <= 0.0 {
                    cell.set_char(' ').set_fg(p.bg).set_bg(p.bg);
                } else {
                    let glyph = if level < 0.25 {
                        '░'
                    } else if level < 0.5 {
                        '▒'
                    } else if level < 0.75 {
                        '▓'
                    } else {
                        '█'
                    };
                    cell.set_char(glyph)
                        .set_fg(spectrum_gradient(p, level))
                        .set_bg(p.bg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};

    fn styled(style: SpectrumStyle) -> SpectrumView {
        SpectrumView::new(true, style)
    }
    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }
    fn render(view: &mut SpectrumView, terminal: &mut Terminal<TestBackend>, playing: bool) {
        terminal
            .draw(|f| view.draw(f, f.area(), Theme::default().palette(), true, playing))
            .unwrap();
    }
    fn active(levels: [f32; BANDS]) -> SpectrumFrame {
        SpectrumFrame {
            active: true,
            levels,
            ..SpectrumFrame::default()
        }
    }
    fn top_row(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect()
    }
    /// Lets the next draw see 200 ms of decay with an expired peak hold.
    fn age(view: &mut SpectrumView) {
        view.updated = Instant::now() - Duration::from_millis(200);
        view.hold.fill(Instant::now() - Duration::from_secs(1));
    }

    #[test]
    fn animation_sleeps_after_decay_and_wakes_for_new_audio() {
        let mut view = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, false);
        assert!(!view.needs_animation(false));
        view.accept(active([1.0; BANDS]));
        assert!(view.needs_animation(true));
        render(&mut view, &mut terminal, true);
        assert!(view.needs_animation(false), "pause must let peaks fall");
        for _ in 0..10 {
            age(&mut view);
            render(&mut view, &mut terminal, false);
        }
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame::default());
        assert!(!view.needs_animation(true), "silent frames stay idle");
        view.accept(active([0.5; BANDS]));
        assert!(view.needs_animation(true), "resume wakes animation");
        view.received = Instant::now() - Duration::from_secs(1);
        assert!(!view.needs_animation(true), "stale audio cannot wake it");
        view.clear();
        view.error = Some("Disconnected".into());
        render(&mut view, &mut terminal, false);
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame::default());
        assert!(
            view.needs_animation(false),
            "recovery clears a visible error"
        );
    }

    #[test]
    fn colors_peaks_staleness_and_generation_follow_actual_frames() {
        let mut view = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        view.accept(SpectrumFrame {
            generation: 1,
            ..active([1.0; BANDS])
        });
        render(&mut view, &mut terminal, true);
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
        terminal = backend(61, 12);
        render(&mut view, &mut terminal, true);
        for x in 1..60 {
            assert_eq!(
                terminal.backend().buffer()[(x, 9)].symbol(),
                if x % 2 == 1 { "█" } else { " " }
            );
        }
        view.received = Instant::now() - Duration::from_secs(1);
        for _ in 0..10 {
            age(&mut view);
            render(&mut view, &mut terminal, true);
        }
        assert_eq!(view.levels, [0.0; BANDS]);
        assert_eq!(view.peaks, [0.0; BANDS]);
        view.accept(SpectrumFrame {
            generation: 2,
            ..SpectrumFrame::default()
        });
        view.accept(SpectrumFrame {
            generation: 1,
            ..active([1.0; BANDS])
        });
        render(&mut view, &mut terminal, true);
        assert_eq!(view.levels, [0.0; BANDS]);
    }

    #[test]
    fn titles_name_the_style_when_they_fit() {
        let mut view = styled(SpectrumStyle::Gradient);
        let mut terminal = backend(44, 12);
        render(&mut view, &mut terminal, false);
        assert!(top_row(&terminal).contains("SPECTRUM · gradient · v close · V style"));
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, false);
        let top = top_row(&terminal);
        assert!(top.contains("SPECTRUM · v close"), "{top}");
        assert!(!top.contains("gradient"), "{top}");
        terminal
            .draw(|f| view.draw(f, f.area(), Theme::default().palette(), false, false))
            .unwrap();
        assert!(top_row(&terminal).starts_with("SPECTRUM · gradient"));
        let mut terminal = backend(1, 1);
        render(&mut view, &mut terminal, false);
    }

    #[test]
    fn gradient_runs_between_the_theme_roles() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Gradient);
        let mut terminal = backend(40, 12);
        view.accept(active([1.0; BANDS]));
        render(&mut view, &mut terminal, true);
        // Body rows are y = 1..=9 (title row 0, axis row 10, border row 11).
        let buffer = terminal.backend().buffer();
        let column: Vec<Color> = (1..=9).map(|y| buffer[(1, y)].fg).collect();
        assert!((1..=9).all(|y| buffer[(1, y)].symbol() == "█"));
        assert_eq!(column[8], p.spectrum[0], "bottom row is the low role");
        assert_eq!(column[0], p.spectrum[2], "top row is the high role");
        assert!(
            column.iter().any(|c| !p.spectrum.contains(c)),
            "intermediate rows blend the roles"
        );
        let mut distinct = column.clone();
        distinct.dedup();
        assert_eq!(distinct.len(), column.len(), "every row has its own color");
    }

    #[test]
    fn mono_uses_accent_and_marks_peaks_with_text() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Mono);
        let mut terminal = backend(40, 12);
        view.accept(active([0.5; BANDS]));
        render(&mut view, &mut terminal, true);
        let is_bar = |c: &ratatui::buffer::Cell| {
            c.symbol() != " " && c.symbol().chars().all(|ch| BLOCKS.contains(&ch))
        };
        let buffer = terminal.backend().buffer();
        let bars = buffer.content().iter().filter(|c| is_bar(c)).count();
        assert!(bars > 0);
        assert!(
            buffer
                .content()
                .iter()
                .filter(|c| is_bar(c))
                .all(|c| c.fg == p.accent)
        );
        assert!(!buffer.content().iter().any(|c| c.symbol() == "▔"));
        // Let the level fall below the held peak so the marker appears.
        view.accept(active([0.1; BANDS]));
        view.updated = Instant::now() - Duration::from_millis(200);
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        let markers: Vec<Color> = buffer
            .content()
            .iter()
            .filter(|c| c.symbol() == "▔")
            .map(|c| c.fg)
            .collect();
        assert!(!markers.is_empty());
        assert!(
            markers.iter().all(|c| *c == p.text),
            "peak marker uses the text role"
        );
        assert!(
            buffer
                .content()
                .iter()
                .filter(|c| is_bar(c))
                .all(|c| c.fg == p.accent)
        );
    }

    #[test]
    fn mirror_is_symmetric_and_paints_partial_lower_cells_inverted() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Mirror);
        // Body rows y = 1..=9; eight are used (2..=9), half = 4; level 0.6 → 2.4 rows.
        let mut terminal = backend(40, 12);
        view.accept(active([0.6; BANDS]));
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].symbol(), " ", "odd top row stays blank");
        for y in [4, 5, 6, 7] {
            assert_eq!(buffer[(1, y)].symbol(), "█", "row {y}");
            assert_eq!(buffer[(1, y)].bg, p.bg, "row {y}");
        }
        let upper = &buffer[(1, 3)];
        let lower = &buffer[(1, 8)];
        assert_eq!(upper.symbol(), "▄", "0.4 of a row is 4 eighths");
        assert_eq!(upper.bg, p.bg);
        assert_eq!(lower.symbol(), "▄", "inverse trick: 8 - 4 eighths");
        assert_eq!(lower.fg, p.bg, "inverse trick paints the canvas color");
        assert_eq!(
            lower.bg, upper.fg,
            "inverse trick fills with the zone color"
        );
        assert_eq!(buffer[(1, 2)].symbol(), " ");
        assert_eq!(buffer[(1, 9)].symbol(), " ");
        assert_eq!(buffer[(1, 9)].fg, p.bg);
        // Let the level drop so peak markers appear on both halves.
        view.accept(active([0.05; BANDS]));
        view.updated = Instant::now() - Duration::from_millis(200);
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        let above = (2..=5).find(|y| buffer[(1, *y)].symbol() == "▔");
        let below = (6..=9).find(|y| buffer[(1, *y)].symbol() == "▁");
        assert!(above.is_some() && below.is_some(), "{above:?} {below:?}");
        assert_eq!(
            above.unwrap() + below.unwrap(),
            11,
            "markers mirror each other"
        );
        assert_eq!(
            buffer[(1, below.unwrap())].fg,
            buffer[(1, above.unwrap())].fg
        );
        // A single body row cannot host a mirror; nothing panics and nothing draws.
        let mut terminal = backend(40, 4);
        view.accept(active([1.0; BANDS]));
        render(&mut view, &mut terminal, true);
        assert_eq!(terminal.backend().buffer()[(1, 1)].symbol(), " ");
    }

    #[test]
    fn dots_light_whole_segments_and_hold_a_peak_dot() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Dots);
        let mut terminal = backend(40, 12);
        view.accept(active([0.5; BANDS]));
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        // Body rows y = 1..=9; 0.5 × 9 = 4.5 rounds to 5 lit segments from the bottom.
        let lit: Vec<u16> = (1..=9)
            .filter(|y| buffer[(1, *y)].symbol() == "●")
            .collect();
        assert_eq!(lit, vec![5, 6, 7, 8, 9]);
        assert!(
            (1..=4).all(|y| buffer[(1, y)].symbol() == "·" && buffer[(1, y)].fg == p.selection)
        );
        assert_eq!(buffer[(1, 9)].fg, p.spectrum[0]);
        view.accept(active([0.1; BANDS]));
        view.updated = Instant::now() - Duration::from_millis(200);
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        let lit: Vec<u16> = (1..=9)
            .filter(|y| buffer[(1, *y)].symbol() == "●")
            .collect();
        assert_eq!(
            lit.len(),
            2,
            "one lit segment plus a held peak dot: {lit:?}"
        );
        assert_eq!(lit[1], 9);
        assert!(lit[0] < 8, "the peak dot floats above the lit segment");
    }

    #[test]
    fn waterfall_scrolls_per_frame_freezes_without_frames_and_survives_generations() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Waterfall);
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, true);
        assert!(!view.needs_animation(true), "an empty waterfall is idle");
        view.accept(active([1.0; BANDS]));
        assert!(view.needs_animation(true), "a frame requests one draw");
        render(&mut view, &mut terminal, true);
        assert!(
            !view.needs_animation(true),
            "nothing moves until the next frame"
        );
        let buffer = terminal.backend().buffer();
        // Body rows y = 1..=9, newest at the bottom; 32 bands fill all 38 columns.
        assert!((1..=38).all(|x| buffer[(x, 9)].symbol() == "█"));
        assert_eq!(buffer[(4, 9)].fg, p.spectrum[2]);
        assert_eq!(buffer[(4, 8)].symbol(), " ");
        view.accept(active([0.3; BANDS]));
        view.accept(active([0.0; BANDS]));
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(4, 9)].symbol(), " ", "silence is a blank row");
        assert_eq!(buffer[(4, 9)].fg, p.bg, "blank cells keep a constant fg");
        assert_eq!(buffer[(4, 8)].symbol(), "▒");
        assert_eq!(buffer[(4, 7)].symbol(), "█");
        assert_eq!(view.history.len(), 3);
        for _ in 0..5 {
            age(&mut view);
            render(&mut view, &mut terminal, false);
        }
        assert_eq!(
            terminal.backend().buffer()[(4, 7)].symbol(),
            "█",
            "pause freezes the history"
        );
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame {
            generation: 1,
            ..SpectrumFrame::default()
        });
        assert_eq!(view.history.len(), 3, "a new generation keeps the past");
        view.accept(SpectrumFrame {
            generation: 1,
            current_id: Some("next".into()),
            ..SpectrumFrame::default()
        });
        assert!(
            view.history.is_empty(),
            "a new track starts a fresh history"
        );
        view.accept(SpectrumFrame {
            generation: 1,
            current_id: Some("next".into()),
            ..active([0.9; BANDS])
        });
        assert_eq!(view.history.len(), 1);
        view.clear();
        assert!(view.history.is_empty());
        for _ in 0..(HISTORY + 10) {
            view.accept(active([0.2; BANDS]));
        }
        assert_eq!(view.history.len(), HISTORY);
        // Narrow panes merge bands instead of clipping them.
        let mut terminal = backend(20, 12);
        render(&mut view, &mut terminal, true);
        let row: Vec<String> = (1..19)
            .map(|x| terminal.backend().buffer()[(x, 9)].symbol().to_string())
            .collect();
        assert!(row.iter().all(|s| s == "░"), "{row:?}");
    }

    #[test]
    fn switching_styles_keeps_levels_and_history() {
        let mut view = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        view.accept(active([0.8; BANDS]));
        render(&mut view, &mut terminal, true);
        assert!(view.levels.iter().all(|v| *v > 0.0));
        view.set_style(SpectrumStyle::Waterfall);
        assert_eq!(view.style(), SpectrumStyle::Waterfall);
        assert!(view.needs_animation(false), "a style change redraws once");
        assert_eq!(view.history.len(), 1, "history was collected while in bars");
        assert!(view.levels.iter().all(|v| *v > 0.0));
        render(&mut view, &mut terminal, true);
        view.set_style(SpectrumStyle::Dots);
        assert_eq!(view.history.len(), 1);
        assert!(view.levels.iter().all(|v| *v > 0.0));
    }
}
