//! Sparks thrown from the bar tops when a band jumps between two frames. They fly on
//! braille dots under gravity, cool from the text role toward the canvas, and only use
//! cells the bars leave blank.
use super::{BANDS, Bar, bar_layout, braille::Braille, merged};
use crate::theme::{Palette, blend};
use rand::RngExt;
use ratatui::{buffer::Buffer, layout::Rect, style::Color};

/// A rise between two frames throws sparks when it beats this floor and one and a half
/// times the band's recent average rise, so a busy band needs a real jump.
const RISE: f32 = 0.1;
/// Quieter bands throw nothing.
const LOUD: f32 = 0.25;
/// Seconds before a bar throws again.
const COOLDOWN: f32 = 0.15;
/// Longest life of a spark, in seconds.
const LIFE: f32 = 1.1;
/// Seconds of travel shown as a streak behind a spark, at most three dots long.
const STREAK: f32 = 0.04;
/// Color steps as a spark cools.
const SHADES: f32 = 8.0;

struct Spark {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: f32,
    life: f32,
}

#[derive(Default)]
pub(super) struct Sparks {
    sparks: Vec<Spark>,
    /// Largest qualifying rise per band since the last draw.
    onsets: [f32; BANDS],
    /// Recent average rise per band.
    flux: [f32; BANDS],
    /// Seconds until each band may throw again.
    cooldown: [f32; BANDS],
    /// Body size the spark positions belong to.
    size: (u16, u16),
}

impl Sparks {
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.onsets = [0.0; BANDS];
        self.flux = [0.0; BANDS];
        self.cooldown = [0.0; BANDS];
    }

    /// Whether sparks are still in flight.
    pub fn is_active(&self) -> bool {
        !self.sparks.is_empty()
    }

    /// Notes the rises between two consecutive frames of one stream.
    pub fn observe(&mut self, previous: &[f32; BANDS], next: &[f32; BANDS]) {
        for (((onset, flux), before), after) in self
            .onsets
            .iter_mut()
            .zip(&mut self.flux)
            .zip(previous)
            .zip(next)
        {
            let rise = after - before;
            if rise > RISE.max(1.5 * *flux) && *after > LOUD {
                *onset = onset.max(rise);
            }
            *flux = 0.85 * *flux + 0.15 * rise.max(0.0);
        }
    }

    /// Moves, throws, and draws the sparks over bars already in `buf`.
    pub fn draw(
        &mut self,
        buf: &mut Buffer,
        body: Rect,
        p: &Palette,
        levels: &[f32; BANDS],
        dt: f32,
        rng: &mut impl RngExt,
    ) {
        if (body.width, body.height) != self.size {
            self.sparks.clear();
            self.size = (body.width, body.height);
        }
        let mut canvas = Braille::new(body);
        let (columns, rows) = canvas.size();
        let (width, height) = (columns as f32, rows as f32);
        for spark in &mut self.sparks {
            spark.vy += gravity(rows) * dt;
            spark.x += spark.vx * dt;
            spark.y += spark.vy * dt;
            spark.age += dt;
        }
        self.sparks
            .retain(|s| s.age < s.life && s.y < height && (0.0..width).contains(&s.x));
        for cooldown in &mut self.cooldown {
            *cooldown = (*cooldown - dt).max(0.0);
        }
        // One or two rows leave no room to fly; the bars stay plain there.
        if body.height > 2 {
            self.throw(body, levels, rng);
        }
        self.onsets = [0.0; BANDS];
        for spark in &self.sparks {
            let shade = ((spark.age / spark.life).min(1.0) * SHADES).floor();
            let color = cooling(p, shade / SHADES);
            let speed = spark.vx.hypot(spark.vy);
            let (dx, dy) = if speed > 0.0 {
                (spark.vx / speed, spark.vy / speed)
            } else {
                (0.0, 0.0)
            };
            // A short streak behind the head shows the direction of travel; younger
            // sparks win shared cells.
            for back in 0..=(speed * STREAK).min(3.0) as usize {
                let back = back as f32;
                canvas.dot(
                    (spark.x - dx * back).floor() as i32,
                    (spark.y - dy * back).floor() as i32,
                    color,
                    shade as u32,
                );
            }
        }
        canvas.overlay(buf, p);
    }

    fn throw(&mut self, body: Rect, levels: &[f32; BANDS], rng: &mut impl RngExt) {
        let rows = i32::from(body.height) * 4;
        let cap = (usize::from(body.width) * usize::from(body.height) / 3).clamp(24, 240);
        let bars: Vec<Bar> = bar_layout(body.width).collect();
        // Busy music would light every bar at once; the largest rises go first.
        let budget = (bars.len() / 4).max(3);
        let mut throws: Vec<(f32, Bar)> = bars
            .into_iter()
            .filter_map(|bar| {
                let rise = merged(&self.onsets, bar.bands.clone());
                let ready = merged(&self.cooldown, bar.bands.clone()) == 0.0;
                (rise > 0.0 && ready).then_some((rise, bar))
            })
            .collect();
        throws.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (rise, bar) in throws.into_iter().take(budget) {
            let level = merged(levels, bar.bands.clone()).clamp(0.0, 1.0);
            // The bar's top cell is partly filled and braille cannot share it, so sparks
            // start in the lowest dot row of the first blank cell above.
            let filled = (level * f32::from(body.height)).ceil() as i32;
            let start = (i32::from(body.height) - filled) * 4 - 1;
            if start < 2 {
                continue;
            }
            let apex = ((0.2 + 0.8 * rise) * 0.5 * rows as f32).min(start as f32);
            let speed = (2.0 * gravity(rows) * apex).sqrt();
            let left = f32::from(bar.columns.start) * 2.0;
            let right = f32::from(bar.columns.end) * 2.0;
            for _ in 0..((rise * 16.0).round() as usize).clamp(2, 6) {
                if self.sparks.len() >= cap {
                    return;
                }
                let vy = -speed * rng.random_range(0.8..=1.0);
                self.sparks.push(Spark {
                    x: rng.random_range(left..right),
                    y: start as f32 + 0.5,
                    vx: rng.random_range(-1.0..=1.0) * 0.35 * speed,
                    vy,
                    age: 0.0,
                    life: rng.random_range(0.7..=LIFE),
                });
            }
            for band in bar.bands {
                self.cooldown[band] = COOLDOWN;
            }
        }
    }
}

/// Dots per second squared: a fall through the whole body takes under a second.
fn gravity(rows: i32) -> f32 {
    2.5 * rows as f32
}

/// Fresh sparks are the text role; they cool through the middle and high roles and fade
/// most of the way into the canvas.
fn cooling(p: &Palette, age: f32) -> Color {
    if age <= 0.2 {
        blend(p.text, p.spectrum[1], age / 0.2)
    } else if age <= 0.6 {
        blend(p.spectrum[1], p.spectrum[2], (age - 0.2) / 0.4)
    } else {
        blend(p.spectrum[2], p.bg, 0.7 * (age - 0.6) / 0.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{spectrum_view::braille::dots_of, theme::Theme};
    use rand::{SeedableRng, rngs::SmallRng};

    fn rise(band: usize, from: f32, to: f32) -> ([f32; BANDS], [f32; BANDS]) {
        let (mut before, mut after) = ([from; BANDS], [from; BANDS]);
        before[band] = from;
        after[band] = to;
        (before, after)
    }

    fn draw(sparks: &mut Sparks, body: Rect, levels: &[f32; BANDS], dt: f32, seed: u64) -> Buffer {
        let mut buf = Buffer::empty(body);
        sparks.draw(
            &mut buf,
            body,
            &Theme::default().palette(),
            levels,
            dt,
            &mut SmallRng::seed_from_u64(seed),
        );
        buf
    }

    #[test]
    fn only_sharp_loud_rises_count_and_busy_bands_need_more() {
        let mut sparks = Sparks::default();
        sparks.observe(&[0.5; BANDS], &[0.5; BANDS]);
        assert_eq!(sparks.onsets, [0.0; BANDS], "steady frames throw nothing");
        let (before, after) = rise(3, 0.05, 0.2);
        sparks.observe(&before, &after);
        assert_eq!(sparks.onsets[3], 0.0, "too quiet");
        let (before, after) = rise(3, 0.3, 0.8);
        sparks.observe(&before, &after);
        assert_eq!(sparks.onsets[3], 0.5);
        let mut busy = Sparks::default();
        for _ in 0..20 {
            busy.observe(&[0.3; BANDS], &[0.6; BANDS]);
        }
        busy.onsets = [0.0; BANDS];
        busy.observe(&[0.4; BANDS], &[0.6; BANDS]);
        assert_eq!(
            busy.onsets, [0.0; BANDS],
            "a rise below 1.5 times the recent average"
        );
    }

    #[test]
    fn a_rise_throws_sparks_above_its_own_bar_only() {
        let body = Rect::new(0, 0, 64, 12);
        let mut sparks = Sparks::default();
        let (before, after) = rise(10, 0.3, 0.7);
        sparks.observe(&before, &after);
        let buf = draw(&mut sparks, body, &after, 0.0, 4);
        // Bar 10 is dot columns 40–41; 0.7 × 12 rows reaches into the ninth cell from the
        // bottom, so sparks start in the lowest dot row of cell row 2.
        assert_eq!(bar_layout(64).nth(10).unwrap().columns, 20..21);
        assert_eq!(sparks.sparks.len(), 6);
        assert!(
            sparks
                .sparks
                .iter()
                .all(|s| (40.0..42.0).contains(&s.x) && s.y == 11.5)
        );
        let mut lit = 0;
        for y in 0..12 {
            for x in 0..64 {
                if !dots_of(buf[(x, y)].symbol()).is_empty() {
                    lit += 1;
                    // Streaks trail a dot behind the head.
                    assert!((19..=21).contains(&x) && y <= 3, "cell {x},{y}");
                }
            }
        }
        assert!(lit > 0);
        assert_eq!(sparks.onsets, [0.0; BANDS], "a draw consumes the onsets");
        // The bar waits before it throws again.
        let count = sparks.sparks.len();
        sparks.observe(&before, &after);
        draw(&mut sparks, body, &after, 0.05, 5);
        assert_eq!(sparks.sparks.len(), count);
    }

    #[test]
    fn sparks_fall_cool_and_expire() {
        let body = Rect::new(0, 0, 64, 12);
        let mut sparks = Sparks::default();
        let (before, after) = rise(10, 0.3, 0.9);
        sparks.observe(&before, &after);
        draw(&mut sparks, body, &after, 0.0, 6);
        let vy = sparks.sparks[0].vy;
        draw(&mut sparks, body, &after, 0.1, 6);
        assert!((sparks.sparks[0].vy - (vy + gravity(48) * 0.1)).abs() < 1e-3);
        for _ in 0..6 {
            draw(&mut sparks, body, &after, 0.2, 6);
        }
        assert!(!sparks.is_active());
        let p = Theme::default().palette();
        assert_eq!(cooling(&p, 0.0), p.text);
        assert_eq!(cooling(&p, 0.6), p.spectrum[2]);
        assert_ne!(
            cooling(&p, 1.0),
            p.bg,
            "sparks never vanish into the canvas color"
        );
    }

    #[test]
    fn busy_music_stays_within_the_budget_and_the_cap() {
        let body = Rect::new(0, 0, 64, 12);
        let mut sparks = Sparks::default();
        sparks.observe(&[0.3; BANDS], &[1.0; BANDS]);
        draw(&mut sparks, body, &[0.6; BANDS], 0.0, 8);
        // 32 bars allow eight throws of at most six sparks each.
        assert_eq!(sparks.sparks.len(), 48);
        // A full bar leaves no room to throw from.
        let mut full = Sparks::default();
        full.observe(&[0.3; BANDS], &[1.0; BANDS]);
        draw(&mut full, body, &[1.0; BANDS], 0.0, 8);
        assert!(!full.is_active());
        // 16 × 4 cells hold at most 24 sparks, however often the bands jump.
        let small = Rect::new(0, 0, 16, 4);
        let mut sparks = Sparks::default();
        for seed in 0..40 {
            sparks.cooldown = [0.0; BANDS];
            sparks.flux = [0.0; BANDS];
            sparks.observe(&[0.3; BANDS], &[0.5; BANDS]);
            draw(&mut sparks, small, &[0.5; BANDS], 0.0, seed);
        }
        assert_eq!(sparks.sparks.len(), 24);
    }

    #[test]
    fn sparks_only_use_blank_cells_and_short_bodies_throw_none() {
        let body = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(body);
        for x in 0..40 {
            buf[(x, 1)].set_char('█');
        }
        let mut sparks = Sparks::default();
        let (before, after) = rise(5, 0.2, 0.8);
        sparks.observe(&before, &after);
        sparks.draw(
            &mut buf,
            body,
            &Theme::default().palette(),
            &after,
            0.0,
            &mut SmallRng::seed_from_u64(2),
        );
        assert!((0..40).all(|x| buf[(x, 1)].symbol() == "█"));
        let mut short = Sparks::default();
        short.observe(&before, &after);
        draw(&mut short, Rect::new(0, 0, 40, 2), &after, 0.0, 2);
        assert!(!short.is_active());
        // A resize drops sparks placed for the old size.
        draw(&mut sparks, Rect::new(0, 0, 50, 12), &after, 0.0, 2);
        assert!(!sparks.is_active());
    }
}
