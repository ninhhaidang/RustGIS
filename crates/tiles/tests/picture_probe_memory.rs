//! A tile picture's header is read before it is handed to the browser's decoder, and that
//! reading allocates no more than decoding it here may. A PNG's colour profile (`iCCP`) is
//! inflated while its header is read; a small file whose profile inflates without end must not
//! make the probe take half a gigabyte.
//!
//! Its own test binary: the process's peak memory is measured, which other tests would raise.
//! Linux only: it reads the peak from `/proc`.
#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustgis_tiles::cache::{self, Payload, State, Want};

/// CRC-32 (ISO 3309, as PNG uses it), bit by bit.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Bits packed into bytes as DEFLATE packs them (RFC 1951 §3.1.1): the least significant first.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// `count` bits of `value`, the lowest first.
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= u64::from(value) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, which DEFLATE packs with its most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let reversed = (0..len).fold(0, |r, i| (r << 1) | ((code >> i) & 1));
        self.put(reversed, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// A zlib stream (RFC 1950) inflating to `1 + 258 * repeats` zero bytes: one literal zero, then
/// `repeats` copies of 258 bytes from one back, in a single block with the fixed Huffman codes.
fn zeros_zlib(repeats: usize) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.put(1, 1); // the last block
    bits.put(1, 2); // fixed Huffman codes
    bits.code(0x30, 8); // literal 0
    for _ in 0..repeats {
        bits.code(0xc5, 8); // length 258 (symbol 285)
        bits.code(0, 5); // distance 1 (code 0)
    }
    bits.code(0, 7); // end of block (symbol 256)
    let mut out = vec![0x78, 0x01];
    out.extend(bits.finish());
    // Adler-32 of n zeros: a = 1, b = n.
    let n = (1 + 258 * repeats as u64) % 65_521;
    out.extend(((n << 16 | 1) as u32).to_be_bytes());
    out
}

/// A 1×1 PNG carrying an `iCCP` profile that inflates to about 540 MB from about 3.4 MB.
fn png_with_huge_profile() -> Vec<u8> {
    let mut pm = tiny_skia::Pixmap::new(1, 1).unwrap_or_else(|| panic!("pixmap"));
    pm.fill(tiny_skia::Color::from_rgba8(10, 20, 30, 255));
    let png = pm.encode_png().unwrap_or_else(|e| panic!("{e}"));
    // The signature and the IHDR chunk (length, type, 13 bytes, CRC).
    let (head, rest) = png.split_at(8 + 4 + 4 + 13 + 4);
    assert_eq!(head.get(12..16), Some(&b"IHDR"[..]));
    let mut chunk = b"iCCP".to_vec();
    chunk.extend(b"icc\0\0"); // the profile's name, then compression method 0 (zlib)
    chunk.extend(zeros_zlib(2_100_000));
    let mut out = head.to_vec();
    out.extend(u32::try_from(chunk.len() - 4).unwrap_or_else(|e| panic!("{e}")).to_be_bytes());
    out.extend(&chunk);
    out.extend(crc32(&chunk).to_be_bytes());
    out.extend(rest);
    out
}

/// The most memory this process has held, in bytes (Linux reports it in `/proc`).
fn peak_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_else(|e| panic!("{e}"));
    let kib = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no VmHWM in /proc/self/status"));
    kib * 1024
}

/// Asks the cache for `url` until it settles.
fn settled(url: &str) -> State {
    let start = Instant::now();
    loop {
        match cache::get(url, Want::Image) {
            State::Pending if start.elapsed() < Duration::from_secs(120) => std::thread::sleep(Duration::from_millis(5)),
            other => return other,
        }
    }
}

#[test]
fn a_tile_pictures_header_is_read_within_the_decode_memory_limit() {
    let png = png_with_huge_profile();
    assert!(png.len() < cache::MAX_BYTES, "{} bytes", png.len());
    let bad = tiny_skia::Pixmap::new(4097, 1).unwrap_or_else(|| panic!("pixmap")).encode_png().unwrap_or_else(|e| panic!("{e}"));
    let (served, wide) = (png.clone(), bad.clone());
    cache::set_fetcher(move |url, done| done(Ok(if url.contains("wide") { wide.clone() } else { served.clone() })));
    // The browser's decoder, standing in: hands the bytes back, so they are decoded here.
    let handed = Arc::new(AtomicUsize::new(0));
    let count = handed.clone();
    cache::set_image_decoder(move |bytes, done| {
        count.fetch_add(1, Ordering::SeqCst);
        done(Err(bytes))
    });

    let before = peak_bytes();
    let State::Ready(tile) = settled("https://tiles.example.org/0/0/0.png") else { panic!("the tile wasn't read") };
    let grew = peak_bytes().saturating_sub(before);
    // The profile goes (png gives up on it at the limit); the picture is the tile.
    let Payload::Image(p) = tile.as_ref() else { panic!("not a picture") };
    assert_eq!((p.width(), p.height()), (1, 1));
    assert_eq!(handed.load(Ordering::SeqCst), 1);
    // Decoding here allows 96 MiB; without a limit of its own the probe took over 512 MiB.
    assert!(grew < 256 << 20, "reading the tile took {} MiB", grew >> 20);

    // A picture over the limits is never handed to the decoder: the tile fails, saying why.
    let State::Failed(why) = settled("https://tiles.example.org/wide/0/0.png") else { panic!("a 4097-pixel tile was read") };
    assert!(why.to_ascii_lowercase().contains("limit"), "{why}");
    assert_eq!(handed.load(Ordering::SeqCst), 1);
    cache::clear_fetcher();
}
