//! The image pipeline on synthetic images: rendition sizes, EXIF orientation,
//! animation, alpha, ThumbHash, corrupt and oversized input, and the image
//! pool.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use image::{DynamicImage, GenericImageView as _, RgbaImage};
use shelfy_media::MediaKind;
use shelfy_media::pool::{ImagePool, JobPanicked};
use shelfy_media::render::{self, RenderError, RenderSpec, Rendered};
use support::{
    BLUE, GREEN, RED, assert_near, decode_webp, gif, gif_claiming, halves, jpeg, jpeg_claiming,
    photo, png, png_claiming, solid, webp_animated, webp_lossless,
};

const G480: RenderSpec = RenderSpec::G480;

fn render(bytes: &[u8]) -> Rendered {
    render::render_bytes(bytes, G480).expect("render")
}

fn rgb_at(image: &DynamicImage, x: u32, y: u32) -> Vec<u8> {
    image.get_pixel(x, y).0[..3].to_vec()
}

#[test]
fn renditions_fit_480_on_the_long_side_and_never_upscale() {
    let cases = [
        ((1600, 900), (480, 270)),
        ((900, 1600), (270, 480)),
        ((1080, 1350), (384, 480)),
        ((2048, 2048), (480, 480)),
        ((480, 480), (480, 480)),
        ((300, 200), (300, 200)),
        ((1, 1), (1, 1)),
    ];
    for ((width, height), expected) in cases {
        let source = png(&DynamicImage::ImageRgb8(halves(width, height, RED, BLUE)));
        let out = render(&source);
        assert_eq!((out.source_width, out.source_height), (width, height));
        assert_eq!((out.width, out.height), expected, "{width}x{height}");
        let webp = decode_webp(&out.webp);
        assert_eq!(webp.dimensions(), expected, "WebP of {width}x{height}");
        assert_eq!(MediaKind::sniff(&out.webp), Some(MediaKind::Webp));
    }
}

#[test]
fn every_decodable_format_renders() {
    let picture = photo(640, 400, 3);
    let rgba = DynamicImage::ImageRgb8(picture.clone()).to_rgba8();
    let sources = [
        ("jpeg", jpeg(&picture, 90, None)),
        ("png", png(&DynamicImage::ImageRgb8(picture.clone()))),
        ("gif", gif(std::slice::from_ref(&rgba))),
        ("webp", webp_lossless(&rgba)),
    ];
    for (name, bytes) in sources {
        let out = render(&bytes);
        assert_eq!((out.width, out.height), (480, 300), "{name}");
        assert!(!out.webp.is_empty() && out.thumbhash.len() <= 25, "{name}");
    }
}

#[test]
fn exif_orientation_is_applied() {
    // Stored: left half red, right half blue. EXIF 6 turns it a quarter
    // clockwise (red on top), 8 a quarter counter-clockwise (red at the
    // bottom), 3 a half turn (red on the right); 2 mirrors it.
    let stored = halves(200, 100, RED, BLUE);
    // (EXIF value, displayed size, [(probe point, expected color); 2])
    type Case = (u16, (u32, u32), [Probe; 2]);
    type Probe = ((u32, u32), [u8; 3]);
    let cases: [Case; 5] = [
        (1, (200, 100), [((40, 50), RED), ((160, 50), BLUE)]),
        (6, (100, 200), [((50, 40), RED), ((50, 160), BLUE)]),
        (8, (100, 200), [((50, 40), BLUE), ((50, 160), RED)]),
        (3, (200, 100), [((40, 50), BLUE), ((160, 50), RED)]),
        (2, (200, 100), [((40, 50), BLUE), ((160, 50), RED)]),
    ];
    for (orientation, size, probes) in cases {
        let out = render(&jpeg(&stored, 95, Some(orientation)));
        assert_eq!(
            (out.source_width, out.source_height),
            size,
            "EXIF {orientation}"
        );
        assert_eq!((out.width, out.height), size, "EXIF {orientation}");
        let webp = decode_webp(&out.webp);
        for ((x, y), color) in probes {
            assert_near(
                &rgb_at(&webp, x, y),
                &color,
                40,
                &format!("EXIF {orientation} at {x},{y}"),
            );
        }
    }
}

#[test]
fn a_rotated_photo_is_resized_in_its_displayed_orientation() {
    let out = render(&jpeg(&halves(1200, 600, RED, BLUE), 90, Some(6)));
    assert_eq!((out.source_width, out.source_height), (600, 1200));
    assert_eq!((out.width, out.height), (240, 480));
    let webp = decode_webp(&out.webp);
    assert_near(&rgb_at(&webp, 120, 60), &RED, 40, "top");
    assert_near(&rgb_at(&webp, 120, 420), &BLUE, 40, "bottom");
}

#[test]
fn animated_images_render_their_first_frame() {
    let frames = [
        solid(60, 40, RED),
        solid(60, 40, BLUE),
        solid(60, 40, GREEN),
    ];
    for (name, bytes) in [("gif", gif(&frames)), ("webp", webp_animated(&frames))] {
        let out = render(&bytes);
        assert_eq!((out.width, out.height), (60, 40), "{name}");
        let webp = decode_webp(&out.webp);
        assert_near(&rgb_at(&webp, 30, 20), &RED, 40, name);
    }
}

#[test]
fn transparency_is_kept() {
    let image = RgbaImage::from_fn(100, 50, |x, _| {
        image::Rgba(if x < 50 {
            [0, 0, 0, 0]
        } else {
            [20, 20, 230, 255]
        })
    });
    let out = render(&png(&DynamicImage::ImageRgba8(image)));
    let webp = decode_webp(&out.webp);
    assert!(webp.color().has_alpha());
    assert!(webp.get_pixel(10, 25).0[3] < 20, "left stays transparent");
    assert_eq!(webp.get_pixel(90, 25).0[3], 255, "right stays opaque");
    let (_, _, _, alpha) = thumbhash::thumb_hash_to_average_rgba(&out.thumbhash).unwrap();
    assert!((0.3..0.7).contains(&alpha), "average alpha {alpha}");
}

#[test]
fn the_thumbhash_round_trips() {
    let out = render(&png(&DynamicImage::ImageRgb8(halves(600, 400, RED, BLUE))));
    assert!(out.thumbhash.len() <= 25, "{} bytes", out.thumbhash.len());

    let ratio = thumbhash::thumb_hash_to_approximate_aspect_ratio(&out.thumbhash).unwrap();
    assert!((ratio - 1.5).abs() < 0.2, "aspect ratio {ratio}");
    let (r, g, b, a) = thumbhash::thumb_hash_to_average_rgba(&out.thumbhash).unwrap();
    assert!(
        (r - 0.5).abs() < 0.1 && g < 0.15 && (b - 0.5).abs() < 0.1,
        "average {r} {g} {b}"
    );
    assert!((a - 1.0).abs() < f32::EPSILON, "opaque");

    let (width, height, rgba) = thumbhash::thumb_hash_to_rgba(&out.thumbhash).unwrap();
    assert!(width > height, "{width}x{height}");
    let pixel = |x: usize, y: usize| rgba[(y * width + x) * 4..][..3].to_vec();
    let (left, right) = (
        pixel(width / 8, height / 2),
        pixel(width * 7 / 8, height / 2),
    );
    assert!(left[0] > left[2] + 60, "left is red: {left:?}");
    assert!(right[2] > right[0] + 60, "right is blue: {right:?}");
}

#[test]
fn the_same_image_renders_the_same_from_bytes_and_from_a_file() {
    let bytes = jpeg(&photo(900, 700, 11), 85, None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.jpg");
    std::fs::write(&path, &bytes).unwrap();
    let from_file = render::render_file(&path, G480).unwrap();
    assert_eq!(from_file, render(&bytes));
}

#[test]
fn corrupt_input_is_an_error_not_a_panic() {
    let valid_jpeg = jpeg(&photo(300, 200, 5), 90, None);
    let valid_png = png(&DynamicImage::ImageRgb8(photo(300, 200, 5)));
    let mut garbage_png = valid_png.clone();
    for byte in &mut garbage_png[60..] {
        *byte = byte.wrapping_mul(31).wrapping_add(7);
    }
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("jpeg header only", valid_jpeg[..120].to_vec()),
        (
            "png cut in its data",
            valid_png[..valid_png.len() / 2].to_vec(),
        ),
        ("png with scrambled data", garbage_png),
        (
            "gif magic and garbage",
            b"GIF89a\x10\x00\x10\x00\xff\xff\xff\xff".repeat(4),
        ),
        (
            "webp magic and garbage",
            b"RIFF\x40\0\0\0WEBPVP8 \x30\0\0\0garbage".to_vec(),
        ),
        ("jpeg magic only", vec![0xFF, 0xD8, 0xFF]),
    ];
    for (name, bytes) in cases {
        match render::render_bytes(&bytes, G480) {
            Err(RenderError::Corrupt(_)) => {}
            other => panic!("{name}: expected Corrupt, got {other:?}"),
        }
    }
}

#[test]
fn other_types_are_refused() {
    let cases: [(&[u8], Option<MediaKind>); 4] = [
        (b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n", Some(MediaKind::Pdf)),
        (b"\0\0\0\x18ftypavif\0\0\0\0mif1miaf", Some(MediaKind::Avif)),
        (b"\0\0\0\x14ftypisom\0\0\x02\0isom", Some(MediaKind::Mp4)),
        (b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", None),
    ];
    for (bytes, kind) in cases {
        match render::render_bytes(bytes, G480) {
            Err(RenderError::Unsupported(found)) => assert_eq!(found, kind),
            other => panic!("{kind:?}: expected Unsupported, got {other:?}"),
        }
    }
}

#[test]
fn oversized_images_are_refused_from_their_header() {
    // Each claims more pixels than the limits allow; none of them could be
    // decoded in the memory the pipeline has, so the header check must stop it.
    let cases = [
        ("png 10000x10000 (100 MP)", png_claiming(10_000, 10_000)),
        ("png 20000x10", png_claiming(20_000, 10)),
        ("gif 60000x60000", gif_claiming(60_000, 60_000)),
        ("jpeg 60000x60000", jpeg_claiming(60_000, 60_000)),
    ];
    for (name, bytes) in cases {
        match render::render_bytes(&bytes, G480) {
            Err(RenderError::TooLarge) => {}
            other => panic!("{name}: expected TooLarge, got {other:?}"),
        }
    }
}

#[test]
fn the_pool_runs_jobs_on_two_named_threads() {
    let pool = ImagePool::new(ImagePool::THREADS).unwrap();
    assert_eq!(pool.threads(), 2);
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let names: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (pool, running, peak) = (&pool, Arc::clone(&running), Arc::clone(&peak));
                scope.spawn(move || {
                    pool.run_blocking(|| {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(30));
                        running.fetch_sub(1, Ordering::SeqCst);
                        std::thread::current().name().unwrap_or_default().to_owned()
                    })
                    .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "at most and at least two at once"
    );
    assert!(
        names.iter().all(|n| n.starts_with("shelfy-image-")),
        "{names:?}"
    );
}

#[tokio::test]
async fn the_pool_survives_a_panicking_job() {
    let pool = ImagePool::new(1).unwrap();
    let result: Result<(), JobPanicked> = pool.run(|| panic!("decoder bug")).await;
    assert!(result.is_err());
    assert_eq!(pool.run(|| 41 + 1).await.unwrap(), 42);
    assert!(pool.run_blocking(|| panic!("again")).is_err());
    assert_eq!(pool.run_blocking(|| "alive").unwrap(), "alive");
}

#[tokio::test]
async fn the_shared_pool_renders_without_blocking_the_runtime() {
    let pool = ImagePool::shared();
    assert_eq!(pool.threads(), ImagePool::THREADS);
    let bytes: Arc<[u8]> = jpeg(&photo(1080, 1350, 9), 85, None).into();
    let out = pool.render_bytes(Arc::clone(&bytes), G480).await.unwrap();
    assert_eq!((out.width, out.height), (384, 480));
    match pool
        .render_bytes(Arc::from(&b"not an image"[..]), G480)
        .await
    {
        Err(RenderError::Unsupported(None)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
}
