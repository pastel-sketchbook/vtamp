//! A filled, interpolated curve on the shared braille canvas.
use super::{BANDS, SpectrumView, braille::Braille, sample_at};
use crate::theme::Palette;
use ratatui::{buffer::Buffer, layout::Rect};

pub(super) fn fits(body: Rect) -> bool {
    body.width >= 4 && body.height >= 2
}

pub(super) fn draw(buf: &mut Buffer, body: Rect, p: &Palette, levels: &[f32; BANDS]) {
    let mut canvas = Braille::new(body);
    let (columns, rows) = canvas.size();
    for x in 0..columns {
        let dots = (sample_at(levels, x as usize, columns as usize) * rows as f32).ceil() as i32;
        for height in 0..dots.min(rows) {
            canvas.dot(
                x,
                rows - 1 - height,
                SpectrumView::zone(p, (height / 4) as u16, body.height),
                0,
            );
        }
    }
    canvas.render(buf, p);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{spectrum_view::braille::dots_of, theme::Theme};

    #[test]
    fn curve_fills_upwards_uses_height_zones_and_keeps_the_treble() {
        let p = Theme::default().palette();
        let body = Rect::new(0, 0, 32, 8);
        let mut buf = Buffer::empty(body);
        let ramp = std::array::from_fn(|i| i as f32 / (BANDS - 1) as f32);
        draw(&mut buf, body, &p, &ramp);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(dots_of(buf[(31, 0)].symbol()).len(), 8);
        assert_eq!(buf[(31, 0)].fg, p.spectrum[2]);
        assert_eq!(buf[(31, 7)].fg, p.spectrum[0]);
        draw(&mut buf, body, &p, &[0.0; BANDS]);
        assert!(buf.content().iter().all(|cell| cell.symbol() == " "));
    }
}
