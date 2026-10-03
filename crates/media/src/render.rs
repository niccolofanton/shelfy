//! The image pipeline (plan §2.13): decode an image once, then produce a WebP
//! rendition and a ThumbHash placeholder from it.
//!
//! - **Decode** with `image`: JPEG, PNG, GIF and WebP. Animated GIF and WebP
//!   give their first frame. The size is checked from the header before any
//!   pixel buffer exists ([`MAX_SIDE`], [`MAX_PIXELS`], [`MAX_DECODE_BYTES`]),
//!   which defuses decompression bombs.
//! - **Orient** by the EXIF orientation. The resize runs on the stored pixels
//!   and the small result is rotated, so a large photo is never rotated at full
//!   size.
//! - **Resize** with `fast_image_resize` (SIMD, Lanczos3) to fit the long side;
//!   never up.
//! - **Encode** lossy WebP with libwebp (the `webp` crate).
//! - **ThumbHash** from the rendition scaled to fit 100 px (the format's
//!   input limit): at most 25 bytes, decoded by the client.
//!
//! The functions here are blocking and CPU-bound: run them on the image pool
//! ([`crate::pool::ImagePool`]), never on an async worker.
//!
//! Colors are not converted: an ICC profile other than sRGB (Display P3 phone
//! photos) is ignored, as browsers do for an untagged WebP.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Cursor, Read as _, Seek, SeekFrom};
use std::path::Path;

use fast_image_resize::images::Image as ResizeImage;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::metadata::Orientation;
use image::{
    DynamicImage, ImageDecoder as _, ImageError, ImageFormat, ImageReader, Limits, RgbImage,
    RgbaImage,
};

use crate::kind::{MediaKind, SNIFF_LEN};
use crate::name::Rendition;

/// Largest width or height decoded, in pixels.
pub const MAX_SIDE: u32 = 16_384;
/// Largest image decoded, in pixels: a 50 MP phone photo fits.
pub const MAX_PIXELS: u64 = 50_000_000;
/// Largest decoded pixel buffer, in bytes.
pub const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
/// Largest encoded source, in bytes. Decoders may read the whole file into
/// memory (the JPEG one does), so this bounds that copy too.
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
/// ThumbHash input limit: at most 100 px on either side.
const THUMBHASH_SIDE: u32 = 100;
/// libwebp effort, 0 (fast) to 6 (small); 4 is libwebp's default.
const WEBP_METHOD: i32 = 4;

/// What to produce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderSpec {
    /// Largest side of the output, in pixels. Smaller sources keep their size.
    pub max_side: u32,
    /// libwebp lossy quality, 0–100.
    pub quality: f32,
}

impl RenderSpec {
    /// The grid rendition (plan §2.13): 480 px, WebP q75.
    pub const G480: Self = Self {
        max_side: 480,
        quality: 75.0,
    };

    /// The master of an archived image that is too large to keep as served
    /// (plan D4, §2.13): WebP q82 at 2,048 px. See [`keeps_original`].
    pub const MASTER: Self = Self {
        max_side: 2_048,
        quality: 82.0,
    };

    /// The master of a video's poster (plan D4, §2.13): always WebP q78, at
    /// most 1,080 px.
    pub const POSTER: Self = Self {
        max_side: 1_080,
        quality: 78.0,
    };
}

/// Largest archived image kept as served, in bytes (plan D4): 1.5 MB.
pub const ORIGINAL_MAX_BYTES: u64 = 1_500_000;

/// Whether an archived image (a cover or an image slide, not a poster) is
/// stored as served: at most [`RenderSpec::MASTER`]'s 2,048 px on its long
/// side and at most [`ORIGINAL_MAX_BYTES`] (plan D4). Otherwise its master
/// is a WebP rendered with [`RenderSpec::MASTER`]. An image the pipeline
/// cannot decode (AVIF) is always kept as served.
#[must_use]
pub fn keeps_original(kind: MediaKind, bytes: u64, width: u32, height: u32) -> bool {
    !kind.is_renderable()
        || (bytes <= ORIGINAL_MAX_BYTES && width.max(height) <= RenderSpec::MASTER.max_side)
}

/// The size of the image file at `path` as displayed (after its EXIF
/// orientation), read from its header: no pixel is decoded.
///
/// # Errors
///
/// [`RenderError`]: not an image the pipeline decodes, or a damaged header.
pub fn dimensions(path: &Path) -> Result<(u32, u32), RenderError> {
    let mut reader = BufReader::new(File::open(path)?);
    let (kind, _) = probe(&mut reader)?;
    let format = match kind {
        Some(MediaKind::Jpeg) => ImageFormat::Jpeg,
        Some(MediaKind::Png) => ImageFormat::Png,
        Some(MediaKind::Gif) => ImageFormat::Gif,
        Some(MediaKind::Webp) => ImageFormat::WebP,
        other => return Err(RenderError::Unsupported(other)),
    };
    let mut image_reader = ImageReader::with_format(reader, format);
    image_reader.limits(limits());
    let mut decoder = image_reader
        .into_decoder()
        .map_err(|e| decode_error(e, kind))?;
    let (width, height) = decoder.dimensions();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    Ok(if swaps_axes(orientation) {
        (height, width)
    } else {
        (width, height)
    })
}

impl Rendition {
    /// What this rendition is rendered with.
    #[must_use]
    pub const fn spec(self) -> RenderSpec {
        match self {
            Self::G480 => RenderSpec::G480,
        }
    }
}

/// The outputs of one render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    /// Width of the source as displayed (after EXIF orientation): the
    /// object's `media_objects.width`.
    pub source_width: u32,
    /// Height of the source as displayed: the object's `media_objects.height`.
    pub source_height: u32,
    /// Width of the WebP.
    pub width: u32,
    /// Height of the WebP.
    pub height: u32,
    /// The WebP file.
    pub webp: Vec<u8>,
    /// The ThumbHash (at most 25 bytes): `posts.thumbhash` for a cover.
    pub thumbhash: Vec<u8>,
}

/// Why a render failed.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The source is not an image the pipeline decodes (`None`: not even a
    /// type of the allowlist).
    #[error("not an image the pipeline decodes")]
    Unsupported(Option<MediaKind>),
    /// The source is over the size limits.
    #[error("the image is over the decode limits")]
    TooLarge,
    /// The source is damaged or truncated.
    #[error("corrupt image: {0}")]
    Corrupt(String),
    /// Resizing or encoding failed.
    #[error("cannot produce the rendition: {0}")]
    Output(String),
    /// Reading the source failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The render panicked (a decoder bug); the pool caught it.
    #[error("the image job panicked")]
    Panicked,
}

/// Renders the image file at `path`.
///
/// # Errors
///
/// [`RenderError`].
pub fn render_file(path: &Path, spec: RenderSpec) -> Result<Rendered, RenderError> {
    render(BufReader::new(File::open(path)?), spec)
}

/// Renders an image held in memory.
///
/// # Errors
///
/// [`RenderError`].
pub fn render_bytes(bytes: &[u8], spec: RenderSpec) -> Result<Rendered, RenderError> {
    render(Cursor::new(bytes), spec)
}

/// Renders the image `reader` starts at.
///
/// # Errors
///
/// [`RenderError`]: the source is refused, damaged or unreadable, or the
/// output cannot be produced.
pub fn render<R: BufRead + Seek>(reader: R, spec: RenderSpec) -> Result<Rendered, RenderError> {
    let Decoded {
        image: small,
        source_width,
        source_height,
    } = decode_and_fit(reader, spec.max_side)?;
    Ok(Rendered {
        source_width,
        source_height,
        width: small.width(),
        height: small.height(),
        webp: encode_webp(&small, spec.quality)?,
        thumbhash: thumbhash(&small)?,
    })
}

/// Decodes the image at `path` and encodes it as a JPEG whose long side is at
/// most `max_side`, at quality `quality` (1–100). For sending to a provider
/// that rejects WebP (the owner's llama.cpp node, which decodes images with
/// stb_image): the archived renditions and masters are WebP or the source's
/// own type, so cataloging transcodes them to JPEG first (P3-13; the node's
/// `without_webp`). Blocking and CPU-bound: run it on the [`crate::pool`].
///
/// # Errors
///
/// [`RenderError`]: the source is refused, damaged or unreadable, or the JPEG
/// cannot be produced.
pub fn jpeg_file(path: &Path, max_side: u32, quality: u8) -> Result<Vec<u8>, RenderError> {
    jpeg(BufReader::new(File::open(path)?), max_side, quality)
}

/// [`jpeg_file`] from an in-memory image (a WebP rendition read from the CAS,
/// a video frame).
///
/// # Errors
///
/// As [`jpeg_file`].
pub fn jpeg_bytes(bytes: &[u8], max_side: u32, quality: u8) -> Result<Vec<u8>, RenderError> {
    jpeg(Cursor::new(bytes), max_side, quality)
}

/// Decodes `reader` and encodes a JPEG (see [`jpeg_file`]).
///
/// # Errors
///
/// As [`jpeg_file`].
pub fn jpeg<R: BufRead + Seek>(
    reader: R,
    max_side: u32,
    quality: u8,
) -> Result<Vec<u8>, RenderError> {
    let Decoded { image: small, .. } = decode_and_fit(reader, max_side)?;
    encode_jpeg(&small, quality)
}

/// A decoded image, resized and oriented, with its displayed source size.
struct Decoded {
    image: DynamicImage,
    source_width: u32,
    source_height: u32,
}

/// Decodes the image `reader` starts at, orients it by its EXIF, and resizes
/// it to fit `max_side` on the long side (never up). The pipeline's one decode
/// path, shared by the WebP and JPEG encoders.
fn decode_and_fit<R: BufRead + Seek>(mut reader: R, max_side: u32) -> Result<Decoded, RenderError> {
    let (kind, source_bytes) = probe(&mut reader)?;
    if source_bytes > MAX_SOURCE_BYTES {
        return Err(RenderError::TooLarge);
    }
    let format = match kind {
        Some(MediaKind::Jpeg) => ImageFormat::Jpeg,
        Some(MediaKind::Png) => ImageFormat::Png,
        Some(MediaKind::Gif) => ImageFormat::Gif,
        Some(MediaKind::Webp) => ImageFormat::WebP,
        other => return Err(RenderError::Unsupported(other)),
    };
    let decode_failed = |e| decode_error(e, kind);

    let mut image_reader = ImageReader::with_format(reader, format);
    image_reader.limits(limits());
    let mut decoder = image_reader.into_decoder().map_err(decode_failed)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(RenderError::Corrupt("the image has no pixels".into()));
    }
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(RenderError::TooLarge);
    }
    // A damaged EXIF block does not make the pixels unusable.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    limits()
        .reserve(decoder.total_bytes())
        .map_err(|_| RenderError::TooLarge)?;
    let image = DynamicImage::from_decoder(decoder).map_err(decode_failed)?;

    let (pixels, pixel_type) = if image.color().has_alpha() {
        (image.into_rgba8().into_raw(), PixelType::U8x4)
    } else {
        (image.into_rgb8().into_raw(), PixelType::U8x3)
    };
    // Sizes as displayed, and the output size in the stored orientation.
    let swaps = swaps_axes(orientation);
    let (source_width, source_height) = if swaps {
        (height, width)
    } else {
        (width, height)
    };
    let (out_width, out_height) = fit(source_width, source_height, max_side);
    let stored = if swaps {
        (out_height, out_width)
    } else {
        (out_width, out_height)
    };
    let pixels = resize(pixels, pixel_type, (width, height), stored)?;
    let mut small = match pixel_type {
        PixelType::U8x4 => RgbaImage::from_raw(stored.0, stored.1, pixels).map(DynamicImage::from),
        _ => RgbImage::from_raw(stored.0, stored.1, pixels).map(DynamicImage::from),
    }
    .ok_or_else(|| RenderError::Output("resized buffer of the wrong size".into()))?;
    small.apply_orientation(orientation);
    Ok(Decoded {
        image: small,
        source_width,
        source_height,
    })
}

/// The size of a `width` × `height` image scaled down to fit `max_side` on its
/// long side, keeping the aspect ratio; a smaller image keeps its size. Both
/// sides stay at least 1.
#[must_use]
pub fn fit(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= max_side {
        return (width, height);
    }
    let scale = |side: u32| {
        let scaled =
            (u64::from(side) * u64::from(max_side) + u64::from(long) / 2) / u64::from(long);
        u32::try_from(scaled).unwrap_or(max_side).clamp(1, max_side)
    };
    if width >= height {
        (max_side, scale(height))
    } else {
        (scale(width), max_side)
    }
}

/// The decode limits (`image`'s own checks, on top of ours).
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

/// Sniffs the type and measures the remaining length, then rewinds.
fn probe<R: BufRead + Seek>(reader: &mut R) -> Result<(Option<MediaKind>, u64), RenderError> {
    let start = reader.stream_position()?;
    let end = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(start))?;
    let mut head = Vec::with_capacity(SNIFF_LEN);
    reader
        .by_ref()
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)?;
    reader.seek(SeekFrom::Start(start))?;
    Ok((MediaKind::sniff(&head), end.saturating_sub(start)))
}

fn decode_error(err: ImageError, kind: Option<MediaKind>) -> RenderError {
    match err {
        ImageError::Limits(_) => RenderError::TooLarge,
        ImageError::Unsupported(_) => RenderError::Unsupported(kind),
        ImageError::IoError(e) if e.kind() != io::ErrorKind::UnexpectedEof => RenderError::Io(e),
        other => RenderError::Corrupt(other.to_string()),
    }
}

/// Whether the orientation turns the image by a quarter: displayed width and
/// height are then the stored height and width.
fn swaps_axes(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// Resizes packed 8-bit RGB or RGBA pixels; alpha is premultiplied for the
/// filter and restored after.
fn resize(
    pixels: Vec<u8>,
    pixel_type: PixelType,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<Vec<u8>, RenderError> {
    if from == to {
        return Ok(pixels);
    }
    let output = |e: &dyn std::fmt::Display| RenderError::Output(format!("resize: {e}"));
    let source =
        ResizeImage::from_vec_u8(from.0, from.1, pixels, pixel_type).map_err(|e| output(&e))?;
    let mut target = ResizeImage::new(to.0, to.1, pixel_type);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&source, &mut target, &options)
        .map_err(|e| output(&e))?;
    Ok(target.into_vec())
}

fn encode_webp(image: &DynamicImage, quality: f32) -> Result<Vec<u8>, RenderError> {
    let mut config = webp::WebPConfig::new()
        .map_err(|()| RenderError::Output("libwebp refused its default configuration".into()))?;
    config.quality = quality;
    config.method = WEBP_METHOD;
    let (width, height) = (image.width(), image.height());
    let encoder = match image {
        DynamicImage::ImageRgba8(buffer) => {
            webp::Encoder::from_rgba(buffer.as_raw(), width, height)
        }
        DynamicImage::ImageRgb8(buffer) => webp::Encoder::from_rgb(buffer.as_raw(), width, height),
        _ => return Err(RenderError::Output("unexpected pixel layout".into())),
    };
    let encoded = encoder
        .encode_advanced(&config)
        .map_err(|e| RenderError::Output(format!("libwebp: {e:?}")))?;
    Ok(encoded.to_vec())
}

/// Encodes `image` as a baseline JPEG at `quality` (1–100). An image with
/// alpha is flattened onto white, since JPEG has no alpha channel.
fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, RenderError> {
    use image::{ExtendedColorType, ImageEncoder as _};
    let rgb = match image {
        DynamicImage::ImageRgb8(buffer) => buffer.clone(),
        other => other.to_rgb8(),
    };
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|e| RenderError::Output(format!("jpeg: {e}")))?;
    Ok(out.into_inner())
}

fn thumbhash(image: &DynamicImage) -> Result<Vec<u8>, RenderError> {
    let from = (image.width(), image.height());
    let to = fit(from.0, from.1, THUMBHASH_SIDE);
    let rgba = resize(image.to_rgba8().into_raw(), PixelType::U8x4, from, to)?;
    // `fit` keeps both sides in 1..=100, as `rgba_to_thumb_hash` requires.
    Ok(thumbhash::rgba_to_thumb_hash(
        to.0 as usize,
        to.1 as usize,
        &rgba,
    ))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn fit_keeps_the_aspect_ratio_and_never_upscales(
            width in 1u32..=20_000,
            height in 1u32..=20_000,
            max_side in 1u32..=2_048,
        ) {
            let (w, h) = fit(width, height, max_side);
            prop_assert!(w >= 1 && h >= 1 && w <= width && h <= height);
            prop_assert_eq!(w.max(h), max_side.min(width.max(height)));
            // The short side is off by at most one pixel of rounding.
            let skew = (u64::from(w) * u64::from(height)).abs_diff(u64::from(h) * u64::from(width));
            prop_assert!(skew <= u64::from(width.max(height)));
        }
    }

    #[test]
    fn fit_scales_the_long_side_down_and_never_up() {
        assert_eq!(fit(1600, 900, 480), (480, 270));
        assert_eq!(fit(900, 1600, 480), (270, 480));
        assert_eq!(fit(1080, 1350, 480), (384, 480));
        assert_eq!(fit(480, 480, 480), (480, 480));
        assert_eq!(fit(481, 481, 480), (480, 480));
        assert_eq!(fit(300, 200, 480), (300, 200));
        assert_eq!(fit(1, 1, 480), (1, 1));
        assert_eq!(fit(20_000, 1, 480), (480, 1));
        assert_eq!(fit(1, 16_384, 100), (1, 100));
        assert_eq!(fit(16_384, 16_384, 100), (100, 100));
    }

    #[test]
    fn masters_follow_the_media_policy() {
        assert_eq!(RenderSpec::MASTER.max_side, 2_048);
        assert!((RenderSpec::MASTER.quality - 82.0).abs() < f32::EPSILON);
        assert_eq!(RenderSpec::POSTER.max_side, 1_080);
        assert!((RenderSpec::POSTER.quality - 78.0).abs() < f32::EPSILON);
        assert!(keeps_original(MediaKind::Jpeg, 1_500_000, 2_048, 1_536));
        assert!(!keeps_original(MediaKind::Jpeg, 1_500_001, 1_080, 1_350));
        assert!(!keeps_original(MediaKind::Png, 900_000, 2_049, 100));
        assert!(
            keeps_original(MediaKind::Avif, 9_000_000, 4_000, 3_000),
            "not decodable: kept"
        );
    }

    #[test]
    fn quarter_turns_swap_the_axes() {
        assert!(swaps_axes(Orientation::Rotate90));
        assert!(swaps_axes(Orientation::Rotate270FlipH));
        assert!(!swaps_axes(Orientation::Rotate180));
        assert!(!swaps_axes(Orientation::FlipHorizontal));
        assert!(!swaps_axes(Orientation::NoTransforms));
    }
}
