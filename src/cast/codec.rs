//! Opus encoding and decoding at the fixed cast format: 48 kHz stereo, 20 ms
//! frames of interleaved `f32` samples.
use anyhow::{Context, Result, ensure};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Frames (sample pairs) per 20 ms packet.
pub const FRAME_FRAMES: usize = 960;
/// Interleaved samples per 20 ms packet.
pub const FRAME_LEN: usize = FRAME_FRAMES * CHANNELS;
/// Recommended output capacity for one packet; actual packets are far smaller.
const MAX_PACKET: usize = 4000;
/// A packet may carry up to 120 ms of audio.
const MAX_DECODED: usize = 5760 * CHANNELS;

/// Frames (samples per channel) a packet decodes to, from its table of contents.
pub fn packet_frames(packet: &[u8]) -> Option<usize> {
    opus::packet::get_nb_samples(packet, SAMPLE_RATE).ok()
}

pub struct Encoder {
    inner: opus::Encoder,
    lookahead: u16,
    packet: Vec<u8>,
}

impl Encoder {
    pub fn new(bitrate: u32) -> Result<Self> {
        let mut inner = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )
        .context("Cannot create the Opus encoder")?;
        inner.set_bitrate(opus::Bitrate::Bits(
            i32::try_from(bitrate).context("Opus bitrate out of range")?,
        ))?;
        inner.set_vbr(true)?;
        inner.set_signal(opus::Signal::Music)?;
        inner.set_complexity(10)?;
        let lookahead = u16::try_from(inner.get_lookahead()?).context("Opus lookahead")?;
        Ok(Self {
            inner,
            lookahead,
            packet: vec![0; MAX_PACKET],
        })
    }

    /// Samples the decoder must discard at the start of a fresh stream.
    pub fn lookahead(&self) -> u16 {
        self.lookahead
    }

    /// Forget previous audio so the next packets decode without earlier state.
    pub fn reset(&mut self) -> Result<()> {
        self.inner.reset_state()?;
        Ok(())
    }

    pub fn encode(&mut self, frame: &[f32]) -> Result<&[u8]> {
        ensure!(
            frame.len() == FRAME_LEN,
            "Opus frames carry {FRAME_LEN} interleaved samples, got {}",
            frame.len()
        );
        let len = self.inner.encode_float(frame, &mut self.packet)?;
        Ok(&self.packet[..len])
    }
}

pub struct Decoder {
    inner: opus::Decoder,
    pcm: Vec<f32>,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo)
                .context("Cannot create the Opus decoder")?,
            pcm: vec![0.0; MAX_DECODED],
        })
    }

    pub fn reset(&mut self) -> Result<()> {
        self.inner.reset_state()?;
        Ok(())
    }

    /// Decode one packet to interleaved stereo samples.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32]> {
        ensure!(!packet.is_empty(), "Empty Opus packet");
        let frames = self.inner.decode_float(packet, &mut self.pcm, false)?;
        Ok(&self.pcm[..frames * CHANNELS])
    }

    /// Conceal one lost 20 ms packet.
    pub fn conceal(&mut self) -> Result<&[f32]> {
        let frames = self
            .inner
            .decode_float(&[], &mut self.pcm[..FRAME_LEN], false)?;
        Ok(&self.pcm[..frames * CHANNELS])
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sine(frames: usize, hz: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let left = 0.5 * (2.0 * std::f32::consts::PI * hz * t).sin();
                let right = 0.3 * (2.0 * std::f32::consts::PI * hz * 1.5 * t).sin();
                [left, right]
            })
            .collect()
    }

    /// Signal-to-noise ratio in dB between the reference and a decoded copy.
    pub fn snr_db(reference: &[f32], decoded: &[f32]) -> f32 {
        let len = reference.len().min(decoded.len());
        let (signal, noise) = reference[..len].iter().zip(&decoded[..len]).fold(
            (0f64, 0f64),
            |(signal, noise), (&a, &b)| {
                (
                    signal + f64::from(a) * f64::from(a),
                    noise + f64::from(a - b) * f64::from(a - b),
                )
            },
        );
        (10.0 * (signal / noise.max(1e-12)).log10()) as f32
    }

    #[test]
    fn frames_round_trip_after_trimming_the_lookahead() {
        let mut encoder = Encoder::new(128_000).unwrap();
        let mut decoder = Decoder::new().unwrap();
        let seconds = 2;
        let input = sine(SAMPLE_RATE as usize * seconds, 440.0);
        let mut output = vec![];
        let mut packets = 0usize;
        let mut bytes = 0usize;
        for frame in input.chunks(FRAME_LEN) {
            let packet = encoder.encode(frame).unwrap();
            assert!(!packet.is_empty() && packet.len() <= MAX_PACKET);
            packets += 1;
            bytes += packet.len();
            output.extend_from_slice(decoder.decode(packet).unwrap());
        }
        assert_eq!(packets, 50 * seconds);
        let kbps = bytes * 8 / seconds / 1000;
        assert!((40..200).contains(&kbps), "{kbps} kbps");
        let skip = usize::from(encoder.lookahead()) * CHANNELS;
        assert!(skip > 0);
        let aligned = &output[skip..];
        // Skip the first 100 ms: the codec converges after a reset.
        let settle = FRAME_LEN * 5;
        let snr = snr_db(&input[settle..], &aligned[settle..]);
        assert!(snr > 18.0, "SNR {snr} dB");
        // Misaligned by half the lookahead the match degrades sharply.
        let misaligned = snr_db(&input[settle..], &output[skip / 2 + settle..]);
        assert!(misaligned < snr - 6.0, "{misaligned} vs {snr}");
    }

    #[test]
    fn wrong_frame_sizes_and_empty_packets_are_rejected() {
        let mut encoder = Encoder::new(96_000).unwrap();
        assert!(encoder.encode(&[0.0; FRAME_LEN - 2]).is_err());
        assert!(encoder.encode(&[0.0; FRAME_LEN * 2]).is_err());
        let mut decoder = Decoder::new().unwrap();
        assert!(decoder.decode(&[]).is_err());
        assert_eq!(decoder.conceal().unwrap().len(), FRAME_LEN);
    }
}
