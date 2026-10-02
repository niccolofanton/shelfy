//! Shared helpers of the media integration tests: synthetic images in every
//! decodable format, EXIF orientation, a migrated library database and a
//! temporary store. No fixture comes from real media.

#![allow(dead_code)] // each test binary uses a different subset

use std::io::Cursor;

use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{
    DynamicImage, ExtendedColorType, Frame, ImageEncoder as _, ImageFormat, RgbImage, Rgba,
    RgbaImage,
};
use rusqlite::Connection;
use shelfy_core::schema::{self, Kind};
use shelfy_media::store::{MediaStore, UserMedia};
use tempfile::TempDir;

/// A user id in the shape of a ULID.
pub const USER: &str = "01J8ZQ5V3X9K2M4N6P8R0T2V4W";
/// Another user.
pub const OTHER_USER: &str = "01J8ZQ5V3X9K2M4N6P8R0T2V4X";
/// 2026-10-02T00:00:00Z, the "now" of the tests.
pub const NOW: i64 = 1_790_899_200_000;
/// One day in milliseconds.
pub const DAY: i64 = 86_400_000;

pub const RED: [u8; 3] = [230, 20, 20];
pub const BLUE: [u8; 3] = [20, 20, 230];
pub const GREEN: [u8; 3] = [20, 200, 20];

/// A store on a temporary directory, removed on drop.
pub struct TempStore {
    pub dir: TempDir,
    pub store: MediaStore,
}

impl TempStore {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = MediaStore::new(dir.path().join("users"));
        Self { dir, store }
    }

    pub fn user(&self, user_id: &str) -> UserMedia {
        self.store.user(user_id).expect("valid user id")
    }
}

/// A fresh, fully migrated library database in memory.
pub fn library() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open in-memory db");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    schema::migrate(&mut conn, Kind::Library).expect("migrate library");
    conn
}

/// An image whose left half is `left` and right half is `right`.
pub fn halves(width: u32, height: u32, left: [u8; 3], right: [u8; 3]) -> RgbImage {
    RgbImage::from_fn(width, height, |x, _| {
        image::Rgb(if x < width / 2 { left } else { right })
    })
}

/// A photo-like image: overlapping smooth waves per channel plus fine
/// deterministic noise, so encoders see gradients and texture as in real
/// photos.
pub fn photo(width: u32, height: u32, seed: u64) -> RgbImage {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut noise = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let r = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        f64::from(u8::try_from(r >> 56).unwrap()) / 255.0 - 0.5
    };
    let phase = f64::from(u32::try_from(seed % 97).unwrap()) / 10.0;
    RgbImage::from_fn(width, height, |x, y| {
        let (x, y) = (f64::from(x), f64::from(y));
        let mut pixel = [0u8; 3];
        for (c, value) in pixel.iter_mut().enumerate() {
            let c = f64::from(u32::try_from(c).unwrap());
            let wave = 60.0 * ((x * 0.011 + c) + (y * 0.007) + phase).sin()
                + 35.0 * ((x * 0.043 - y * 0.031) + c * 2.0).sin()
                + 15.0 * ((x * 0.19 + y * 0.23) + phase * c).sin();
            let level = 128.0 + wave + 24.0 * noise();
            *value = level.clamp(0.0, 255.0) as u8;
        }
        image::Rgb(pixel)
    })
}

/// The EXIF (TIFF) block of an image with this orientation tag (1–8).
pub fn exif_orientation(value: u16) -> Vec<u8> {
    let mut exif = b"MM\0\x2a\0\0\0\x08".to_vec(); // big-endian TIFF, IFD0 at 8
    exif.extend_from_slice(&1u16.to_be_bytes()); // one entry
    exif.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    exif.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    exif.extend_from_slice(&1u32.to_be_bytes()); // one value
    exif.extend_from_slice(&value.to_be_bytes());
    exif.extend_from_slice(&[0, 0]); // padding of the value field
    exif.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    exif
}

/// `image` as JPEG, with an EXIF orientation when given.
pub fn jpeg(image: &RgbImage, quality: u8, orientation: Option<u16>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    if let Some(value) = orientation {
        encoder
            .set_exif_metadata(exif_orientation(value))
            .expect("JPEG takes EXIF");
    }
    encoder
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .expect("encode JPEG");
    out
}

/// `image` as PNG.
pub fn png(image: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .expect("encode PNG");
    out
}

/// `image` as lossless WebP.
pub fn webp_lossless(image: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .expect("encode WebP");
    out
}

/// An animated GIF of `frames`.
pub fn gif(frames: &[RgbaImage]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut out);
        encoder.set_repeat(Repeat::Infinite).expect("repeat");
        encoder
            .encode_frames(frames.iter().cloned().map(Frame::new))
            .expect("encode GIF");
    }
    out
}

/// An animated WebP of `frames` (libwebp's animation encoder).
pub fn webp_animated(frames: &[RgbaImage]) -> Vec<u8> {
    let config = webp::WebPConfig::new().expect("config");
    let (width, height) = frames[0].dimensions();
    let mut encoder = webp::AnimEncoder::new(width, height, &config);
    for (i, frame) in frames.iter().enumerate() {
        let timestamp = i32::try_from(i).unwrap() * 100;
        encoder.add_frame(webp::AnimFrame::from_rgba(
            frame.as_raw(),
            width,
            height,
            timestamp,
        ));
    }
    encoder.try_encode().expect("encode animated WebP").to_vec()
}

/// A solid RGBA frame.
pub fn solid(width: u32, height: u32, rgb: [u8; 3]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba([rgb[0], rgb[1], rgb[2], 255]))
}

/// Decodes a WebP rendition.
pub fn decode_webp(bytes: &[u8]) -> DynamicImage {
    image::load(Cursor::new(bytes), ImageFormat::WebP).expect("a valid WebP")
}

/// Asserts that `actual` is within `tolerance` of `expected` on every channel.
pub fn assert_near(actual: &[u8], expected: &[u8], tolerance: u8, what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: channel count");
    for (a, e) in actual.iter().zip(expected) {
        assert!(
            a.abs_diff(*e) <= tolerance,
            "{what}: {actual:?} is not within {tolerance} of {expected:?}"
        );
    }
}

/// CRC-32 (ISO-HDLC) of `data`, for patching PNG chunks.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A valid PNG whose header claims `width` × `height` (its data is tiny).
pub fn png_claiming(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = png(&DynamicImage::ImageRgb8(halves(2, 2, RED, BLUE)));
    // Signature (8) + IHDR length (4); then "IHDR", width, height, …, CRC.
    let ihdr = 12;
    bytes[ihdr + 4..ihdr + 8].copy_from_slice(&width.to_be_bytes());
    bytes[ihdr + 8..ihdr + 12].copy_from_slice(&height.to_be_bytes());
    let crc = crc32(&bytes[ihdr..ihdr + 17]);
    bytes[ihdr + 17..ihdr + 21].copy_from_slice(&crc.to_be_bytes());
    bytes
}

/// A valid GIF whose logical screen claims `width` × `height`.
pub fn gif_claiming(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = gif(&[solid(2, 2, RED)]);
    bytes[6..8].copy_from_slice(&width.to_le_bytes());
    bytes[8..10].copy_from_slice(&height.to_le_bytes());
    bytes
}

/// A valid JPEG whose frame header claims `width` × `height`.
pub fn jpeg_claiming(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = jpeg(&halves(16, 16, RED, BLUE), 90, None);
    let sof = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("baseline SOF0");
    // FF C0, length (2), precision (1), height (2), width (2).
    bytes[sof + 5..sof + 7].copy_from_slice(&height.to_be_bytes());
    bytes[sof + 7..sof + 9].copy_from_slice(&width.to_be_bytes());
    bytes
}
