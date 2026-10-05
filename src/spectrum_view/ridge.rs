//! Ridgelines of recent frames on braille dots, nearest at the bottom, like the stacked
//! pulsar plot on Joy Division's "Unknown Pleasures". Straight segments join neighboring
//! band values, and nearer lines hide whatever lies behind them (a floating horizon).
use super::{BANDS, braille::Braille, sample_at};
use crate::theme::{Palette, blend, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect};
use std::collections::VecDeque;

/// Gradient shades a line can take; coarser shades resend fewer cells.
const SHADES: f32 = 16.0;
/// Fade toward the canvas per quarter of depth, so the farthest lines keep a readable
/// contrast even on the light theme.
const FADE: f32 = 0.15;

/// Draws the history, newest frame last; `pushed` counts every frame ever added, so a
/// kept frame keeps its place while old frames drop off the front.
pub(super) fn draw(
    buf: &mut Buffer,
    body: Rect,
    p: &Palette,
    history: &VecDeque<[f32; BANDS]>,
    pushed: u64,
) {
    let mut canvas = Braille::new(body);
    let (columns, rows) = canvas.size();
    if columns > 0 && rows > 0 {
        // One or two terminal rows only fit the newest line, as a sparkline.
        let single = body.height <= 2;
        let spacing = (rows as f32 / 14.0).round().clamp(3.0, 6.0) as u64;
        // Short bodies scroll a dot every second frame so their few lines stay readable.
        let frames_per_dot = if rows < 48 { 2 } else { 1 };
        let amplitude = if single {
            (rows - 1) as f32
        } else {
            (0.4 * rows as f32).min(7.0 * spacing as f32)
        };
        let width = columns as usize;
        let mut horizon = vec![rows; width];
        let mut profile = vec![0.0; width];
        let mut tops = vec![0; width];
        let mut highs = vec![0; width];
        for (age, levels) in history.iter().rev().enumerate() {
            let depth = age as i32 / frames_per_dot as i32;
            let index = pushed.saturating_sub(1 + age as u64);
            let kept = index.is_multiple_of(spacing * frames_per_dot);
            // Only the newest line sits on the bottom row; older ones wait for their dot.
            if age > 0 && (single || !kept || depth == 0) {
                continue;
            }
            let baseline = rows - 1 - depth;
            if baseline < 0 {
                break;
            }
            let fade = FADE * (4 * depth / rows).min(3) as f32;
            for x in 0..width {
                profile[x] = sample_at(levels, x, width);
                tops[x] = baseline - (profile[x] * amplitude).round() as i32;
            }
            for x in 0..width {
                // Reach halfway to each neighbor, so steep segments stay connected.
                let y = tops[x];
                let (mut high, mut low) = (y, y);
                for neighbor in [x.checked_sub(1), Some(x + 1).filter(|n| *n < width)]
                    .into_iter()
                    .flatten()
                {
                    let middle = (y + tops[neighbor]).div_euclid(2);
                    high = high.min(middle);
                    low = low.max(middle);
                }
                let shade = (profile[x] * SHADES).round() / SHADES;
                let color = blend(spectrum_gradient(p, shade), p.bg, fade);
                // Nearer lines win shared cells; within a line, the louder point does.
                let rank = ((age as u32) << 8) | (255 - (shade * 255.0) as u32);
                for dot in high..=low.min(horizon[x] - 1) {
                    canvas.dot(x as i32, dot, color, rank);
                }
                highs[x] = high;
            }
            for (limit, high) in horizon.iter_mut().zip(&highs) {
                *limit = (*limit).min(*high);
            }
        }
    }
    canvas.render(buf, p);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{spectrum_view::braille::dots_of, theme::Theme};

    fn render(width: u16, height: u16, history: &[[f32; BANDS]]) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        draw(
            &mut buf,
            area,
            &Theme::default().palette(),
            &history.iter().copied().collect(),
            history.len() as u64,
        );
        buf
    }

    fn lit(buf: &Buffer) -> Vec<(i32, i32)> {
        let mut dots = Vec::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                for (dx, dy) in dots_of(buf[(x, y)].symbol()) {
                    dots.push((i32::from(x * 2 + dx), i32::from(y * 4 + dy)));
                }
            }
        }
        dots
    }

    #[test]
    fn straight_segments_join_band_centers_without_gaps() {
        let width = 64;
        let mut levels = [0.0; BANDS];
        levels[16] = 1.0;
        assert_eq!(sample_at(&levels, 0, width), 0.0);
        assert_eq!(sample_at(&levels, 30, width), 0.0);
        // Band 16's center falls between dot columns 32 and 33; the slopes meet there.
        let rising = sample_at(&levels, 31, width);
        assert!(rising > 0.0 && rising < sample_at(&levels, 32, width));
        assert_eq!(sample_at(&levels, 32, width), sample_at(&levels, 33, width));
        // A narrow canvas merges bands instead of skipping them.
        assert_eq!(sample_at(&levels, 4, 8), 1.0);
        let dots = lit(&render(32, 12, &[levels]));
        for x in 0..64 {
            assert!(dots.iter().any(|(dx, _)| *dx == x), "column {x} is lit");
        }
        // 48 dot rows: amplitude min(0.4 × 48, 7 × 3) = 19.2 above baseline 47.
        let peak = dots.iter().map(|(_, y)| *y).min().unwrap();
        assert_eq!(
            peak,
            47 - (sample_at(&levels, 32, width) * 19.2).round() as i32
        );
        for x in 28..38 {
            let column: Vec<i32> = dots
                .iter()
                .filter(|(dx, _)| *dx == x)
                .map(|(_, y)| *y)
                .collect();
            let (top, bottom) = (column.iter().min().unwrap(), column.iter().max().unwrap());
            assert_eq!(
                column.len() as i32,
                bottom - top + 1,
                "column {x} has no gaps"
            );
        }
    }

    #[test]
    fn nearer_lines_hide_the_lines_behind_them() {
        // 48 dot rows keep every third frame; frame 0 is three dots behind frame 3.
        let flat = [0.1; BANDS];
        let mut near = [0.0; BANDS];
        near[..16].fill(1.0);
        let dots = lit(&render(40, 12, &[flat, flat, flat, near]));
        let behind = 47 - 3 - (0.1_f32 * 19.2).round() as i32;
        let back: Vec<i32> = dots
            .iter()
            .filter(|(_, y)| *y == behind)
            .map(|(x, _)| *x)
            .collect();
        assert!(
            back.iter().any(|x| *x > 50),
            "visible beside the quiet half"
        );
        assert!(
            back.iter().all(|x| *x > 34),
            "hidden behind the loud half: {back:?}"
        );
    }

    #[test]
    fn far_lines_fade_toward_the_canvas() {
        let p = Theme::default().palette();
        let buf = render(40, 12, &[[0.1; BANDS]; 30]);
        let distance = |color| {
            let [a, b] = [color, p.bg].map(crate::theme::channels);
            (0..3).map(|i| u32::from(a[i].abs_diff(b[i]))).sum::<u32>()
        };
        // The newest line lies in the bottom row; the line 26 dots back lies in row 4.
        let (near, far) = (buf[(20, 11)].fg, buf[(20, 4)].fg);
        assert_ne!(near, p.bg);
        assert_ne!(far, p.bg);
        assert!(distance(far) < distance(near), "{far:?} vs {near:?}");
    }

    #[test]
    fn kept_lines_scroll_one_dot_per_frame_on_tall_bodies() {
        let top = |buf: &Buffer| lit(buf).iter().map(|(_, y)| *y).min().unwrap();
        let tall = |frames: usize| {
            let mut history = vec![[0.0; BANDS]; frames];
            history[0] = [0.5; BANDS];
            top(&render(40, 12, &history))
        };
        assert_eq!(tall(8), tall(7) - 1);
        assert_eq!(tall(9), tall(7) - 2);
        // Short bodies move a dot every second frame.
        let short = |frames: usize| {
            let mut history = vec![[0.0; BANDS]; frames];
            history[0] = [0.5; BANDS];
            top(&render(40, 6, &history))
        };
        assert_eq!(short(7), short(8));
        assert_eq!(short(9), short(7) - 1);
    }

    #[test]
    fn tiny_bodies_show_only_the_newest_line() {
        let mut history = vec![[1.0; BANDS]; 6];
        history.push([0.0; BANDS]);
        let dots = lit(&render(40, 2, &history));
        assert!(!dots.is_empty());
        assert!(dots.iter().all(|(_, y)| *y == 7), "{dots:?}");
        assert!(lit(&render(40, 2, &[])).is_empty());
        for (width, height) in [(1, 1), (0, 3), (3, 0)] {
            render(width, height, &history);
        }
    }
}
