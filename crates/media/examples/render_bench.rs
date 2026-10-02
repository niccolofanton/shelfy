//! Times the image pipeline in a release build (plan §6.2): per-image render
//! time (decode, resize, WebP, ThumbHash), `g480` sizes, the throughput of
//! the 2-thread pool and the cost of storing an object.
//!
//! ```sh
//! cargo run -p shelfy-media --release --example render_bench
//! # Real images instead of synthetic ones; prints aggregates only:
//! cargo run -p shelfy-media --release --example render_bench -- --dir <images>
//! ```
//!
//! The synthetic images are photo-like (smooth waves plus fine noise) at the
//! sizes the archive sees: Instagram covers, X `name=large`, phone photos and
//! PNG screenshots.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder as _, RgbImage};
use shelfy_media::pool::ImagePool;
use shelfy_media::render::{self, RenderSpec, Rendered};
use shelfy_media::store::{IngestLimits, MediaStore};

/// Images per synthetic class.
const PER_CLASS: u64 = 24;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sources = match args.iter().position(|a| a == "--dir") {
        Some(i) => vec![(
            "files of --dir".to_owned(),
            read_dir(Path::new(&args[i + 1])),
        )],
        None => synthetic(),
    };
    println!(
        "{:<28} {:>4} {:>9} {:>22} {:>16} {:>6}",
        "class", "n", "source KB", "render ms p50/p95/max", "g480 KB p50/p95", "hash B"
    );
    let mut all = Vec::new();
    for (class, images) in &sources {
        let mut times = Vec::new();
        let mut sizes = Vec::new();
        let mut hash_len = 0;
        let mut failed = 0;
        for bytes in images {
            let started = Instant::now();
            match render::render_bytes(bytes, RenderSpec::G480) {
                Ok(Rendered {
                    webp, thumbhash, ..
                }) => {
                    times.push(started.elapsed());
                    sizes.push(webp.len());
                    hash_len = hash_len.max(thumbhash.len());
                }
                Err(_) => failed += 1,
            }
        }
        let source_kb =
            images.iter().map(Vec::len).sum::<usize>() as f64 / images.len().max(1) as f64 / 1024.0;
        println!(
            "{:<28} {:>4} {:>9.0} {:>22} {:>16} {:>6}{}",
            class,
            times.len(),
            source_kb,
            format!(
                "{:.1} / {:.1} / {:.1}",
                ms(pct(&times, 50)),
                ms(pct(&times, 95)),
                ms(pct(&times, 100))
            ),
            format!("{:.1} / {:.1}", kb(pct(&sizes, 50)), kb(pct(&sizes, 95))),
            hash_len,
            if failed > 0 {
                format!("  ({failed} not renderable)")
            } else {
                String::new()
            }
        );
        all.extend(images.iter().cloned());
    }

    // Throughput: everything through the shared 2-thread pool at once.
    let pool = ImagePool::shared();
    let started = Instant::now();
    std::thread::scope(|scope| {
        for bytes in &all {
            scope.spawn(move || {
                let _ = pool.run_blocking(|| render::render_bytes(bytes, RenderSpec::G480));
            });
        }
    });
    let wall = started.elapsed();
    println!(
        "pool ({} threads): {} images in {:.2} s = {:.1} images/s",
        pool.threads(),
        all.len(),
        wall.as_secs_f64(),
        all.len() as f64 / wall.as_secs_f64()
    );

    // Storing: hash while writing, fsync, rename, fsync the directory.
    let dir = tempfile::tempdir().expect("temp dir");
    let media = MediaStore::new(dir.path())
        .user("bench")
        .expect("valid user id");
    let mut times = Vec::new();
    for bytes in all.iter().take(48) {
        let started = Instant::now();
        let staged = media
            .ingest(bytes.as_slice(), IngestLimits::UPLOAD)
            .expect("ingest");
        staged.publish().expect("publish");
        times.push(started.elapsed());
    }
    println!(
        "ingest + publish: p50 {:.1} ms, p95 {:.1} ms ({} objects)",
        ms(pct(&times, 50)),
        ms(pct(&times, 95)),
        times.len()
    );
}

fn synthetic() -> Vec<(String, Vec<Vec<u8>>)> {
    let classes: [(&str, u32, u32, bool); 5] = [
        ("IG cover 1080x1350 jpg", 1080, 1350, false),
        ("IG square 1080x1080 jpg", 1080, 1080, false),
        ("X large 2048x1536 jpg", 2048, 1536, false),
        ("phone 4032x3024 jpg", 4032, 3024, false),
        ("screenshot 1440x900 png", 1440, 900, true),
    ];
    classes
        .iter()
        .map(|&(name, width, height, as_png)| {
            let images = (0..PER_CLASS)
                .map(|seed| {
                    let image = photo(width, height, seed + u64::from(width));
                    if as_png { png(&image) } else { jpeg(&image) }
                })
                .collect();
            (name.to_owned(), images)
        })
        .collect()
}

fn read_dir(dir: &Path) -> Vec<Vec<u8>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("readable directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files.iter().filter_map(|p| std::fs::read(p).ok()).collect()
}

/// A photo-like image: overlapping smooth waves per channel plus fine noise.
fn photo(width: u32, height: u32, seed: u64) -> RgbImage {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let phase = (seed % 97) as f32 / 10.0;
    RgbImage::from_fn(width, height, |x, y| {
        let (x, y) = (x as f32, y as f32);
        let mut pixel = [0u8; 3];
        for (c, value) in pixel.iter_mut().enumerate() {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let noise = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as f32 / 255.0 - 0.5;
            let c = c as f32;
            let level = 128.0
                + 60.0 * (x * 0.006 + y * 0.004 + c + phase).sin()
                + 30.0 * (x * 0.031 - y * 0.023 + c * 2.0).sin()
                + 12.0 * (x * 0.17 + y * 0.21 + phase * c).sin()
                + 10.0 * noise;
            *value = level.clamp(0.0, 255.0) as u8;
        }
        image::Rgb(pixel)
    })
}

fn jpeg(image: &RgbImage) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, 85)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .expect("encode JPEG");
    out.into_inner()
}

fn png(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .expect("encode PNG");
    out
}

/// The `p`-th percentile (nearest rank) of `values`.
fn pct<T: Copy + Ord + Default>(values: &[T], p: usize) -> T {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return T::default();
    }
    let rank = (p * sorted.len()).div_ceil(100).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn kb(bytes: usize) -> f64 {
    bytes as f64 / 1024.0
}
