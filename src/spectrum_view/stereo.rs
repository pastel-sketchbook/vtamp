//! Independent L/R envelopes, mirrored about the middle with readable channel labels.
use super::{BANDS, BLOCKS, SpectrumView, bar_layout, merged};
use crate::{spectrum::SpectrumChannels, theme::Palette};
use ratatui::{buffer::Buffer, layout::Rect};

#[derive(Default)]
pub(super) struct Stereo {
    left: [f32; BANDS],
    right: [f32; BANDS],
}
impl Stereo {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn is_active(&self) -> bool {
        self.left.iter().chain(&self.right).any(|v| *v > 0.0)
    }
    pub fn advance(&mut self, target: Option<&SpectrumChannels>, dt: f32) {
        for i in 0..BANDS {
            self.left[i] = target
                .map_or(0.0, |c| c.left[i].clamp(0.0, 1.0))
                .max(self.left[i] - dt * 1.8);
            self.right[i] = target
                .map_or(0.0, |c| c.right[i].clamp(0.0, 1.0))
                .max(self.right[i] - dt * 1.8);
        }
    }
    pub fn draw(&self, buf: &mut Buffer, body: Rect, p: &Palette) {
        let graph = graph(body);
        let height = graph.height & !1;
        let half = height / 2;
        let middle = graph.bottom() - half;
        buf[(body.x, middle - 1)]
            .set_char('L')
            .set_fg(p.muted)
            .set_bg(p.bg);
        buf[(body.x, middle)]
            .set_char('R')
            .set_fg(p.muted)
            .set_bg(p.bg);
        for bar in bar_layout(graph.width) {
            let left = merged(&self.left, bar.bands.clone()) * f32::from(half);
            let right = merged(&self.right, bar.bands) * f32::from(half);
            for x in bar.columns {
                for row in 0..half {
                    let color = SpectrumView::zone(p, row, half);
                    let upper = SpectrumView::units(left, row);
                    buf[(graph.x + x, middle - 1 - row)]
                        .set_char(BLOCKS[upper])
                        .set_fg(if upper == 0 { p.bg } else { color })
                        .set_bg(p.bg);
                    let lower = SpectrumView::units(right, row);
                    let cell = &mut buf[(graph.x + x, middle + row)];
                    match lower {
                        0 => cell.set_char(' ').set_fg(p.bg).set_bg(p.bg),
                        8 => cell.set_char('█').set_fg(color).set_bg(p.bg),
                        _ => cell.set_char(BLOCKS[8 - lower]).set_fg(p.bg).set_bg(color),
                    };
                }
            }
        }
    }
}

pub(super) fn fits(body: Rect) -> bool {
    body.width >= 10 && body.height >= 4
}
pub(super) fn graph(body: Rect) -> Rect {
    Rect::new(
        body.x + body.width.min(2),
        body.y,
        body.width.saturating_sub(2),
        body.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn labels_orientation_partial_blocks_and_channel_decay_are_independent() {
        let p = Theme::default().palette();
        let body = Rect::new(0, 0, 14, 8);
        let mut view = Stereo::default();
        let mut buf = Buffer::empty(body);
        view.advance(
            Some(&SpectrumChannels {
                left: [0.375; BANDS],
                right: [0.375; BANDS],
            }),
            0.0,
        );
        view.draw(&mut buf, body, &p);
        let x = graph(body).x + bar_layout(graph(body).width).next().unwrap().columns.start;
        assert_eq!(buf[(0, 3)].symbol(), "L");
        assert_eq!(buf[(0, 4)].symbol(), "R");
        assert_eq!(buf[(x, 2)].symbol(), "▄");
        assert_eq!(buf[(x, 5)].symbol(), "▄");
        assert_eq!(buf[(x, 2)].fg, buf[(x, 5)].bg);
        assert_eq!(buf[(x, 2)].bg, buf[(x, 5)].fg);
        view.advance(
            Some(&SpectrumChannels {
                left: [0.8; BANDS],
                right: [0.0; BANDS],
            }),
            0.3,
        );
        assert_eq!(view.right, [0.0; BANDS]);
        assert_eq!(view.left, [0.8; BANDS]);
        view.advance(None, 1.0);
        assert!(!view.is_active());
    }
}
