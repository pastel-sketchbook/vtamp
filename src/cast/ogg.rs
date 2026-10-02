//! Ogg page framing (RFC 3533) over a byte stream.
//!
//! The writer lays packets into pages and emits finished pages into a byte
//! buffer; the reader is push-based, needs no seeking, and resynchronizes on
//! the capture pattern after damaged or truncated input.

const CAPTURE: &[u8; 4] = b"OggS";
const HEADER_LEN: usize = 27;
const MAX_SEGMENTS: usize = 255;
/// Granule position of a page on which no packet ends (`-1` in the specification).
pub const NO_GRANULE: u64 = u64::MAX;

pub const CONTINUED: u8 = 0x01;
pub const BOS: u8 = 0x02;
pub const EOS: u8 = 0x04;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04c1_1db7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}
static CRC_TABLE: [u32; 256] = crc_table();

/// Ogg CRC: polynomial 0x04c11db7, zero initial value, no reflection, no final XOR.
pub fn crc32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |crc, &byte| {
        (crc << 8) ^ CRC_TABLE[usize::from((crc >> 24) as u8 ^ byte)]
    })
}

/// Lays packets into pages of one logical stream.
pub struct PageWriter {
    serial: u32,
    sequence: u32,
    segments: Vec<u8>,
    body: Vec<u8>,
    /// The pending page starts inside a packet begun on the previous page.
    continued: bool,
    /// Granule of the last packet completed on the pending page.
    granule: u64,
    /// Granule of the last packet completed on any page, for an empty EOS page.
    last_granule: u64,
}

impl PageWriter {
    pub fn new(serial: u32) -> Self {
        Self {
            serial,
            sequence: 0,
            segments: Vec::with_capacity(MAX_SEGMENTS),
            body: Vec::new(),
            continued: false,
            granule: NO_GRANULE,
            last_granule: 0,
        }
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// Append a packet whose last sample has the given granule position. A
    /// packet that does not fit continues on following pages, which are written
    /// to `out` as they fill.
    pub fn packet(&mut self, data: &[u8], granule: u64, out: &mut Vec<u8>) {
        let mut rest = data;
        loop {
            if self.segments.len() == MAX_SEGMENTS {
                self.emit(false, out);
                self.continued = true;
            }
            if rest.len() >= 255 {
                self.segments.push(255);
                self.body.extend_from_slice(&rest[..255]);
                rest = &rest[255..];
            } else {
                self.segments.push(rest.len() as u8);
                self.body.extend_from_slice(rest);
                self.granule = granule;
                self.last_granule = granule;
                return;
            }
        }
    }

    /// Write the pending page. Without pending packets only an EOS request
    /// writes a page (an empty one carrying the last granule).
    pub fn flush(&mut self, eos: bool, out: &mut Vec<u8>) -> bool {
        if self.segments.is_empty() && !eos {
            return false;
        }
        if self.segments.is_empty() {
            self.granule = self.last_granule;
        }
        self.emit(eos, out);
        self.continued = false;
        true
    }

    fn emit(&mut self, eos: bool, out: &mut Vec<u8>) {
        let mut flags = 0;
        if self.continued {
            flags |= CONTINUED;
        }
        if self.sequence == 0 {
            flags |= BOS;
        }
        if eos {
            flags |= EOS;
        }
        let start = out.len();
        out.extend_from_slice(CAPTURE);
        out.push(0);
        out.push(flags);
        out.extend_from_slice(&self.granule.to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.push(self.segments.len() as u8);
        out.extend_from_slice(&self.segments);
        out.extend_from_slice(&self.body);
        let crc = crc32(&out[start..]);
        out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence = self.sequence.wrapping_add(1);
        self.segments.clear();
        self.body.clear();
        self.granule = NO_GRANULE;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub flags: u8,
    pub granule: u64,
    pub serial: u32,
    pub sequence: u32,
    pub segments: Vec<u8>,
    pub body: Vec<u8>,
}

impl Page {
    pub fn continued(&self) -> bool {
        self.flags & CONTINUED != 0
    }
    pub fn bos(&self) -> bool {
        self.flags & BOS != 0
    }
    pub fn eos(&self) -> bool {
        self.flags & EOS != 0
    }
    /// Packet pieces in order: `(bytes, complete)`. Pieces that continue on the
    /// next page are reported incomplete; a continued first piece belongs to a
    /// packet begun earlier.
    pub fn pieces(&self) -> impl Iterator<Item = (&[u8], bool)> {
        let mut offset = 0;
        let mut segments = self.segments.iter();
        std::iter::from_fn(move || {
            let start = offset;
            let mut pending = false;
            for &lacing in segments.by_ref() {
                offset += usize::from(lacing);
                if lacing < 255 {
                    return Some((&self.body[start..offset], true));
                }
                pending = true;
            }
            pending.then(|| (&self.body[start..offset], false))
        })
    }
}

/// Reassembles pages from arbitrary byte chunks.
#[derive(Default)]
pub struct PageReader {
    buffer: Vec<u8>,
    /// Bytes dropped while searching for a valid page.
    pub skipped: u64,
}

impl PageReader {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_page(&mut self) -> Option<Page> {
        loop {
            let Some(start) = self
                .buffer
                .windows(CAPTURE.len())
                .position(|window| window == CAPTURE)
            else {
                let keep = self.buffer.len().min(CAPTURE.len() - 1);
                self.drop_front(self.buffer.len() - keep);
                return None;
            };
            self.drop_front(start);
            if self.buffer.len() < HEADER_LEN {
                return None;
            }
            if self.buffer[4] != 0 {
                self.drop_front(1);
                continue;
            }
            let segment_count = usize::from(self.buffer[26]);
            let table_end = HEADER_LEN + segment_count;
            if self.buffer.len() < table_end {
                return None;
            }
            let body_len: usize = self.buffer[HEADER_LEN..table_end]
                .iter()
                .map(|&lacing| usize::from(lacing))
                .sum();
            let total = table_end + body_len;
            if self.buffer.len() < total {
                return None;
            }
            let page = &self.buffer[..total];
            let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
            let mut crc = crc32(&page[..22]);
            crc = (0..4).fold(crc, |crc, _| {
                (crc << 8) ^ CRC_TABLE[usize::from((crc >> 24) as u8)]
            });
            for &byte in &page[26..] {
                crc = (crc << 8) ^ CRC_TABLE[usize::from((crc >> 24) as u8 ^ byte)];
            }
            if crc != stored {
                self.drop_front(1);
                continue;
            }
            let parsed = Page {
                flags: page[5],
                granule: u64::from_le_bytes(page[6..14].try_into().unwrap()),
                serial: u32::from_le_bytes(page[14..18].try_into().unwrap()),
                sequence: u32::from_le_bytes(page[18..22].try_into().unwrap()),
                segments: page[HEADER_LEN..table_end].to_vec(),
                body: page[table_end..total].to_vec(),
            };
            self.buffer.drain(..total);
            return Some(parsed);
        }
    }

    fn drop_front(&mut self, count: usize) {
        if count > 0 {
            self.skipped += count as u64;
            self.buffer.drain(..count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(bytes: &[u8], chunk: usize) -> (Vec<Page>, u64) {
        let mut reader = PageReader::default();
        let mut pages = vec![];
        for piece in bytes.chunks(chunk.max(1)) {
            reader.push(piece);
            while let Some(page) = reader.next_page() {
                pages.push(page);
            }
        }
        (pages, reader.skipped)
    }

    fn packet(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31) ^ seed)
            .collect()
    }

    #[test]
    fn crc_matches_the_reference_implementation() {
        for len in [0, 1, 27, 300, 4096] {
            let data = packet(len, 7);
            assert_eq!(crc32(&data), ogg_pager::crc32(&data), "{len}");
        }
    }

    #[test]
    fn pages_round_trip_with_every_lacing_shape_and_chunking() {
        let packets: Vec<Vec<u8>> = [0usize, 1, 254, 255, 256, 510, 1000, 65_025, 70_000]
            .into_iter()
            .enumerate()
            .map(|(i, len)| packet(len, i as u8))
            .collect();
        let mut writer = PageWriter::new(0xDEAD_BEEF);
        let mut bytes = vec![];
        for (i, data) in packets.iter().enumerate() {
            writer.packet(data, (i as u64 + 1) * 960, &mut bytes);
            if i % 3 == 2 && i + 1 < packets.len() {
                assert!(writer.flush(false, &mut bytes));
            }
        }
        assert!(writer.flush(true, &mut bytes));
        assert!(!writer.flush(false, &mut bytes));

        let (pages, skipped) = read_all(&bytes, usize::MAX);
        assert_eq!(skipped, 0);
        assert_eq!(pages.len(), 5);
        assert!(pages[0].bos());
        assert!(pages.iter().skip(1).all(|p| !p.bos()));
        assert!(pages.last().unwrap().eos());
        assert!(pages.iter().rev().skip(1).all(|p| !p.eos()));
        assert!(pages.iter().all(|p| p.serial == 0xDEAD_BEEF));
        assert!(
            pages
                .iter()
                .enumerate()
                .all(|(i, p)| p.sequence == i as u32)
        );
        assert!(pages.iter().all(|p| p.segments.len() <= MAX_SEGMENTS));

        let mut assembled = vec![];
        let mut partial = vec![];
        let mut last_granule = None;
        for page in &pages {
            let mut ended_here = false;
            for (piece, complete) in page.pieces() {
                partial.extend_from_slice(piece);
                if complete {
                    assembled.push(std::mem::take(&mut partial));
                    ended_here = true;
                }
            }
            if ended_here {
                assert_ne!(page.granule, NO_GRANULE);
                assert_eq!(page.granule, assembled.len() as u64 * 960);
                last_granule = Some(page.granule);
            } else {
                assert_eq!(page.granule, NO_GRANULE);
            }
        }
        assert!(partial.is_empty());
        assert_eq!(assembled, packets);
        assert_eq!(last_granule, Some(packets.len() as u64 * 960));

        for chunk in [1, 7, 100, 1500] {
            let (again, skipped) = read_all(&bytes, chunk);
            assert_eq!(skipped, 0, "{chunk}");
            assert_eq!(again, pages, "{chunk}");
        }
        // The layout matches an independent Ogg implementation.
        let mut cursor = std::io::Cursor::new(&bytes);
        for page in &pages {
            let reference = ogg_pager::Page::read(&mut cursor).unwrap();
            assert_eq!(reference.header().abgp, page.granule);
            assert_eq!(reference.header().stream_serial, page.serial);
            assert_eq!(reference.header().sequence_number, page.sequence);
            assert_eq!(reference.header().header_type_flag(), page.flags);
            assert_eq!(reference.content(), page.body);
        }
    }

    #[test]
    fn empty_eos_page_carries_the_last_granule_and_continuation_is_flagged() {
        let mut writer = PageWriter::new(1);
        let mut bytes = vec![];
        writer.packet(&packet(70_000, 1), 4800, &mut bytes);
        writer.flush(false, &mut bytes);
        writer.flush(true, &mut bytes);
        let (pages, _) = read_all(&bytes, usize::MAX);
        assert_eq!(pages.len(), 3);
        assert!(!pages[0].continued() && pages[0].bos());
        assert_eq!(pages[0].granule, NO_GRANULE);
        assert!(pages[1].continued() && !pages[1].eos());
        assert_eq!(pages[1].granule, 4800);
        assert!(pages[2].eos() && !pages[2].continued());
        assert!(pages[2].segments.is_empty());
        assert_eq!(pages[2].granule, 4800);
    }

    #[test]
    fn reader_skips_garbage_and_damaged_pages_then_resynchronizes() {
        let mut writer = PageWriter::new(9);
        let mut bytes = vec![];
        let mut boundaries = vec![];
        for i in 0..4u8 {
            writer.packet(&packet(300, i), u64::from(i) * 960, &mut bytes);
            writer.flush(i == 3, &mut bytes);
            boundaries.push(bytes.len());
        }
        let mut damaged = b"OggS junk OggS\0".to_vec();
        let prefix = damaged.len();
        damaged.extend_from_slice(&bytes);
        // Flip one body byte of the second page.
        damaged[prefix + boundaries[0] + HEADER_LEN + 3] ^= 0xFF;
        let (pages, skipped) = read_all(&damaged, 13);
        assert_eq!(
            pages.iter().map(|p| p.sequence).collect::<Vec<_>>(),
            vec![0, 2, 3]
        );
        assert_eq!(
            skipped,
            (prefix + boundaries[1] - boundaries[0]) as u64,
            "a damaged page is skipped byte by byte until the next capture"
        );
        // A truncated tail never yields a page and does not panic.
        let (pages, _) = read_all(&bytes[..boundaries[2] + 20], usize::MAX);
        assert_eq!(pages.len(), 3);
    }
}
