//! Polar petals on braille dots around a ring. Bands run from LOW at the left over the
//! top to HIGH at the right, and the lower half mirrors the upper one, so the axis labels
//! stay true. Onsets between frames pump a glowing core and send waves outward. Bodies too
//! short for a circle draw a mirrored strip.
use super::{BANDS, bands, braille::Braille, merged};
use crate::theme::{Palette, blend, spectrum_gradient};
use ratatui::{buffer::Buffer, layout::Rect, style::Color};
use std::f32::consts::PI;

/// Cell size assumed when the terminal reports none.
const FALLBACK_CELL: (u16, u16) = (10, 20);
/// Ring radius at rest, as a share of the outer radius.
const RING: f32 = 0.36;
/// How far a full onset widens the ring, as a share of its rest radius.
const PULSE: f32 = 0.15;
/// Smallest outer radius, in dots, that still reads as a circle.
const MIN_RADIUS: f32 = 7.0;
/// Bodies more than twice as wide as tall widen the circle into an ellipse, up to this
/// many times wider, so the figure is not lost in the middle of a short pane.
const STRETCH: f32 = 2.0;
/// A frame is an onset when the mean rise over all bands since the previous frame beats
/// this floor and this many times the recent average rise.
const ONSET_FLOOR: f32 = 0.02;
const ONSET_RATIO: f32 = 1.8;
/// Seconds before another onset counts.
const COOLDOWN: f32 = 0.25;
/// Fall of the onset pulse per second, so a full pulse fades within a third of a second.
const FALL: f32 = 3.0;
/// Seconds a wave takes from the ring to the edge.
const WAVE: f32 = 0.7;
/// Waves in flight at once; a new one replaces the oldest.
const WAVES: usize = 3;
/// Background tint behind petal tips, and at most behind the core and a fresh wave.
const TIP_GLOW: f32 = 0.18;
const CORE_GLOW: f32 = 0.3;
const WAVE_GLOW: f32 = 0.15;
/// A shared cell takes the color of a held peak first, then a petal tip, a petal (the
/// hotter end first), the ring, a wave, and the core.
const PEAK: u32 = 0;
const TIP: u32 = 1;
const PETAL: u32 = 2;
const RIM: u32 = 2_000;
const RIPPLE: u32 = 2_100;
const CORE: u32 = 3_000;

struct Wave {
    age: f32,
    strength: f32,
}

#[derive(Default)]
pub(super) struct Radial {
    /// Recent average of the mean rise between frames.
    flux: f32,
    /// Strongest onset since the last draw, 0–1.
    onset: f32,
    /// Seconds before another onset counts.
    cooldown: f32,
    /// Onset pulse, 0–1: jumps with an onset and falls back.
    pulse: f32,
    waves: Vec<Wave>,
}

/// One dot of the figure: its color, rank, and optional background tint.
type Mark = (Color, u32, Option<(Color, f32)>);

impl Radial {
    pub fn reset(&mut self) {
        self.flux = 0.0;
        self.onset = 0.0;
        self.cooldown = 0.0;
        self.pulse = 0.0;
        self.waves.clear();
    }

    /// Whether the pulse or a wave still moves. A pending onset arrives with a fresh
    /// frame, which draws anyway; a panel without a body never consumes it.
    pub fn is_active(&self) -> bool {
        self.pulse > 0.0 || !self.waves.is_empty()
    }

    /// Notes an onset between two consecutive frames of one stream. Its strength grows
    /// from 0.4 at the threshold to 1 at three times the threshold ratio.
    pub fn observe(&mut self, previous: &[f32; BANDS], next: &[f32; BANDS]) {
        let rise = previous
            .iter()
            .zip(next)
            .map(|(before, after)| (after - before).max(0.0))
            .sum::<f32>()
            / BANDS as f32;
        if rise > ONSET_FLOOR.max(ONSET_RATIO * self.flux) {
            let ratio = rise / self.flux.max(ONSET_FLOOR / ONSET_RATIO);
            let strength = 0.4 + 0.6 * (ratio - ONSET_RATIO) / (2.0 * ONSET_RATIO);
            self.onset = self.onset.max(strength.clamp(0.4, 1.0));
        }
        self.flux += 0.1 * (rise - self.flux);
    }

    pub fn draw(
        &mut self,
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
            self.circle(&mut canvas, p, levels, peaks, (sx, sy), radius);
        }
        canvas.render(buf, p);
    }

    /// Ages the pulse and waves by `dt` seconds and starts a wave for a pending onset.
    pub fn advance(&mut self, dt: f32) {
        self.pulse = (self.pulse - FALL * dt).max(0.0);
        self.cooldown = (self.cooldown - dt).max(0.0);
        for wave in &mut self.waves {
            wave.age += dt;
        }
        self.waves.retain(|wave| wave.age < WAVE);
        if self.onset > 0.0 && self.cooldown == 0.0 {
            self.pulse = self.pulse.max(self.onset);
            if self.waves.len() == WAVES {
                self.waves.remove(0);
            }
            self.waves.push(Wave {
                age: 0.0,
                strength: self.onset,
            });
            self.cooldown = COOLDOWN;
        }
        self.onset = 0.0;
    }

    fn circle(
        &self,
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
        let loud = levels.iter().sum::<f32>() / BANDS as f32;
        let energy = self.pulse.max(0.5 * loud);
        let rest = RING * radius;
        let ring = rest * (1.0 + PULSE * self.pulse);
        let reach = radius - (1.0 + PULSE) * rest;
        let rays = ray_count(rest, reach, unit);
        let rim = blend(p.border, p.accent, energy);
        // The core follows the overall level and swells with each onset.
        let core = rest * (0.5 * loud + 0.3 * self.pulse).min(0.8);
        // Waves leave the ring fast and slow down toward the edge as they fade.
        let waves: Vec<(f32, f32)> = self
            .waves
            .iter()
            .map(|wave| {
                let progress = wave.age / WAVE;
                let eased = 1.0 - (1.0 - progress).powi(2);
                (
                    ring + (radius - ring) * eased,
                    (1.0 - progress) * wave.strength,
                )
            })
            .collect();
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
                let arc = distance * PI / rays as f32;
                let off = arc * (position - ray as f32 - 0.5).abs();
                // Half a dot measured along the radius, so circles stay one dot thick.
                let half = 0.5 * (sx * angle.cos().abs()).max(sy * angle.sin().abs());
                let span = bands(ray, rays);
                let level = merged(levels, span.clone()).clamp(0.0, 1.0);
                let peak = merged(peaks, span).clamp(0.0, 1.0);
                let outward = distance - ring;
                let tip = level * reach;
                // Petals fill 60% of their sector and keep at least a dot between them.
                let width = (0.3 * arc).min(0.5 * arc - 0.5 * unit).max(0.5 * unit);
                let mark: Option<Mark> = if (peak - level) * reach >= 1.5 * unit
                    && (outward - peak * reach).abs() < half
                    && off <= width + 0.5 * unit
                {
                    Some((spectrum_gradient(p, peak), PEAK, None))
                } else if outward >= half && outward <= tip && off <= width {
                    let t = outward / reach;
                    let color = spectrum_gradient(p, t);
                    if outward > tip - unit {
                        let tint = Some((color, TIP_GLOW));
                        Some((blend(color, p.text, 0.5), TIP, tint))
                    } else {
                        Some((color, PETAL + ((1.0 - t) * 1_000.0) as u32, None))
                    }
                } else if outward.abs() < half {
                    Some((rim, RIM, None))
                } else if distance <= core {
                    let inner = 1.0 - distance / core;
                    let color = blend(p.accent, p.text, inner * energy);
                    let rank = CORE + (distance / core * 100.0) as u32;
                    Some((color, rank, Some((p.accent, CORE_GLOW * energy))))
                } else {
                    waves
                        .iter()
                        .find(|(at, _)| (distance - at).abs() < half)
                        .map(|(_, fade)| {
                            let color = blend(p.bg, p.accent, *fade);
                            (color, RIPPLE, Some((p.accent, WAVE_GLOW * fade)))
                        })
                };
                if let Some((color, rank, tint)) = mark {
                    for y in [y, rows - 1 - y] {
                        canvas.dot(x, y, color, rank);
                        if let Some((tint, strength)) = tint {
                            canvas.glow(x, y, tint, strength);
                        }
                    }
                }
            }
        }
    }
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
    const SILENT: [f32; BANDS] = [0.0; BANDS];

    /// Draws `radial` once, `dt` seconds after its previous draw.
    fn draw_with(
        radial: &mut Radial,
        (width, height): (u16, u16),
        levels: [f32; BANDS],
        peaks: [f32; BANDS],
        dt: f32,
    ) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let p = Theme::default().palette();
        radial.advance(dt);
        radial.draw(&mut buf, area, &p, &levels, &peaks, CELL);
        buf
    }

    /// A fresh figure: no onset has pulsed the ring or sent a wave.
    fn render(
        width: u16,
        height: u16,
        levels: [f32; BANDS],
        peaks: [f32; BANDS],
        cell: (u16, u16),
    ) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let p = Theme::default().palette();
        Radial::default().draw(&mut buf, area, &p, &levels, &peaks, cell);
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

    /// Distance of a dot from the center of a 60 × 16 body, in dots (square at 10 × 20).
    fn from_center((x, y): (i32, i32)) -> f32 {
        (x as f32 + 0.5 - 60.0).hypot(y as f32 + 0.5 - 32.0)
    }

    /// The outermost lit dot on the row just above the center of a 60 × 16 body.
    fn rightmost(buf: &Buffer) -> i32 {
        lit(buf)
            .into_iter()
            .filter(|(_, y)| *y == 31)
            .map(|(x, _)| x)
            .max()
            .unwrap()
    }

    fn bands_at(range: std::ops::Range<usize>, value: f32) -> [f32; BANDS] {
        std::array::from_fn(|band| if range.contains(&band) { value } else { 0.0 })
    }

    #[test]
    fn lower_half_mirrors_the_upper_half_exactly() {
        let levels: [f32; BANDS] = std::array::from_fn(|band| (band % 7) as f32 / 6.0);
        let peaks: [f32; BANDS] = std::array::from_fn(|band| (levels[band] + 0.3).min(1.0));
        // A pulsed core, glow, and a wave in flight mirror as well.
        let mut radial = Radial::default();
        radial.observe(&SILENT, &levels);
        draw_with(&mut radial, (60, 16), levels, peaks, 0.0);
        let buf = draw_with(&mut radial, (60, 16), levels, peaks, 0.2);
        assert_eq!(radial.waves.len(), 1);
        let mut rows = 0;
        for y in 0..8 {
            for x in 0..60 {
                let (upper, lower) = (&buf[(x, y)], &buf[(x, 15 - y)]);
                let mut flipped: Vec<_> = dots_of(lower.symbol())
                    .into_iter()
                    .map(|(dx, dy)| (dx, 3 - dy))
                    .collect();
                let mut expected = dots_of(upper.symbol());
                expected.sort_unstable();
                flipped.sort_unstable();
                assert_eq!(expected, flipped, "cell {x},{y}");
                assert_eq!((upper.fg, upper.bg), (lower.fg, lower.bg), "cell {x},{y}");
                rows += usize::from(!expected.is_empty());
            }
        }
        assert!(rows > 0);
    }

    #[test]
    fn low_bands_light_the_left_and_high_bands_the_right() {
        let silent = lit(&render(60, 16, SILENT, SILENT, CELL));
        for (range, left) in [(6..10, true), (22..27, false)] {
            let dots = lit(&render(60, 16, bands_at(range, 1.0), SILENT, CELL));
            // The core at the center follows the overall level, not one side.
            let petals: Vec<_> = dots
                .iter()
                .filter(|dot| !silent.contains(dot) && from_center(**dot) > 10.0)
                .collect();
            assert!(petals.len() > 10, "{petals:?}");
            assert!(
                petals.iter().all(|(x, _)| (*x < 60) == left),
                "left={left}: {petals:?}"
            );
        }
    }

    #[test]
    fn the_idle_ring_uses_the_border_role_and_onsets_widen_it() {
        let p = Theme::default().palette();
        let buf = render(60, 16, SILENT, SILENT, CELL);
        assert!(buf.content().iter().all(|cell| cell.bg == p.bg));
        assert!(
            buf.content()
                .iter()
                .all(|cell| cell.symbol() == " " || cell.fg == p.border)
        );
        let mut radial = Radial::default();
        radial.observe(&SILENT, &[0.8; BANDS]);
        // Silent levels: no petals, so the outermost dot on the row is the ring.
        let pulsed = draw_with(&mut radial, (60, 16), SILENT, SILENT, 0.0);
        assert!(rightmost(&pulsed) > rightmost(&buf));
        assert!(pulsed.content().iter().any(|cell| cell.fg == p.accent));
        let more = lit(&render(60, 16, [0.9; BANDS], SILENT, CELL)).len();
        let fewer = lit(&render(60, 16, [0.3; BANDS], SILENT, CELL)).len();
        assert!(more > fewer, "{more} > {fewer}");
    }

    #[test]
    fn onsets_need_a_clear_rise_over_the_recent_average() {
        let mut radial = Radial::default();
        let quiet = [0.3; BANDS];
        for _ in 0..20 {
            radial.observe(&quiet, &quiet);
        }
        radial.observe(&quiet, &[0.31; BANDS]);
        assert_eq!(
            radial.onset, 0.0,
            "steady levels and tiny rises are no onset"
        );
        radial.observe(&quiet, &[0.36; BANDS]);
        assert!(radial.onset > 0.0, "a rise stands out in a quiet passage");
        // A passage that rises every frame raises the bar for the same rise.
        let mut busy = Radial::default();
        for _ in 0..30 {
            busy.observe(&quiet, &[0.36; BANDS]);
        }
        busy.onset = 0.0;
        busy.observe(&quiet, &[0.36; BANDS]);
        assert_eq!(busy.onset, 0.0);
        busy.observe(&quiet, &[0.5; BANDS]);
        assert!((0.4..=1.0).contains(&busy.onset), "{}", busy.onset);
    }

    #[test]
    fn onsets_swell_a_glowing_core() {
        let p = Theme::default().palette();
        let levels = [0.5; BANDS];
        let core = |buf: &Buffer| {
            lit(buf)
                .into_iter()
                .filter(|dot| from_center(*dot) < 9.0)
                .count()
        };
        let mut radial = Radial::default();
        let steady = draw_with(&mut radial, (60, 16), levels, SILENT, 0.05);
        radial.observe(&[0.2; BANDS], &levels);
        let hit = draw_with(&mut radial, (60, 16), levels, SILENT, 0.05);
        assert!(
            core(&hit) > 2 * core(&steady),
            "{} {}",
            core(&hit),
            core(&steady)
        );
        // The cell at the center glows toward the accent role at full strength.
        assert_eq!(hit[(30, 8)].bg, blend(p.bg, p.accent, CORE_GLOW));
        assert_ne!(steady[(30, 8)].bg, hit[(30, 8)].bg);
    }

    #[test]
    fn waves_travel_outward_fade_and_expire() {
        let p = Theme::default().palette();
        let resting = render(60, 16, SILENT, SILENT, CELL);
        let mut radial = Radial::default();
        radial.observe(&SILENT, &[0.6; BANDS]);
        let mut reach = Vec::new();
        let mut colors = Vec::new();
        for _ in 0..5 {
            let buf = draw_with(&mut radial, (60, 16), SILENT, SILENT, 0.1);
            let x = rightmost(&buf);
            reach.push(x);
            colors.push(buf[(x as u16 / 2, 7)].fg);
        }
        assert!(reach.windows(2).all(|pair| pair[1] > pair[0]), "{reach:?}");
        let fade = |color| crate::theme::channels(color).map(i32::from);
        let distance = |color| {
            let (a, b) = (fade(color), fade(p.bg));
            (0..3).map(|i| (a[i] - b[i]).abs()).sum::<i32>()
        };
        assert!(
            distance(colors[4]) < distance(colors[1]),
            "older waves fade toward the canvas"
        );
        // A wave lives 0.7 seconds; the figure then rests as if nothing happened.
        let buf = draw_with(&mut radial, (60, 16), SILENT, SILENT, 0.3);
        assert!(!radial.is_active());
        assert_eq!(buf, resting);
    }

    #[test]
    fn onsets_inside_the_cooldown_send_one_wave_and_three_at_most_fly() {
        let mut radial = Radial::default();
        radial.observe(&SILENT, &[0.6; BANDS]);
        draw_with(&mut radial, (60, 16), SILENT, SILENT, 0.05);
        radial.observe(&SILENT, &[0.6; BANDS]);
        draw_with(&mut radial, (60, 16), SILENT, SILENT, 0.1);
        assert_eq!(radial.waves.len(), 1);
        for _ in 0..6 {
            radial.observe(&SILENT, &[0.6; BANDS]);
            draw_with(&mut radial, (60, 16), SILENT, SILENT, 0.26);
            assert!(radial.waves.len() <= WAVES);
        }
        assert_eq!(radial.waves.len(), WAVES);
    }

    #[test]
    fn held_peaks_float_beyond_the_rays_in_the_color_of_their_height() {
        let p = Theme::default().palette();
        let buf = render(60, 16, [0.5; BANDS], [1.0; BANDS], CELL);
        assert!(buf.content().iter().any(|cell| cell.fg == p.spectrum[2]));
        let without = lit(&render(60, 16, [0.2; BANDS], SILENT, CELL)).len();
        let with = lit(&render(60, 16, [0.2; BANDS], [0.9; BANDS], CELL)).len();
        assert!(with > without, "caps add dots: {with} > {without}");
        let close = lit(&render(60, 16, [0.2; BANDS], [0.21; BANDS], CELL)).len();
        assert_eq!(close, without, "a peak at the tip adds no cap");
    }

    #[test]
    fn circles_follow_the_cell_shape() {
        // Width / height of the resting ring in dots is the inverse of the dot shape.
        for (cell, expected) in [((10, 20), 1.0), ((8, 20), 1.25), ((12, 20), 5.0 / 6.0)] {
            let dots = lit(&render(80, 24, SILENT, SILENT, cell));
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
            assert!(!lit(&render(80, 24, [0.5; BANDS], SILENT, cell)).is_empty());
        }
    }

    #[test]
    fn wide_short_bodies_stretch_the_ring_up_to_twice_as_wide() {
        let dots = lit(&render(60, 7, SILENT, SILENT, CELL));
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
        let buf = render(40, 2, levels, SILENT, CELL);
        let dots = lit(&buf);
        assert!(!dots.is_empty());
        for (x, y) in &dots {
            assert!(*x < 40, "only the low half lights: {x}");
            assert!(dots.contains(&(*x, 7 - y)), "mirror of {x},{y}");
        }
        let empty = render(40, 2, SILENT, SILENT, CELL);
        assert!(lit(&empty).is_empty());
    }
}
