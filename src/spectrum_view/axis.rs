//! Frequency labels positioned against the actual graph geometry.
use super::{BANDS, bar_layout};
use crate::{spectrum::SpectrumFrame, theme::Palette};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

#[derive(Clone, Copy)]
pub(super) enum Mapping {
    Bars,
    Continuous,
    Ends,
}

fn position(graph: Rect, mapping: Mapping, ratio: f32) -> u16 {
    let offset = match mapping {
        Mapping::Bars => {
            let band = ((ratio * BANDS as f32) as usize).min(BANDS - 1);
            bar_layout(graph.width)
                .find(|bar| bar.bands.contains(&band))
                .map_or(0, |bar| (bar.columns.start + bar.columns.end - 1) / 2)
        }
        _ => (ratio * f32::from(graph.width) - 0.5)
            .round()
            .clamp(0.0, f32::from(graph.width.saturating_sub(1))) as u16,
    };
    graph.x + offset
}

pub(super) fn draw(
    buf: &mut Buffer,
    graph: Rect,
    y: u16,
    p: &Palette,
    mapping: Mapping,
    frame: Option<&SpectrumFrame>,
) {
    if graph.width == 0 {
        return;
    }
    let style = Style::default().fg(p.muted).bg(p.bg);
    let scale = frame.filter(|f| {
        f.low_hz.is_finite() && f.high_hz.is_finite() && f.low_hz > 0.0 && f.high_hz > f.low_hz
    });
    let mut end = None;
    if graph.width >= 12
        && !matches!(mapping, Mapping::Ends)
        && let Some(frame) = scale
    {
        let span = (frame.high_hz / frame.low_hz).ln();
        for (hz, label) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            if hz < frame.low_hz || hz > frame.high_hz {
                continue;
            }
            let ratio = (hz / frame.low_hz).ln() / span;
            let width = label.len() as u16;
            let x = position(graph, mapping, ratio)
                .saturating_sub(width / 2)
                .clamp(graph.x, graph.right() - width);
            if end.is_some_and(|end| x <= end) {
                continue;
            }
            buf.set_string(x, y, label, style);
            end = Some(x + width);
        }
    }
    if end.is_none() {
        buf.set_stringn(graph.x, y, "LOW", usize::from(graph.width), style);
        if graph.width >= 9 {
            buf.set_string(graph.right() - 4, y, "HIGH", style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn render(width: u16, mapping: Mapping, low: f32, high: f32) -> String {
        let mut buf = Buffer::empty(Rect::new(0, 0, width + 3, 2));
        draw(
            &mut buf,
            Rect::new(3, 0, width, 1),
            1,
            &Theme::default().palette(),
            mapping,
            Some(&SpectrumFrame {
                low_hz: low,
                high_hz: high,
                ..SpectrumFrame::default()
            }),
        );
        (0..width + 3).map(|x| buf[(x, 1)].symbol()).collect()
    }

    #[test]
    fn labels_follow_scale_and_keep_a_blank_between_them() {
        let row = render(60, Mapping::Bars, 40.0, 16_000.0);
        assert!(row.contains("100") && row.contains("1k") && row.contains("10k"));
        for width in 12..100 {
            let row = render(width, Mapping::Continuous, 40.0, 16_000.0);
            assert!(
                row.split_whitespace()
                    .all(|word| ["100", "1k", "10k"].contains(&word)),
                "{row}"
            );
        }
        assert!(!render(60, Mapping::Continuous, 40.0, 8000.0).contains("10k"));
        for (width, mapping, low, high) in [
            (11, Mapping::Bars, 40.0, 16_000.0),
            (60, Mapping::Ends, 40.0, 16_000.0),
            (60, Mapping::Bars, 0.0, 0.0),
            (60, Mapping::Bars, f32::NAN, 16_000.0),
            (60, Mapping::Bars, 1000.0, 40.0),
        ] {
            let row = render(width, mapping, low, high);
            assert!(row.contains("LOW") && row.contains("HIGH"), "{row}");
        }
    }

    #[test]
    fn bar_marks_snap_to_the_group_containing_the_frequency() {
        for width in [12, 21, 64, 150] {
            let graph = Rect::new(5, 0, width, 1);
            for bar in bar_layout(width) {
                let band_center = (bar.bands.start + bar.bands.end) as f32 / 2.0;
                let x = position(graph, Mapping::Bars, band_center / BANDS as f32);
                assert!(bar.columns.contains(&(x - graph.x)));
            }
        }
    }
}
