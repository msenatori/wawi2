//! Just enough PNG: read a file's size from its header (the validator, no
//! decoding) and write a plain RGBA image (the placeholder art of
//! `mascota nueva`). No dependency: the writer stores the pixels without
//! compression, which is fine for a few small placeholders.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// What the header says about a PNG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// 6 = RGBA, 4 = gray + alpha, 3 = palette (may carry transparency), …
    pub color_type: u8,
}

impl Header {
    /// Whether the image can be see-through at all (alpha channel, or a
    /// palette, which may have a `tRNS` chunk).
    pub fn may_have_alpha(&self) -> bool {
        matches!(self.color_type, 3 | 4 | 6)
    }
}

pub fn header(path: &Path) -> io::Result<Header> {
    let mut bytes = [0u8; 26];
    File::open(path)?.read_exact(&mut bytes)?;
    parse_header(&bytes)
}

pub fn size(path: &Path) -> io::Result<(u32, u32)> {
    header(path).map(|h| (h.width, h.height))
}

pub fn parse_header(bytes: &[u8]) -> io::Result<Header> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "no es un PNG");
    if bytes.len() < 26 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return Err(bad());
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (width, height) = (be(16), be(20));
    if width == 0 || height == 0 {
        return Err(bad());
    }
    Ok(Header {
        width,
        height,
        color_type: bytes[25],
    })
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(data);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}

/// An RGBA image (`width * height * 4` bytes, rows top to bottom) as PNG.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(rgba.len(), width as usize * height as usize * 4);
    let row = width as usize * 4;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgba.chunks(row) {
        raw.push(0); // no filter
        raw.extend(line);
    }
    // zlib, stored blocks of at most 65535 bytes.
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend([1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        z.push(u8::from(last));
        let len = block.len() as u16;
        z.extend(len.to_le_bytes());
        z.extend((!len).to_le_bytes());
        z.extend(block);
    }
    z.extend(adler32(&raw).to_be_bytes());

    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8 bits, RGBA, deflate, no filter, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_it_writes_it_reads_back() {
        let png = encode_rgba(3, 2, &[200; 3 * 2 * 4]);
        let h = parse_header(&png).unwrap();
        assert_eq!((h.width, h.height, h.color_type), (3, 2, 6));
        assert!(h.may_have_alpha());
        assert!(parse_header(b"GIF89a.....................").is_err());
    }

    #[test]
    fn the_checksums_are_the_standard_ones() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn it_reads_the_size_of_wawi_frames() {
        let dir = crate::default_assets().join("wawi/cuadros");
        assert_eq!(size(&dir.join("idle_a.png")).unwrap(), (320, 286));
    }
}
