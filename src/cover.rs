//! Prepare the next image off-thread while the current image remains visible.
#[cfg(test)]
use image::Rgba;
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
};
use ratatui_image::{Resize, ResizeEncodeRender, protocol::StatefulProtocol};
use std::sync::mpsc::Sender;

pub(crate) struct ResizeRequest {
    protocol: StatefulProtocol,
    resize: Resize,
    size: Size,
    generation: u64,
}

pub(crate) struct ResizeResponse {
    protocol: Option<StatefulProtocol>,
    generation: u64,
}

impl ResizeRequest {
    pub fn resize_encode(mut self) -> ResizeResponse {
        self.protocol.resize_encode(&self.resize, self.size);
        let success = matches!(self.protocol.last_encoding_result(), Some(Ok(())));
        ResizeResponse {
            protocol: success.then_some(self.protocol),
            generation: self.generation,
        }
    }
}

/// The player fills its cover rect. The rect is sized to the image's own
/// aspect ratio, so scaling up (which `Resize::Fit` refuses to do) only fills
/// the space instead of distorting the artwork.
pub(crate) const COVER_RESIZE: Resize = Resize::Scale(None);

pub(crate) struct Cover {
    visible: Option<StatefulProtocol>,
    pending: Option<StatefulProtocol>,
    encoding: bool,
    generation: u64,
    tx: Sender<ResizeRequest>,
}

impl Cover {
    pub fn new(tx: Sender<ResizeRequest>, pending: Option<StatefulProtocol>) -> Self {
        Self {
            visible: None,
            pending,
            encoding: false,
            generation: 0,
            tx,
        }
    }

    /// Invalidate old work immediately, including while the new file is decoding.
    pub fn retain_visible(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.encoding = false;
    }

    pub fn replace_protocol(&mut self, protocol: StatefulProtocol) {
        self.retain_visible();
        self.pending = Some(protocol);
    }

    pub fn empty_protocol(&mut self) {
        self.retain_visible();
        self.visible = None;
    }

    pub fn has_image(&self) -> bool {
        self.visible.is_some() || self.pending.is_some() || self.encoding
    }

    #[cfg(test)]
    pub fn background_color(&self) -> Option<Rgba<u8>> {
        self.pending
            .as_ref()
            .or(self.visible.as_ref())
            .and_then(|p| p.background_color())
    }

    /// Size the current cover would occupy in `area` under `resize`, used by
    /// tests to prove a wide image fills its slot instead of leaving bars.
    #[cfg(test)]
    pub fn size_for(&self, resize: Resize, size: Size) -> Option<Size> {
        self.pending
            .as_ref()
            .or(self.visible.as_ref())
            .map(|protocol| protocol.size_for(resize, size))
    }

    pub fn update_resized_protocol(&mut self, response: ResizeResponse) -> bool {
        if response.generation != self.generation {
            return false;
        }
        self.encoding = false;
        // Failure clears the old cover too: it must not represent another song.
        self.visible = response.protocol;
        true
    }
}

impl ResizeEncodeRender for Cover {
    fn needs_resize(&self, resize: &Resize, size: Size) -> Option<Size> {
        if self.encoding {
            return None;
        }
        self.pending
            .as_ref()
            .or(self.visible.as_ref())
            .and_then(|p| p.needs_resize(resize, size))
    }

    fn resize_encode(&mut self, resize: &Resize, size: Size) {
        if self.encoding {
            return;
        }
        // For a song change, take only the candidate and keep drawing visible.
        // A layout resize may move the visible protocol to the worker instead.
        if let Some(protocol) = self.pending.take().or_else(|| self.visible.take()) {
            self.generation = self.generation.wrapping_add(1);
            self.encoding = true;
            if self
                .tx
                .send(ResizeRequest {
                    protocol,
                    resize: resize.clone(),
                    size,
                    generation: self.generation,
                })
                .is_err()
            {
                self.encoding = false;
                self.visible = None;
            }
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if let Some(protocol) = &mut self.visible {
            // Sixel cannot be clipped like text; avoid spilling an old image
            // into adjacent panels if the pane shrank while work was pending.
            if protocol.needs_resize(&COVER_RESIZE, area.into()).is_none() {
                protocol.render(area, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::Artwork;
    use ratatui_image::{FontSize, picker::ProtocolType};
    use std::sync::mpsc;

    #[test]
    fn keep_visible_cover_until_replacement_is_ready_and_reject_obsolete_work() {
        for kind in [
            ProtocolType::Halfblocks,
            ProtocolType::Kitty,
            ProtocolType::Sixel,
        ] {
            let artwork = Artwork::Native {
                protocol: kind,
                font_size: FontSize::new(10, 20),
                tmux: true,
            };
            let protocol = |color| {
                artwork.new_resize_protocol(
                    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                        64,
                        64,
                        image::Rgb(color),
                    )),
                    Rgba([0, 0, 0, 255]),
                )
            };
            let area = Rect::new(0, 0, 8, 4);
            let render = |cover: &mut Cover| {
                let mut buffer = Buffer::empty(area);
                cover.resize_encode_render(&COVER_RESIZE, area, &mut buffer);
                buffer
            };
            let (tx, rx) = mpsc::channel();
            let mut cover = Cover::new(tx, None);
            cover.replace_protocol(protocol([255, 0, 0]));
            render(&mut cover);
            assert!(cover.update_resized_protocol(rx.recv().unwrap().resize_encode()));
            render(&mut cover); // Kitty transmits only on the first render.
            let original = render(&mut cover);
            assert_ne!(original, Buffer::empty(area));

            cover.retain_visible(); // New file is still decoding.
            assert_eq!(render(&mut cover), original);
            cover.replace_protocol(protocol([0, 255, 0]));
            assert_eq!(render(&mut cover), original); // Encoding in progress.
            let obsolete = rx.recv().unwrap().resize_encode();
            assert_eq!(render(&mut cover), original);

            cover.retain_visible(); // Skip to another song before encoding returns.
            assert!(!cover.update_resized_protocol(obsolete));
            assert_eq!(render(&mut cover), original);
            cover.replace_protocol(protocol([0, 0, 255]));
            assert_eq!(render(&mut cover), original);
            assert!(cover.update_resized_protocol(rx.recv().unwrap().resize_encode()));
            assert_ne!(render(&mut cover), original);

            cover.replace_protocol(protocol([255, 255, 0]));
            render(&mut cover);
            let obsolete = rx.recv().unwrap().resize_encode();
            cover.empty_protocol(); // A track without art must clear immediately.
            assert!(!cover.update_resized_protocol(obsolete));
            assert!(!cover.has_image());
            assert_eq!(render(&mut cover), Buffer::empty(area));

            cover.replace_protocol(protocol([0, 255, 255]));
            render(&mut cover);
            let mut failed = rx.recv().unwrap().resize_encode();
            failed.protocol = None;
            assert!(cover.update_resized_protocol(failed));
            assert!(!cover.has_image());
        }
    }
}
