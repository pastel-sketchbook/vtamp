//! Terminal graphics selection. Native Sixel belongs to the current pane, so tmux
//! can retain it when switching windows; it must not be sent through passthrough.
use crate::cli::Art;
use image::{DynamicImage, Rgba};
use ratatui_image::{
    FontSize,
    picker::{
        Picker, ProtocolType,
        cap_parser::{Parser, QueryStdioOptions, Response},
    },
    protocol::{
        StatefulProtocol, StatefulProtocolType, halfblocks::Halfblocks, kitty::StatefulKitty,
        sixel::Sixel,
    },
};
use std::{
    io::{self, IsTerminal, Write},
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

const BACKGROUND: Option<Rgba<u8>> = Some(Rgba([32, 37, 33, 255]));
const FALLBACK_FONT: FontSize = FontSize::new(10, 20);
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) enum Artwork {
    Detected(Picker),
    Native {
        protocol: ProtocolType,
        font_size: FontSize,
        tmux: bool,
    },
}

impl Artwork {
    /// Call after entering raw mode, before starting the terminal event reader.
    pub fn detect(art: Art) -> Self {
        if matches!(art, Art::None | Art::Halfblocks) {
            return Self::native(art, false, Capabilities::default());
        }
        let tmux = std::env::var_os("TMUX").is_some()
            || std::env::var("TERM").is_ok_and(|s| s.starts_with("tmux"))
            || std::env::var("TERM_PROGRAM").is_ok_and(|s| s == "tmux");
        let multiplexer = tmux || std::env::var("TERM").is_ok_and(|s| s.starts_with("screen"));
        if multiplexer || matches!(art, Art::Sixel) {
            // Picker constructors enable tmux passthrough as a side effect. Probe
            // the pane directly instead, leaving the user's tmux options alone.
            return Self::native(art, tmux, probe().unwrap_or_default());
        }
        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        if matches!(art, Art::Kitty) {
            picker.set_protocol_type(ProtocolType::Kitty);
        }
        picker.set_background_color(BACKGROUND);
        Self::Detected(picker)
    }

    fn native(art: Art, tmux: bool, caps: Capabilities) -> Self {
        let protocol = match art {
            Art::Sixel => ProtocolType::Sixel,
            Art::Kitty => ProtocolType::Kitty,
            Art::Auto if caps.sixel && caps.font_size.is_some() => ProtocolType::Sixel,
            _ => ProtocolType::Halfblocks,
        };
        Self::Native {
            protocol,
            font_size: caps.font_size.unwrap_or(FALLBACK_FONT),
            tmux,
        }
    }

    pub fn new_resize_protocol(&self, image: DynamicImage) -> StatefulProtocol {
        match self {
            Self::Detected(picker) => picker.new_resize_protocol(image),
            Self::Native {
                protocol,
                font_size,
                tmux,
            } => {
                let protocol = match protocol {
                    // Always unwrapped: tmux's own Sixel support owns the image.
                    ProtocolType::Sixel => StatefulProtocolType::Sixel(Sixel::default()),
                    ProtocolType::Kitty => StatefulProtocolType::Kitty(StatefulKitty::new(
                        rand::random(),
                        *tmux,
                        false,
                    )),
                    _ => StatefulProtocolType::Halfblocks(Halfblocks::default()),
                };
                StatefulProtocol::new(image, *font_size, BACKGROUND, protocol)
            }
        }
    }
}

#[derive(Default)]
struct Capabilities {
    sixel: bool,
    font_size: Option<FontSize>,
}

impl Capabilities {
    /// Return true at the final status reply, leaving subsequent keys unread.
    fn accept(&mut self, response: Response) -> bool {
        match response {
            Response::Sixel => self.sixel = true,
            Response::CellSize(Some((width, height))) if width > 0 && height > 0 => {
                self.font_size = Some(FontSize::new(width, height));
            }
            Response::Status => return true,
            _ => (),
        }
        false
    }
}

fn probe() -> io::Result<Capabilities> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    if !stdin.is_terminal() || !stdout.is_terminal() {
        return Ok(Capabilities::default());
    }
    let mut caps = Capabilities::default();
    if let Ok(size) = crossterm::terminal::window_size()
        && size.columns > 0
        && size.rows > 0
    {
        let width = size.width / size.columns;
        let height = size.height / size.rows;
        if width > 0 && height > 0 {
            caps.font_size = Some(FontSize::new(width, height));
        }
    }
    let query = Parser::query(
        false,
        QueryStdioOptions {
            blacklist_protocols: vec![ProtocolType::Kitty],
            ..Default::default()
        },
    );
    stdout.write_all(query.as_bytes())?;
    stdout.flush()?;

    // Do not leave a blocking reader thread behind after a timeout: it could
    // steal the first keypress from crossterm on terminals that don't answer.
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut parser = Parser::new();
    let mut fd = libc::pollfd {
        fd: stdin.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Bound both time and input size, including malformed replies.
    for _ in 0..1024 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        // SAFETY: fd is a valid pollfd and stdin stays open throughout the probe.
        let ready = unsafe { libc::poll(&mut fd, 1, remaining.as_millis().max(1) as i32) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 || fd.revents & libc::POLLIN == 0 {
            break;
        }
        let mut byte = 0_u8;
        // SAFETY: byte is writable for exactly one byte. Only this thread reads
        // stdin during startup; poll has confirmed that input is available.
        let read = unsafe { libc::read(fd.fd, (&mut byte as *mut u8).cast(), 1) };
        if read <= 0 {
            break;
        }
        for response in parser.push(char::from(byte)) {
            if caps.accept(response) {
                return Ok(caps);
            }
        }
    }
    Ok(caps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect};
    use ratatui_image::{Resize, ResizeEncodeRender};

    fn capabilities(reply: &str) -> Capabilities {
        let mut caps = Capabilities::default();
        let mut parser = Parser::new();
        for ch in reply.chars() {
            for response in parser.push(ch) {
                if caps.accept(response) {
                    return caps;
                }
            }
        }
        caps
    }

    #[test]
    fn native_tmux_sixel_uses_reported_pixels_without_passthrough() {
        let caps = capabilities("\x1b[?1;2;4c\x1b[6;34;17t\x1b[0n");
        let renderer = Artwork::native(Art::Auto, true, caps);
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            512,
            512,
            image::Rgb([180, 246, 118]),
        ));
        let mut protocol = renderer.new_resize_protocol(image);
        for area in [Rect::new(0, 0, 18, 9), Rect::new(0, 0, 12, 6)] {
            protocol.resize_encode(&Resize::Fit(None), area.as_size());
            protocol.last_encoding_result().unwrap().unwrap();
            let StatefulProtocolType::Sixel(sixel) = protocol.protocol_type() else {
                panic!("Sixel was not selected");
            };
            let pixels = area.height * 34;
            assert!(sixel.data.contains(&format!("\"1;1;{pixels};{pixels}")));
            assert!(!sixel.is_tmux);
            assert!(!sixel.data.contains("\x1bPtmux;"));
            let mut buffer = Buffer::empty(area);
            protocol.render(area, &mut buffer);
            assert!(buffer[(0, 0)].symbol().contains("\x1bP"));
        }
    }

    #[test]
    fn automatic_selection_requires_sixel_and_valid_cell_dimensions() {
        for reply in [
            "",
            "\x1b[?1;2c\x1b[6;34;17t\x1b[0n",
            "\x1b[?1;2;4c\x1b[0n",
            "\x1b[?1;2;4c\x1b[6;0;0t\x1b[0n",
            "\x1b[?1;2;4c\x1b[6;999999;17t\x1b[0n",
        ] {
            let renderer = Artwork::native(Art::Auto, true, capabilities(reply));
            assert!(matches!(
                renderer
                    .new_resize_protocol(DynamicImage::new_rgb8(64, 64))
                    .protocol_type(),
                StatefulProtocolType::Halfblocks(_)
            ));
        }
    }

    #[test]
    fn explicit_modes_override_terminal_capabilities() {
        for art in [Art::Halfblocks, Art::None, Art::Sixel, Art::Kitty] {
            let renderer = Artwork::native(art, true, Capabilities::default());
            let protocol = renderer.new_resize_protocol(DynamicImage::new_rgb8(64, 64));
            assert!(matches!(
                (art, protocol.protocol_type()),
                (
                    Art::Halfblocks | Art::None,
                    StatefulProtocolType::Halfblocks(_)
                ) | (Art::Sixel, StatefulProtocolType::Sixel(_))
                    | (Art::Kitty, StatefulProtocolType::Kitty(_))
            ));
        }
    }
}
