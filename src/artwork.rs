//! Terminal graphics selection. Native Sixel belongs to the current pane, so tmux
//! can retain it when switching windows; it must not be sent through passthrough.
use crate::cli::Art;
use image::{DynamicImage, Rgba};
use ratatui_image::{
    FontSize,
    picker::{
        Capability, Picker, ProtocolType,
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
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const FALLBACK_FONT: FontSize = FontSize::new(10, 20);
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) enum Artwork {
    Detected(Picker),
    Native {
        protocol: ProtocolType,
        font_size: FontSize,
        tmux: bool,
        compress: bool,
    },
}

impl Artwork {
    /// Call after entering raw mode, before starting the terminal event reader.
    /// Keep the returned guard alive until the interface detaches.
    pub fn detect(art: Art) -> (Self, Option<TmuxPassthrough>) {
        if matches!(art, Art::None | Art::Halfblocks) {
            return (Self::native(art, false, Capabilities::default()), None);
        }
        let tmux = std::env::var_os("TMUX").is_some()
            || std::env::var("TERM").is_ok_and(|s| s.starts_with("tmux"))
            || std::env::var("TERM_PROGRAM").is_ok_and(|s| s == "tmux");
        let multiplexer = tmux || std::env::var("TERM").is_ok_and(|s| s.starts_with("screen"));
        if multiplexer || matches!(art, Art::Sixel) {
            let mut caps = probe(false).unwrap_or_default();
            if tmux && matches!(art, Art::Auto) {
                // tmux's DA1 describes its parser, not the attached terminal.
                // Without end-to-end support tmux draws a '+' placeholder.
                caps.restrict_to_tmux_clients(&tmux_client_features().unwrap_or_default());
            }
            let mut passthrough = None;
            if tmux
                && (matches!(art, Art::Kitty)
                    || (matches!(art, Art::Auto) && !(caps.sixel && caps.font_size.is_some())))
            {
                passthrough = TmuxPassthrough::enable();
                if let Some(guard) = &passthrough
                    && guard.pane_is_active()
                {
                    // Replies from the outer terminal go to the active pane.
                    // Do not inject query replies into another running program.
                    let outer = probe(true).unwrap_or_default();
                    caps.kitty = outer.kitty;
                    caps.compress = outer.compress;
                    caps.font_size = caps.font_size.or(outer.font_size);
                }
                if matches!(art, Art::Auto) && !(caps.kitty && caps.font_size.is_some()) {
                    passthrough = None; // Restore the pane option on failed detection.
                }
            }
            return (Self::native(art, tmux, caps), passthrough);
        }
        let mut picker = Picker::from_query_stdio_with_options(QueryStdioOptions {
            kitty_compression: true,
            ..Default::default()
        })
        .unwrap_or_else(|_| Picker::halfblocks());
        if matches!(art, Art::Kitty) {
            picker.set_protocol_type(ProtocolType::Kitty);
        }
        (Self::Detected(picker), None)
    }

    fn native(art: Art, tmux: bool, caps: Capabilities) -> Self {
        let protocol = match art {
            Art::Sixel => ProtocolType::Sixel,
            Art::Kitty => ProtocolType::Kitty,
            Art::Auto if caps.sixel && caps.font_size.is_some() => ProtocolType::Sixel,
            Art::Auto if caps.kitty && caps.font_size.is_some() => ProtocolType::Kitty,
            _ => ProtocolType::Halfblocks,
        };
        Self::Native {
            protocol,
            font_size: caps.font_size.unwrap_or(FALLBACK_FONT),
            tmux,
            compress: caps.compress,
        }
    }

    pub fn video_graphics(&self) -> Option<VideoGraphics> {
        let (kind, font, tmux, compress) = match self {
            Self::Detected(p) => (
                p.protocol_type(),
                p.font_size(),
                p.tmux_detected(),
                p.capabilities().contains(&Capability::KittyCompression),
            ),
            Self::Native {
                protocol,
                font_size,
                tmux,
                compress,
            } => (*protocol, *font_size, *tmux, *compress),
        };
        matches!(kind, ProtocolType::Kitty | ProtocolType::Sixel).then_some(VideoGraphics {
            kind,
            font,
            tmux,
            compress,
        })
    }

    /// Cell size of the terminal, as understood by the graphics protocol.
    pub fn font_size(&self) -> FontSize {
        match self {
            Self::Detected(picker) => picker.font_size(),
            Self::Native { font_size, .. } => *font_size,
        }
    }

    pub fn new_resize_protocol(
        &self,
        image: DynamicImage,
        background: Rgba<u8>,
    ) -> StatefulProtocol {
        match self {
            Self::Detected(picker) => {
                let mut picker = picker.clone();
                picker.set_background_color(Some(background));
                picker.new_resize_protocol(image)
            }
            Self::Native {
                protocol,
                font_size,
                tmux,
                ..
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
                StatefulProtocol::new(image, *font_size, Some(background), protocol)
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct VideoGraphics {
    pub kind: ProtocolType,
    pub font: FontSize,
    pub tmux: bool,
    pub compress: bool,
}
impl VideoGraphics {
    pub fn protocol(self, image: DynamicImage, background: Rgba<u8>, id: u32) -> StatefulProtocol {
        let protocol = match self.kind {
            ProtocolType::Kitty => {
                StatefulProtocolType::Kitty(StatefulKitty::new(id, self.tmux, self.compress))
            }
            _ => StatefulProtocolType::Sixel(Sixel::default()),
        };
        StatefulProtocol::new(image, self.font, Some(background), protocol)
    }
    pub fn delete(self, ids: [u32; 2]) -> String {
        if self.kind != ProtocolType::Kitty {
            return String::new();
        }
        ids.into_iter()
            .map(|id| {
                let sequence = format!("\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
                if self.tmux {
                    format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
                } else {
                    sequence
                }
            })
            .collect()
    }
}

#[derive(Default)]
struct Capabilities {
    sixel: bool,
    kitty: bool,
    compress: bool,
    font_size: Option<FontSize>,
}

impl Capabilities {
    fn restrict_to_tmux_clients(&mut self, clients: &str) {
        let mut clients = clients.lines().peekable();
        self.sixel &= clients.peek().is_some()
            && clients.all(|client| {
                let mut fields = client.split('\t');
                let features = fields.next().unwrap_or_default();
                let width = fields.next().and_then(|s| s.parse::<u16>().ok());
                let height = fields.next().and_then(|s| s.parse::<u16>().ok());
                features.split(',').any(|feature| feature == "sixel")
                    && width.is_some_and(|w| w > 0)
                    && height.is_some_and(|h| h > 0)
            });
    }

    /// Return true at the final status reply, leaving subsequent keys unread.
    fn accept(&mut self, response: Response) -> bool {
        match response {
            Response::Sixel => self.sixel = true,
            Response::Kitty => self.kitty = true,
            Response::KittyCompression => self.compress = true,
            Response::CellSize(Some((width, height))) if width > 0 && height > 0 => {
                self.font_size = Some(FontSize::new(width, height));
            }
            Response::Status => return true,
            _ => (),
        }
        false
    }
}

fn tmux_query(args: &[&str]) -> Option<String> {
    let output = Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Kitty image uploads need passthrough; scope it to this pane and this attach.
pub(crate) struct TmuxPassthrough {
    pane: String,
    previous: Option<String>,
    changed: bool,
}

impl TmuxPassthrough {
    /// An outer update is released by tmux's final redraw, so every attached
    /// client must have tmux's synchronized-output capability enabled.
    pub fn synchronized_updates(&self) -> bool {
        tmux_client_features().is_some_and(|clients| {
            !clients.trim().is_empty()
                && clients.lines().all(|client| {
                    client.split('\t').next().is_some_and(|features| {
                        features.split(',').any(|feature| feature == "sync")
                    })
                })
        })
    }

    fn enable() -> Option<Self> {
        let pane = std::env::var("TMUX_PANE").ok()?;
        let local = tmux_query(&["show-options", "-p", "-v", "-t", &pane, "allow-passthrough"])?;
        let effective = tmux_query(&[
            "show-options",
            "-A",
            "-p",
            "-v",
            "-t",
            &pane,
            "allow-passthrough",
        ])?;
        let changed = effective.trim() == "off";
        if changed {
            tmux_query(&["set-option", "-p", "-t", &pane, "allow-passthrough", "on"])?;
        }
        Some(Self {
            pane,
            previous: (!local.trim().is_empty()).then(|| local.trim().to_owned()),
            changed,
        })
    }

    fn pane_is_active(&self) -> bool {
        tmux_query(&["display-message", "-p", "-t", &self.pane, "#{pane_active}"])
            .is_some_and(|s| s.trim() == "1")
    }
}

impl Drop for TmuxPassthrough {
    fn drop(&mut self) {
        if !self.changed {
            return;
        }
        // Preserve a setting the user changed while vtamp was attached.
        if !tmux_query(&[
            "show-options",
            "-p",
            "-v",
            "-t",
            &self.pane,
            "allow-passthrough",
        ])
        .is_some_and(|s| s.trim() == "on")
        {
            return;
        }
        if let Some(previous) = &self.previous {
            tmux_query(&[
                "set-option",
                "-p",
                "-t",
                &self.pane,
                "allow-passthrough",
                previous,
            ]);
        } else {
            tmux_query(&[
                "set-option",
                "-p",
                "-u",
                "-t",
                &self.pane,
                "allow-passthrough",
            ]);
        }
    }
}

fn tmux_client_features() -> Option<String> {
    // Scope to this pane's session: a Sixel client on an unrelated session
    // cannot display our image. All attached clients must be able to render it.
    let pane = std::env::var("TMUX_PANE").ok()?;
    let session = tmux_query(&["display-message", "-p", "-t", &pane, "#{session_id}"])?;
    let session = session.trim();
    if !session.starts_with('$') || session[1..].parse::<u32>().is_err() {
        return None;
    }
    tmux_query(&[
        "list-clients",
        "-t",
        session,
        "-F",
        "#{client_termfeatures}\t#{client_cell_width}\t#{client_cell_height}",
    ])
}

fn probe(tmux_passthrough: bool) -> io::Result<Capabilities> {
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
        tmux_passthrough,
        QueryStdioOptions {
            kitty_compression: true,
            blacklist_protocols: vec![if tmux_passthrough {
                ProtocolType::Sixel
            } else {
                ProtocolType::Kitty
            }],
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
    fn video_compression_requires_a_positive_terminal_reply() {
        for (reply, expected) in [
            ("\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\\x1b[0n", true),
            ("\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;EINVAL\x1b\\\x1b[0n", false),
            ("\x1b_Gi=31;OK\x1b\\\x1b[0n", false),
        ] {
            let artwork = Artwork::native(Art::Kitty, true, capabilities(reply));
            assert_eq!(artwork.video_graphics().unwrap().compress, expected);
        }
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
        let mut protocol = renderer.new_resize_protocol(image, Rgba([0, 0, 0, 255]));
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
                    .new_resize_protocol(DynamicImage::new_rgb8(64, 64), Rgba([0, 0, 0, 255]))
                    .protocol_type(),
                StatefulProtocolType::Halfblocks(_)
            ));
        }
    }

    #[test]
    fn kitty_reply_selects_pixel_graphics_through_tmux() {
        let caps = capabilities("\x1b_Gi=31;OK\x1b\\\x1b[6;34;17t\x1b[0n");
        let renderer = Artwork::native(Art::Auto, true, caps);
        let mut protocol =
            renderer.new_resize_protocol(DynamicImage::new_rgb8(512, 512), Rgba([0, 0, 0, 255]));
        let area = Rect::new(0, 0, 18, 9);
        protocol.resize_encode(&Resize::Fit(None), area.as_size());
        protocol.last_encoding_result().unwrap().unwrap();
        let mut buffer = Buffer::empty(area);
        protocol.render(area, &mut buffer);
        let upload = buffer[(0, 0)].symbol();
        assert!(upload.starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(upload.contains("a=T,U=1,f=32,t=d,s=306,v=306,"));
        assert!(upload.contains('\u{10eeee}'));
        // Subsequent frames use placeholders without uploading pixels again.
        protocol.render(area, &mut buffer);
        assert!(!buffer[(0, 0)].symbol().contains("\x1b_G"));
        assert!(buffer[(0, 0)].symbol().contains('\u{10eeee}'));
    }

    #[test]
    fn failed_kitty_detection_falls_back_and_native_sixel_takes_priority() {
        for reply in [
            "\x1b_Gi=31;ENOTSUP\x1b\\\x1b[6;34;17t\x1b[0n",
            "\x1b_Gi=31;OK\x1b\\\x1b[0n",
        ] {
            let renderer = Artwork::native(Art::Auto, true, capabilities(reply));
            assert!(matches!(
                renderer
                    .new_resize_protocol(DynamicImage::new_rgb8(64, 64), Rgba([0, 0, 0, 255]))
                    .protocol_type(),
                StatefulProtocolType::Halfblocks(_)
            ));
        }
        let caps = capabilities("\x1b_Gi=31;OK\x1b\\\x1b[?1;2;4c\x1b[6;34;17t\x1b[0n");
        let renderer = Artwork::native(Art::Auto, true, caps);
        assert!(matches!(
            renderer
                .new_resize_protocol(DynamicImage::new_rgb8(64, 64), Rgba([0, 0, 0, 255]))
                .protocol_type(),
            StatefulProtocolType::Sixel(_)
        ));
    }

    #[test]
    fn tmux_parser_support_does_not_imply_client_sixel_support() {
        for (clients, supported) in [
            ("RGB,sixel\t17\t34\n", true),
            ("sixel,RGB\t17\t34\nsixel\t10\t20\n", true),
            ("RGB,clipboard\t17\t34\n", false),
            ("RGB,sixel\t17\t34\nRGB,clipboard\t17\t34\n", false),
            ("RGB,nosixel\t17\t34\n", false),
            ("RGB,sixel\t0\t0\n", false),
            ("RGB,sixel\t17\t\n", false),
            ("", false),
            ("\n", false),
        ] {
            let mut caps = capabilities("\x1b[?1;2;4c\x1b[6;34;17t\x1b[0n");
            caps.restrict_to_tmux_clients(clients);
            let renderer = Artwork::native(Art::Auto, true, caps);
            assert_eq!(
                matches!(
                    renderer
                        .new_resize_protocol(DynamicImage::new_rgb8(64, 64), Rgba([0, 0, 0, 255]))
                        .protocol_type(),
                    StatefulProtocolType::Sixel(_)
                ),
                supported,
                "client report: {clients:?}"
            );
        }
        let mut caps = capabilities("\x1b[?1;2c\x1b[6;34;17t\x1b[0n");
        caps.restrict_to_tmux_clients("sixel\t17\t34\n");
        assert!(!caps.sixel, "the tmux parser must also support Sixel");
    }

    #[test]
    fn explicit_modes_override_terminal_capabilities() {
        for art in [Art::Halfblocks, Art::None, Art::Sixel, Art::Kitty] {
            let renderer = Artwork::native(art, true, Capabilities::default());
            let protocol =
                renderer.new_resize_protocol(DynamicImage::new_rgb8(64, 64), Rgba([0, 0, 0, 255]));
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
