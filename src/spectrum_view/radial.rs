//! Polar bars on braille dots around a ring that follows the bass. Bands run from LOW at
//! the left over the top to HIGH at the right, and the lower half mirrors the upper one,
//! so the axis labels stay true. Bodies too short for a circle draw a mirrored strip.
use super::{BANDS, bands, braille::Braille, merged};
use crate::theme::{Palette, blend, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect};
use std::f32::consts::PI;

/// Cell size assumed when the terminal reports none.
const FALLBACK_CELL: (u16, u16) = (10, 20);
/// Ring radius at rest, as a share of the outer radius.
const RING: f32 = 0.36;
/// How far a strong bass widens the ring, as a share of its rest radius.
const PULSE: f32 = 0.25;
/// Smallest outer radius, in dots, that still reads as a circle.
const MIN_RADIUS: f32 = 7.0;
/// Bodies more than twice as wide as tall widen the circle into an ellipse, up to this
/// many times wider, so the figure is not lost in the middle of a short pane.
const STRETCH: f32 = 2.0;
/// A shared cell takes the color of a held peak first, then a ray, then the ring.
const PEAK: u32 = 0;
const RAY: u32 = 1;
const RIM: u32 = 2_000;

pub(super) fn draw(
    buf: &mut Buffer,
    body: Rect,
    p: &Palette,
    levels: &[f32; BANDS],
    peaks: &[f32; BANDS],
    cell: (u16, u16),
) {
    let mut canvas = Braille::new(body);
    let (columns, rows) = canvas.size();
    let (sx, sy) = dot_size(cell);
    let (width, height) = (columns as f32 * sx, rows as f32 * sy);
    let stretch = (width / height.max(1.0) / 2.0).clamp(1.0, STRETCH);
    // Narrower dots in the figure's own space draw it wider on screen.
    let sx = sx / stretch;
    let radius = (columns as f32 * sx).min(rows as f32 * sy) / 2.0 - sx.max(sy);
    if radius < MIN_RADIUS * sx.max(sy) {
        strip(&mut canvas, p, levels);
    } else {
        circle(&mut canvas, p, levels, peaks, (sx, sy), radius);
    }
    canvas.render(buf, p);
}

/// Pixels per dot from the terminal cell, with an implausible cell shape clamped.
fn dot_size(cell: (u16, u16)) -> (f32, f32) {
    let (width, height) = if cell.0 == 0 || cell.1 == 0 {
        FALLBACK_CELL
    } else {
        cell
    };
    let height = f32::from(height);
    let width = f32::from(width).clamp(0.3 * height, 0.8 * height);
    (width / 2.0, height / 4.0)
}

/// Rays per half: fewer on small rings, so neighbors stay apart; each ray takes the
/// loudest of its bands.
fn ray_count(rest: f32, reach: f32, unit: f32) -> usize {
    [32, 16, 8]
        .into_iter()
        .find(|rays| PI * (rest + reach / 2.0) / *rays as f32 >= 2.5 * unit)
        .unwrap_or(4)
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn circle(
    canvas: &mut Braille,
    p: &Palette,
    levels: &[f32; BANDS],
    peaks: &[f32; BANDS],
    (sx, sy): (f32, f32),
    radius: f32,
) {
    let (columns, rows) = canvas.size();
    let unit = sx.max(sy);
    let (cx, cy) = (columns as f32 * sx / 2.0, rows as f32 * sy / 2.0);
    // Typical music keeps the bass above half scale, so only strong bass moves the ring.
    let bass = levels[..5].iter().sum::<f32>() / 5.0;
    let pulse = smoothstep(0.5, 0.95, bass);
    let rest = RING * radius;
    let ring = rest * (1.0 + PULSE * pulse);
    let reach = radius - (1.0 + PULSE) * rest;
    let rays = ray_count(rest, reach, unit);
    let rim = blend(p.border, spectrum_gradient(p, bass), pulse);
    // Each upper-half dot is tested once and written to both halves, so the mirror is exact.
    for y in 0..rows / 2 {
        let dy = cy - (y as f32 + 0.5) * sy;
        for x in 0..columns {
            let dx = (x as f32 + 0.5) * sx - cx;
            let distance = dx.hypot(dy);
            let angle = dy.atan2(dx);
            let position = (PI - angle) / PI * rays as f32;
            let ray = (position.max(0.0) as usize).min(rays - 1);
            // Distance from the ray's center line, along the circle.
            let off = distance * (position - ray as f32 - 0.5).abs() * PI / rays as f32;
            // Half a dot measured along the radius, so the ring stays one dot thick.
            let half = 0.5 * (sx * angle.cos().abs()).max(sy * angle.sin().abs());
            let span = bands(ray, rays);
            let level = merged(levels, span.clone()).clamp(0.0, 1.0);
            let peak = merged(peaks, span).clamp(0.0, 1.0);
            let outward = distance - ring;
            // Rays start a dot wide and widen to two as their neighbors fall away.
            let width = (0.2 * distance * PI / rays as f32).clamp(0.5 * unit, unit);
            let mark = if (peak - level) * reach >= 1.5 * unit
                && (outward - peak * reach).abs() < half
                && off <= width + 0.5 * unit
            {
                Some((spectrum_gradient(p, peak), PEAK))
            } else if outward >= half && outward <= level * reach && off <= width {
                let t = outward / reach;
                // The hotter end of a ray wins a shared cell.
                Some((spectrum_gradient(p, t), RAY + ((1.0 - t) * 1_000.0) as u32))
            } else if outward.abs() < half {
                Some((rim, RIM))
            } else {
                None
            };
            if let Some((color, rank)) = mark {
                canvas.dot(x, y, color, rank);
                canvas.dot(x, rows - 1 - y, color, rank);
            }
        }
    }
}

/// A strip mirrored around its center line, LOW at the left like the circle's halves.
fn strip(canvas: &mut Braille, p: &Palette, levels: &[f32; BANDS]) {
    let (columns, rows) = canvas.size();
    let half = rows / 2;
    for x in 0..columns {
        let level = merged(levels, bands(x as usize, columns as usize)).clamp(0.0, 1.0);
        let reach = (level * half as f32).round() as i32;
        for step in 0..reach {
            let color = spectrum_gradient(p, (step as f32 + 0.5) / half as f32);
            canvas.dot(x, half - 1 - step, color, 0);
            canvas.dot(x, half + step, color, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{spectrum_view::braille::dots_of, theme::Theme};

    const CELL: (u16, u16) = (10, 20);

    fn render(
        width: u16,
        height: u16,
        levels: [f32; BANDS],
        peaks: [f32; BANDS],
        cell: (u16, u16),
    ) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        draw(
            &mut buf,
            area,
            &Theme::default().palette(),
            &levels,
            &peaks,
            cell,
        );
        buf
    }

    /// Every lit dot as (x, y) in canvas dots.
    fn lit(buf: &Buffer) -> Vec<(i32, i32)> {
        let area = buf.area;
        let mut dots = Vec::new();
        for y in 0..area.height {
            for x in 0..area.width {
                for (dx, dy) in dots_of(buf[(x, y)].symbol()) {
                    dots.push((i32::from(x * 2 + dx), i32::from(y * 4 + dy)));
                }
            }
        }
        dots
    }

    fn bands_at(range: std::ops::Range<usize>, value: f32) -> [f32; BANDS] {
        std::array::from_fn(|band| if range.contains(&band) { value } else { 0.0 })
    }

    #[test]
    fn lower_half_mirrors_the_upper_half_exactly() {
        let levels: [f32; BANDS] = std::array::from_fn(|band| (band % 7) as f32 / 6.0);
        let peaks: [f32; BANDS] = std::array::from_fn(|band| (levels[band] + 0.3).min(1.0));
        let buf = render(60, 16, levels, peaks, CELL);
        let mut rows = 0;
        for y in 0..8 {
            for x in 0..60 {
                let (upper, lower) = (&buf[(x, y)], &buf[(x, 15 - y)]);
                let flipped: Vec<_> = dots_of(lower.symbol())
                    .into_iter()
                    .map(|(dx, dy)| (dx, 3 - dy))
                    .collect();
                let mut expected = dots_of(upper.symbol());
                expected.sort_unstable();
                let mut flipped = flipped;
                flipped.sort_unstable();
                assert_eq!(expected, flipped, "cell {x},{y}");
                assert_eq!(upper.fg, lower.fg, "cell {x},{y}");
                rows += usize::from(!expected.is_empty());
            }
        }
        assert!(rows > 0);
    }

    #[test]
    fn low_bands_light_the_left_and_high_bands_the_right() {
        let silent = lit(&render(60, 16, [0.0; BANDS], [0.0; BANDS], CELL));
        let center = 60;
        for (range, left) in [(6..10, true), (22..27, false)] {
            let dots = lit(&render(60, 16, bands_at(range, 1.0), [0.0; BANDS], CELL));
            let rays: Vec<_> = dots.iter().filter(|dot| !silent.contains(dot)).collect();
            assert!(rays.len() > 10, "{rays:?}");
            assert!(
                rays.iter().all(|(x, _)| (*x < center) == left),
                "left={left}: {rays:?}"
            );
        }
    }

    #[test]
    fn idle_ring_uses_the_border_role_and_bass_widens_it() {
        let p = Theme::default().palette();
        let buf = render(60, 16, [0.0; BANDS], [0.0; BANDS], CELL);
        assert!(
            buf.content()
                .iter()
                .all(|cell| cell.symbol() == " " || cell.fg == p.border)
        );
        let rightmost = |buf: &Buffer| {
            lit(buf)
                .into_iter()
                .filter(|(_, y)| *y == 31)
                .map(|(x, _)| x)
                .max()
                .unwrap()
        };
        let resting = rightmost(&buf);
        // Strong bass only: the right side has no rays, so its outermost dot is the ring.
        let pulsing = rightmost(&render(60, 16, bands_at(0..5, 1.0), [0.0; BANDS], CELL));
        assert!(pulsing > resting, "{pulsing} > {resting}");
        let more = lit(&render(60, 16, [0.9; BANDS], [0.0; BANDS], CELL)).len();
        let fewer = lit(&render(60, 16, [0.3; BANDS], [0.0; BANDS], CELL)).len();
        assert!(more > fewer, "{more} > {fewer}");
    }

    #[test]
    fn held_peaks_float_beyond_the_rays_in_the_color_of_their_height() {
        let p = Theme::default().palette();
        let buf = render(60, 16, [0.5; BANDS], [1.0; BANDS], CELL);
        assert!(buf.content().iter().any(|cell| cell.fg == p.spectrum[2]));
        let without = lit(&render(60, 16, [0.2; BANDS], [0.0; BANDS], CELL)).len();
        let with = lit(&render(60, 16, [0.2; BANDS], [0.9; BANDS], CELL)).len();
        assert!(with > without, "caps add dots: {with} > {without}");
        let close = lit(&render(60, 16, [0.2; BANDS], [0.21; BANDS], CELL)).len();
        assert_eq!(close, without, "a peak at the tip adds no cap");
    }

    #[test]
    fn circles_follow_the_cell_shape() {
        // Width / height of the resting ring in dots is the inverse of the dot shape.
        for (cell, expected) in [((10, 20), 1.0), ((8, 20), 1.25), ((12, 20), 5.0 / 6.0)] {
            let dots = lit(&render(80, 24, [0.0; BANDS], [0.0; BANDS], cell));
            let span = |values: Vec<i32>| {
                (values.iter().max().unwrap() - values.iter().min().unwrap() + 1) as f32
            };
            let ratio =
                span(dots.iter().map(|d| d.0).collect()) / span(dots.iter().map(|d| d.1).collect());
            assert!(
                (ratio / expected - 1.0).abs() < 0.15,
                "{cell:?}: {ratio} vs {expected}"
            );
        }
        // Unknown or absurd cell sizes still draw a ring.
        for cell in [(0, 0), (1, 400), (400, 1)] {
            assert!(!lit(&render(80, 24, [0.5; BANDS], [0.0; BANDS], cell)).is_empty());
        }
    }

    #[test]
    fn wide_short_bodies_stretch_the_ring_up_to_twice_as_wide() {
        let dots = lit(&render(60, 7, [0.0; BANDS], [0.0; BANDS], CELL));
        let span = |values: Vec<i32>| {
            (values.iter().max().unwrap() - values.iter().min().unwrap() + 1) as f32
        };
        // Square dots at 10 × 20 cells: the extent in dots is the extent on screen.
        let ratio =
            span(dots.iter().map(|d| d.0).collect()) / span(dots.iter().map(|d| d.1).collect());
        assert!((1.7..2.3).contains(&ratio), "{ratio}");
    }

    #[test]
    fn small_rings_merge_bands_into_fewer_rays() {
        let rays = |height: f32| {
            let (sx, sy) = dot_size(CELL);
            let radius = (height * 4.0 * sy) / 2.0 - sx.max(sy);
            let rest = RING * radius;
            ray_count(rest, radius - (1.0 + PULSE) * rest, sx.max(sy))
        };
        assert_eq!(rays(7.0), 8);
        assert_eq!(rays(17.0), 16);
        assert_eq!(rays(21.0), 32);
    }

    #[test]
    fn short_bodies_draw_a_strip_mirrored_around_its_center() {
        let levels = bands_at(0..16, 1.0);
        let buf = render(40, 2, levels, [0.0; BANDS], CELL);
        let dots = lit(&buf);
        assert!(!dots.is_empty());
        for (x, y) in &dots {
            assert!(*x < 40, "only the low half lights: {x}");
            assert!(dots.contains(&(*x, 7 - y)), "mirror of {x},{y}");
        }
        let empty = render(40, 2, [0.0; BANDS], [0.0; BANDS], CELL);
        assert!(lit(&empty).is_empty());
    }
}
