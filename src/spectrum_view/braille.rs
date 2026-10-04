//! Braille dot canvas: 2 × 4 dots per terminal cell, one foreground color per cell.
use crate::theme::{Palette, blend};
use ratatui::{buffer::Buffer, layout::Rect, style::Color, symbols::braille::BRAILLE};

/// Dots over the cells of an area. A cell shows a single color, so every dot carries a
/// rank and the lowest rank drawn into a cell chooses it; equal ranks keep the first.
/// A cell may also glow: its background blends toward a color, the strongest tint winning.
pub(super) struct Braille {
    area: Rect,
    cells: Vec<Cell>,
}

#[derive(Clone, Copy)]
struct Cell {
    bits: u8,
    rank: u32,
    color: Color,
    glow: Option<(Color, f32)>,
}

impl Braille {
    pub fn new(area: Rect) -> Self {
        let blank = Cell {
            bits: 0,
            rank: u32::MAX,
            color: Color::Reset,
            glow: None,
        };
        Self {
            area,
            cells: vec![blank; usize::from(area.width) * usize::from(area.height)],
        }
    }

    /// Canvas size in dots.
    pub fn size(&self) -> (i32, i32) {
        (
            i32::from(self.area.width) * 2,
            i32::from(self.area.height) * 4,
        )
    }

    /// Lights one dot; dots outside the canvas are ignored.
    pub fn dot(&mut self, x: i32, y: i32, color: Color, rank: u32) {
        let Some(cell) = self.cell(x, y) else {
            return;
        };
        // ratatui's table is indexed by the dot bits in row-major order.
        cell.bits |= 1 << (y % 4 * 2 + x % 2);
        if rank < cell.rank {
            cell.rank = rank;
            cell.color = color;
        }
    }

    /// Tints the background of the cell under a dot toward `color` by `strength`.
    pub fn glow(&mut self, x: i32, y: i32, color: Color, strength: f32) {
        if let Some(cell) = self.cell(x, y)
            && cell.glow.is_none_or(|(_, current)| strength > current)
        {
            cell.glow = Some((color, strength));
        }
    }

    fn cell(&mut self, x: i32, y: i32) -> Option<&mut Cell> {
        let (width, height) = self.size();
        if !(0..width).contains(&x) || !(0..height).contains(&y) {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        Some(&mut self.cells[y / 4 * usize::from(self.area.width) + x / 2])
    }

    /// Writes every cell; blank cells without a glow take one constant style.
    pub fn render(&self, buf: &mut Buffer, p: &Palette) {
        for (index, cell) in self.cells.iter().enumerate() {
            let target = &mut buf[self.position(index)];
            let bg = cell
                .glow
                .map_or(p.bg, |(color, strength)| blend(p.bg, color, strength));
            if cell.bits == 0 {
                target.set_char(' ').set_fg(bg).set_bg(bg);
            } else {
                target
                    .set_char(BRAILLE[usize::from(cell.bits)])
                    .set_fg(cell.color)
                    .set_bg(bg);
            }
        }
    }

    /// Writes lit cells only where the buffer is still blank, so other glyphs stay.
    pub fn overlay(&self, buf: &mut Buffer, p: &Palette) {
        for (index, cell) in self.cells.iter().enumerate() {
            let target = &mut buf[self.position(index)];
            if cell.bits != 0 && target.symbol() == " " {
                target
                    .set_char(BRAILLE[usize::from(cell.bits)])
                    .set_fg(cell.color)
                    .set_bg(p.bg);
            }
        }
    }

    fn position(&self, index: usize) -> (u16, u16) {
        let width = usize::from(self.area.width);
        (
            self.area.x + (index % width) as u16,
            self.area.y + (index / width) as u16,
        )
    }
}

/// Dots of a rendered braille glyph as (column, row) pairs within its cell.
#[cfg(test)]
pub(super) fn dots_of(symbol: &str) -> Vec<(u16, u16)> {
    let Some(bits) = symbol
        .chars()
        .next()
        .and_then(|glyph| BRAILLE.iter().position(|c| *c == glyph))
    else {
        return Vec::new();
    };
    (0..8u16)
        .filter(|bit| bits & (1 << bit) != 0)
        .map(|bit| (bit % 2, bit / 2))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn dots_map_to_their_braille_glyphs() {
        // Unicode numbers braille dots down the left column, then the right, then row 4.
        let expected = [
            ((0, 0), '⠁'),
            ((0, 1), '⠂'),
            ((0, 2), '⠄'),
            ((1, 0), '⠈'),
            ((1, 1), '⠐'),
            ((1, 2), '⠠'),
            ((0, 3), '⡀'),
            ((1, 3), '⢀'),
        ];
        let p = Theme::default().palette();
        for ((x, y), glyph) in expected {
            let area = Rect::new(2, 1, 1, 1);
            let mut canvas = Braille::new(area);
            canvas.dot(x, y, p.accent, 0);
            let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
            canvas.render(&mut buf, &p);
            assert_eq!(buf[(2, 1)].symbol(), glyph.to_string(), "dot {x},{y}");
            assert_eq!(dots_of(buf[(2, 1)].symbol()), vec![(x as u16, y as u16)]);
        }
    }

    #[test]
    fn lower_ranks_choose_the_color_and_stray_dots_are_ignored() {
        let p = Theme::default().palette();
        let mut canvas = Braille::new(Rect::new(0, 0, 2, 1));
        for (x, y) in [(-1, 0), (0, -1), (4, 0), (0, 4)] {
            canvas.dot(x, y, p.error, 0);
        }
        canvas.dot(0, 0, p.text, 5);
        canvas.dot(1, 3, p.accent, 2);
        canvas.dot(0, 1, p.warning, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        canvas.render(&mut buf, &p);
        assert_eq!(buf[(0, 0)].symbol(), "⢃");
        assert_eq!(
            buf[(0, 0)].fg,
            p.accent,
            "rank 2 wins; the first of equals stays"
        );
        assert_eq!(buf[(0, 0)].bg, p.bg);
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_eq!((buf[(1, 0)].fg, buf[(1, 0)].bg), (p.bg, p.bg));
    }

    #[test]
    fn the_strongest_glow_tints_a_cell_background() {
        let p = Theme::default().palette();
        let mut canvas = Braille::new(Rect::new(0, 0, 2, 1));
        canvas.dot(0, 0, p.text, 0);
        canvas.glow(0, 0, p.accent, 0.1);
        canvas.glow(1, 2, p.spectrum[2], 0.3);
        canvas.glow(0, 3, p.text, 0.2);
        canvas.glow(9, 0, p.text, 1.0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        canvas.render(&mut buf, &p);
        assert_eq!(buf[(0, 0)].fg, p.text);
        assert_eq!(buf[(0, 0)].bg, blend(p.bg, p.spectrum[2], 0.3));
        assert_eq!(
            (buf[(1, 0)].fg, buf[(1, 0)].bg),
            (p.bg, p.bg),
            "no glow, no tint"
        );
    }

    #[test]
    fn overlay_fills_only_blank_cells() {
        let p = Theme::default().palette();
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf[(0, 0)].set_char('█').set_fg(p.spectrum[0]);
        let mut canvas = Braille::new(Rect::new(0, 0, 3, 1));
        canvas.dot(0, 0, p.text, 0);
        canvas.dot(4, 0, p.text, 0);
        canvas.overlay(&mut buf, &p);
        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(0, 0)].fg, p.spectrum[0]);
        assert_eq!(buf[(1, 0)].symbol(), " ", "unlit cells are left alone");
        assert_eq!(buf[(1, 0)].fg, Color::Reset);
        assert_eq!(buf[(2, 0)].symbol(), "⠁");
        assert_eq!(buf[(2, 0)].fg, p.text);
    }
}
