//! Doom-style fire on half-block pixels: the bottom pixel row takes the band levels, and
//! every step each pixel takes the heat of a random pixel below it, minus a random loss.
use super::{BANDS, bands, merged};
use crate::theme::{Palette, blend, is_light};
use rand::RngExt;
use ratatui::{buffer::Buffer, layout::Rect, style::Color, symbols::half_block::UPPER};

/// Heat shades; finer steps would mostly resend cells for changes nobody sees.
const STEPS: usize = 16;
/// Heat below this is gone, so a dying fire ends instead of leaving faint specks.
const EMBER: f32 = 1.0 / 64.0;
/// Seconds for heat to climb the whole body, which also bounds how long it takes to
/// burn out once the levels are gone.
const CLIMB: f32 = 0.35;
/// Steps per second on very short and very tall bodies.
const MIN_RATE: f32 = 30.0;
const MAX_RATE: f32 = 150.0;
/// Share of the height a full level reaches on average.
const REACH: f32 = 0.9;
/// Sideways drift per step; the doubled middle keeps each band's flames over its column.
const DRIFT: [isize; 4] = [-1, 0, 0, 1];

#[derive(Default)]
pub(super) struct Fire {
    width: usize,
    rows: usize,
    /// Heat in 0..1 per pixel, top row first; a terminal row holds two pixel rows.
    heat: Vec<f32>,
    /// Elapsed time not yet spent on whole steps, in steps.
    owed: f32,
    hot: bool,
}

impl Fire {
    pub fn reset(&mut self) {
        self.heat.fill(0.0);
        self.owed = 0.0;
        self.hot = false;
    }

    /// Whether heat is left to burn out.
    pub fn is_hot(&self) -> bool {
        self.hot
    }

    pub fn draw(
        &mut self,
        buf: &mut Buffer,
        body: Rect,
        p: &Palette,
        levels: &[f32; BANDS],
        dt: f32,
        rng: &mut impl RngExt,
    ) {
        let (width, rows) = (usize::from(body.width), usize::from(body.height) * 2);
        if (width, rows) != (self.width, self.rows) {
            *self = Self {
                width,
                rows,
                heat: vec![0.0; width * rows],
                ..Self::default()
            };
        }
        if self.heat.is_empty() {
            return;
        }
        self.owed += dt * (rows as f32 / CLIMB).clamp(MIN_RATE, MAX_RATE);
        while self.owed >= 1.0 {
            self.step(levels, rng);
            self.owed -= 1.0;
        }
        self.render(buf, body, p);
    }

    fn step(&mut self, levels: &[f32; BANDS], rng: &mut impl RngExt) {
        let (width, rows) = (self.width, self.rows);
        let source = (rows - 1) * width;
        for (x, heat) in self.heat[source..].iter_mut().enumerate() {
            // A gentle curve lets ordinary levels reach the hotter shades.
            *heat = merged(levels, bands(x, width)).clamp(0.0, 1.0).powf(0.75);
        }
        let mut hot = self.heat[source..].iter().any(|heat| *heat > 0.0);
        // Doom loses all or nothing per pixel, which tears the flames into tongues.
        let loss = 2.0 / (REACH * rows as f32);
        // Top row first, so every row still reads the row below as it was.
        for y in 0..rows - 1 {
            for x in 0..width {
                let from = x
                    .saturating_add_signed(DRIFT[rng.random_range(0..DRIFT.len())])
                    .min(width - 1);
                let mut heat = self.heat[(y + 1) * width + from];
                if rng.random_bool(0.5) {
                    heat -= loss;
                }
                let heat = if heat < EMBER { 0.0 } else { heat };
                hot |= heat > 0.0;
                self.heat[y * width + x] = heat;
            }
        }
        self.hot = hot;
    }

    fn render(&self, buf: &mut Buffer, body: Rect, p: &Palette) {
        let shades: [Color; STEPS + 1] =
            std::array::from_fn(|step| shade(p, step as f32 / STEPS as f32));
        let index = |heat: f32| ((heat * STEPS as f32).ceil() as usize).min(STEPS);
        for row in 0..body.height {
            for column in 0..body.width {
                let upper = usize::from(row) * 2 * self.width + usize::from(column);
                let (top, bottom) = (
                    index(self.heat[upper]),
                    index(self.heat[upper + self.width]),
                );
                let cell = &mut buf[(body.x + column, body.y + row)];
                if top == 0 && bottom == 0 {
                    cell.set_char(' ').set_fg(p.bg).set_bg(p.bg);
                } else {
                    cell.set_char(UPPER)
                        .set_fg(shades[top])
                        .set_bg(shades[bottom]);
                }
            }
        }
    }
}

/// The canvas when cold. Dark themes then pass the high and middle roles to the text role
/// at full heat, a red-to-white flame; light themes run from the middle role to the high
/// role, so the hottest flame is the strongest ink.
fn shade(p: &Palette, heat: f32) -> Color {
    if is_light(p) {
        if heat <= 0.4 {
            blend(p.bg, p.spectrum[1], heat / 0.4)
        } else {
            blend(p.spectrum[1], p.spectrum[2], (heat - 0.4) / 0.6)
        }
    } else if heat <= 0.35 {
        blend(p.bg, p.spectrum[2], heat / 0.35)
    } else if heat <= 0.75 {
        blend(p.spectrum[2], p.spectrum[1], (heat - 0.35) / 0.4)
    } else {
        blend(p.spectrum[1], p.text, (heat - 0.75) / 0.25)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use rand::{SeedableRng, rngs::SmallRng};

    fn fire(width: u16, height: u16) -> (Fire, Rect) {
        let area = Rect::new(0, 0, width, height);
        let mut fire = Fire::default();
        let mut buf = Buffer::empty(area);
        // A zero step sizes the grid without drawing heat.
        fire.draw(
            &mut buf,
            area,
            &Theme::default().palette(),
            &[0.0; BANDS],
            0.0,
            &mut SmallRng::seed_from_u64(1),
        );
        (fire, area)
    }

    fn row_mean(fire: &Fire, y: usize) -> f32 {
        fire.heat[y * fire.width..(y + 1) * fire.width]
            .iter()
            .sum::<f32>()
            / fire.width as f32
    }

    #[test]
    fn heat_rises_from_the_levels_and_cools_with_height() {
        let (mut fire, _) = fire(60, 8);
        let mut rng = SmallRng::seed_from_u64(7);
        let mut means = [0.0; 16];
        for _ in 0..80 {
            fire.step(&[0.9; BANDS], &mut rng);
            for (y, mean) in means.iter_mut().enumerate() {
                *mean += row_mean(&fire, y);
            }
        }
        assert!(fire.is_hot());
        assert!(means[15] > means[11] && means[11] > means[7] && means[7] > means[3]);
        assert!(
            means[2] > 0.0,
            "a loud fire reaches the top quarter: {means:?}"
        );
    }

    #[test]
    fn low_bands_burn_only_on_the_left() {
        let (mut fire, _) = fire(80, 6);
        let mut rng = SmallRng::seed_from_u64(3);
        let levels: [f32; BANDS] = std::array::from_fn(|band| if band < 16 { 1.0 } else { 0.0 });
        for _ in 0..60 {
            fire.step(&levels, &mut rng);
        }
        for y in 0..fire.rows {
            for x in 52..80 {
                assert_eq!(fire.heat[y * 80 + x], 0.0, "pixel {x},{y}");
            }
        }
        assert!(row_mean(&fire, 6) > 0.0);
    }

    #[test]
    fn cold_levels_burn_out_within_one_climb() {
        let (mut fire, area) = fire(40, 6);
        let mut rng = SmallRng::seed_from_u64(9);
        for _ in 0..30 {
            fire.step(&[1.0; BANDS], &mut rng);
        }
        for _ in 0..fire.rows {
            fire.step(&[0.0; BANDS], &mut rng);
        }
        assert!(!fire.is_hot());
        assert!(fire.heat.iter().all(|heat| *heat == 0.0));
        let p = Theme::default().palette();
        let mut buf = Buffer::empty(area);
        fire.render(&mut buf, area, &p);
        assert!(
            buf.content()
                .iter()
                .all(|cell| cell.symbol() == " " && cell.fg == p.bg && cell.bg == p.bg)
        );
    }

    #[test]
    fn colors_come_from_the_heat_shades_and_full_heat_is_the_text_role() {
        let p = Theme::default().palette();
        let shades: Vec<Color> = (0..=STEPS)
            .map(|step| shade(&p, step as f32 / STEPS as f32))
            .collect();
        assert_eq!(shades[0], p.bg);
        assert_eq!(shades[STEPS], p.text);
        let latte = Theme::CatppuccinLatte.palette();
        assert_eq!(shade(&latte, 0.0), latte.bg);
        assert_eq!(shade(&latte, 0.4), latte.spectrum[1]);
        assert_eq!(shade(&latte, 1.0), latte.spectrum[2]);
        let (mut fire, area) = fire(40, 6);
        let mut rng = SmallRng::seed_from_u64(5);
        for _ in 0..20 {
            fire.step(&[1.0; BANDS], &mut rng);
        }
        let mut buf = Buffer::empty(area);
        fire.render(&mut buf, area, &p);
        for cell in buf.content() {
            assert!(cell.symbol() == "▀" || cell.symbol() == " ");
            assert!(shades.contains(&cell.fg) && shades.contains(&cell.bg));
        }
        // The source row is the lower pixel of the last terminal row.
        assert!((0..40).all(|x| buf[(x, 5)].bg == p.text));
    }

    #[test]
    fn identical_seeds_draw_identical_fires_and_resizing_starts_cold() {
        let p = Theme::default().palette();
        let levels: [f32; BANDS] = std::array::from_fn(|band| band as f32 / 31.0);
        let draw = |seed| {
            let (mut fire, area) = fire(50, 7);
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut buf = Buffer::empty(area);
            for _ in 0..10 {
                fire.draw(&mut buf, area, &p, &levels, 0.05, &mut rng);
            }
            (fire, buf)
        };
        let (mut fire, first) = draw(11);
        assert_eq!(first, draw(11).1);
        assert!(fire.is_hot());
        let mut rng = SmallRng::seed_from_u64(1);
        for (width, height) in [(61, 12), (40, 1), (1, 1), (5, 0), (0, 3)] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            fire.draw(&mut buf, area, &p, &levels, 0.0, &mut rng);
            assert!(!fire.is_hot(), "{width}×{height} starts cold");
            fire.draw(&mut buf, area, &p, &levels, 0.2, &mut rng);
        }
    }
}
