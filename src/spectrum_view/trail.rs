//! The six most recent frame tops; old frames fade and new frames win collisions.
use super::{BANDS, SpectrumView, bar_layout, merged};
use crate::theme::{Palette, blend};
use ratatui::{buffer::Buffer, layout::Rect};
use std::collections::VecDeque;

const LENGTH: usize = 6;

pub(super) fn draw(buf: &mut Buffer, body: Rect, p: &Palette, history: &VecDeque<[f32; BANDS]>) {
    if body.height == 0 {
        return;
    }
    for (age, levels) in history.iter().rev().take(LENGTH).enumerate().rev() {
        for bar in bar_layout(body.width) {
            let level = merged(levels, bar.bands);
            if level <= 0.0 {
                continue;
            }
            let row = ((level * f32::from(body.height)).ceil() as u16)
                .saturating_sub(1)
                .min(body.height - 1);
            let color = blend(
                SpectrumView::zone(p, row, body.height),
                p.bg,
                age as f32 / LENGTH as f32,
            );
            for x in bar.columns {
                buf[(body.x + x, body.bottom() - 1 - row)]
                    .set_char('▄')
                    .set_fg(color)
                    .set_bg(p.bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn new_tops_win_and_older_tops_fade_without_losing_high_bands() {
        let p = Theme::default().palette();
        let body = Rect::new(0, 0, 12, 10);
        let mut buf = Buffer::empty(body);
        let history = VecDeque::from([[0.2; BANDS], [0.6; BANDS], [0.8; BANDS], [0.8; BANDS]]);
        draw(&mut buf, body, &p, &history);
        let x = bar_layout(body.width).last().unwrap().columns.start;
        assert_eq!(buf[(x, 2)].fg, SpectrumView::zone(&p, 7, 10));
        assert_eq!(
            buf[(x, 4)].fg,
            blend(SpectrumView::zone(&p, 5, 10), p.bg, 2.0 / 6.0)
        );
        assert_eq!(
            buf[(x, 8)].fg,
            blend(SpectrumView::zone(&p, 1, 10), p.bg, 3.0 / 6.0)
        );
        let mut silent = Buffer::empty(body);
        draw(&mut silent, body, &p, &VecDeque::from([[0.0; BANDS]]));
        assert!(silent.content().iter().all(|c| c.symbol() == " "));
    }
}
