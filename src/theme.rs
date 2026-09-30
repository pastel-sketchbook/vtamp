//! Built-in palettes. Role assignments are tuned for terminal text contrast.
//! Palette origins and variant names are recorded in docs/themes.md.
use clap::ValueEnum;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    CatppuccinMocha,
    CatppuccinLatte,
    RosePine,
    Gruvbox,
    TokyoNight,
    Nord,
    Dracula,
    Kanagawa,
    Classic,
}

impl Theme {
    pub const ALL: [Self; 9] = [
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::RosePine,
        Self::Gruvbox,
        Self::TokyoNight,
        Self::Nord,
        Self::Dracula,
        Self::Kanagawa,
        Self::Classic,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::CatppuccinLatte => "catppuccin-latte",
            Self::RosePine => "rose-pine",
            Self::Gruvbox => "gruvbox",
            Self::TokyoNight => "tokyo-night",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
            Self::Kanagawa => "kanagawa",
            Self::Classic => "classic",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::RosePine => "Rosé Pine",
            Self::Gruvbox => "Gruvbox",
            Self::TokyoNight => "Tokyo Night",
            Self::Nord => "Nord",
            Self::Dracula => "Dracula",
            Self::Kanagawa => "Kanagawa",
            Self::Classic => "Classic",
        }
    }

    pub fn mode(self) -> &'static str {
        if self == Self::CatppuccinLatte {
            "light"
        } else {
            "dark"
        }
    }

    pub fn palette(self) -> Palette {
        // canvas, panel, selection, text, muted, accent, border, warning, error
        let colors = match self {
            Self::CatppuccinMocha => [
                0x1e1e2e, 0x181825, 0x313244, 0xcdd6f4, 0xbac2de, 0xfab387, 0x585b70, 0xf9e2af,
                0xf38ba8,
            ],
            Self::CatppuccinLatte => [
                0xeff1f5, 0xe6e9ef, 0xccd0da, 0x4c4f69, 0x4c4f69, 0x8434eb, 0x9ca0b0, 0x7b4c0a,
                0xb51030,
            ],
            Self::RosePine => [
                0x191724, 0x1f1d2e, 0x26233a, 0xe0def4, 0x908caa, 0xebbcba, 0x524f67, 0xf6c177,
                0xeb6f92,
            ],
            Self::Gruvbox => [
                0x282828, 0x32302f, 0x3c3836, 0xebdbb2, 0xbdae93, 0xfe8019, 0x665c54, 0xfabd2f,
                0xff7a6b,
            ],
            Self::TokyoNight => [
                0x1a1b26, 0x16161e, 0x292e42, 0xc0caf5, 0xa9b1d6, 0x7aa2f7, 0x565f89, 0xe0af68,
                0xf7768e,
            ],
            Self::Nord => [
                0x2e3440, 0x3b4252, 0x434c5e, 0xeceff4, 0xd8dee9, 0x88c0d0, 0x4c566a, 0xebcb8b,
                0xe6a0a8,
            ],
            Self::Dracula => [
                0x282a36, 0x21222c, 0x44475a, 0xf8f8f2, 0xd7d7df, 0xbd93f9, 0x6272a4, 0xf1fa8c,
                0xff9090,
            ],
            Self::Kanagawa => [
                0x1f1f28, 0x16161d, 0x2d4f67, 0xdcd7ba, 0xc8c093, 0x7e9cd8, 0x54546d, 0xe6c384,
                0xff5d62,
            ],
            Self::Classic => [
                0x181c19, 0x202521, 0x30392e, 0xeeeae0, 0xa4b19b, 0xb4f676, 0x4d5848, 0xefbc72,
                0xff9090,
            ],
        };
        let [
            bg,
            panel,
            selection,
            text,
            muted,
            accent,
            border,
            warning,
            error,
        ] = colors.map(rgb);
        Palette {
            bg,
            panel,
            selection,
            text,
            muted,
            accent,
            border,
            warning,
            error,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color,
    pub panel: Color,
    pub selection: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub border: Color,
    pub warning: Color,
    pub error: Color,
}

fn rgb(value: u32) -> Color {
    Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

pub(crate) fn channels(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => unreachable!("Theme colors are RGB"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color) -> f64 {
        let [r, g, b] = channels(color).map(|v| {
            let s = f64::from(v) / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
    fn contrast(a: Color, b: Color) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn readable_text_and_focus_in_every_theme() {
        for theme in Theme::ALL {
            let p = theme.palette();
            for bg in [p.bg, p.panel, p.selection] {
                for fg in [p.text, p.muted] {
                    assert!(
                        contrast(fg, bg) >= 4.5,
                        "{} text: {:?}/{:?}: {}",
                        theme.id(),
                        fg,
                        bg,
                        contrast(fg, bg)
                    );
                }
            }
            for fg in [p.accent, p.warning, p.error] {
                assert!(
                    contrast(fg, p.bg) >= 4.5,
                    "{} status: {:?}: {}",
                    theme.id(),
                    fg,
                    contrast(fg, p.bg)
                );
                assert!(
                    contrast(fg, p.panel) >= 4.5,
                    "{} popup text: {:?}: {}",
                    theme.id(),
                    fg,
                    contrast(fg, p.panel)
                );
            }
            assert!(contrast(p.accent, p.bg) >= 3.0);
        }
    }

    #[test]
    fn cli_and_settings_names_match() {
        for theme in Theme::ALL {
            assert_eq!(theme.to_possible_value().unwrap().get_name(), theme.id());
            assert_eq!(serde_json::to_value(theme).unwrap(), theme.id());
        }
    }
}
