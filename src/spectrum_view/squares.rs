//! Whole-cell segments, sharing the dots ladder and the spectrum gradient.
use super::{BANDS, bar_layout, merged};
use crate::theme::{Palette, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect};

pub(super) fn draw(
    buf: &mut Buffer,
    body: Rect,
    p: &Palette,
    levels: &[f32; BANDS],
    peaks: &[f32; BANDS],
) {
    for bar in bar_layout(body.width) {
        let lit = (merged(levels, bar.bands.clone()) * f32::from(body.height)).round() as u16;
        let peak = (merged(peaks, bar.bands) * f32::from(body.height)).round() as u16;
        for x in bar.columns {
            for row in 0..body.height {
                let on = row < lit || peak.checked_sub(1) == Some(row);
                let color = if on {
                    spectrum_gradient(
                        p,
                        f32::from(row) / f32::from(body.height.saturating_sub(1).max(1)),
                    )
                } else {
                    p.selection
                };
                buf[(body.x + x, body.bottom() - 1 - row)]
                    .set_char(if on { '█' } else { '·' })
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
    fn segments_use_the_full_gradient_and_keep_the_held_peak() {
        let p = Theme::default().palette();
        let body = Rect::new(2, 3, 8, 5);
        let mut buf = Buffer::empty(body);
        draw(&mut buf, body, &p, &[0.4; BANDS], &[1.0; BANDS]);
        let x = body.x + bar_layout(body.width).next().unwrap().columns.start;
        assert_eq!(buf[(x, 7)].symbol(), "█");
        assert_eq!(buf[(x, 7)].fg, p.spectrum[0]);
        assert_eq!(buf[(x, 5)].symbol(), "·");
        assert_eq!(buf[(x, 3)].symbol(), "█");
        assert_eq!(buf[(x, 3)].fg, p.spectrum[2]);
    }
}
