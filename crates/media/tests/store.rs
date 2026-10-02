//! The per-user store: layout, dedupe, atomic writes, ingest limits, async
//! ingest, removal and the sweep of interrupted writes.

mod support;

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use image::DynamicImage;
use shelfy_media::kind::{KindSet, SNIFF_LEN};
use shelfy_media::store::{IngestError, IngestLimits, MediaStore, TEMP_DIR};
use shelfy_media::{Digest, MediaKind, ObjectName, Rendition};
use support::{BLUE, OTHER_USER, RED, TempStore, USER, halves, jpeg, photo, png};

/// Files under `dir`, recursively, relative to it.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            for file in files(&path) {
                out.push(format!(
                    "{}/{file}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        } else {
            out.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

fn temp_files(media: &shelfy_media::store::UserMedia) -> Vec<String> {
    files(&media.root().join(TEMP_DIR))
}

#[test]
fn objects_are_stored_under_their_digest_as_the_plan_lays_out() {
    let t = TempStore::new();
    let media = t.user(USER);
    let bytes = jpeg(&photo(64, 48, 1), 90, None);
    let digest = Digest::of(&bytes);

    let stored = media
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap()
        .publish()
        .unwrap();
    assert_eq!(stored.digest, digest);
    assert_eq!(stored.kind, MediaKind::Jpeg);
    assert_eq!(stored.size, bytes.len() as u64);
    assert!(!stored.deduplicated);

    let hex = digest.to_string();
    let expected = t
        .dir
        .path()
        .join("users")
        .join(USER)
        .join("media")
        .join(&hex[..2])
        .join(format!("{hex}.jpg"));
    assert_eq!(media.object_path(&digest, MediaKind::Jpeg), expected);
    assert_eq!(media.path(&stored.name()), expected);
    assert_eq!(fs::read(&expected).unwrap(), bytes);
    assert!(media.contains(&digest, MediaKind::Jpeg));
    assert!(!media.contains(&digest, MediaKind::Png));
    assert_eq!(
        media.rendition_path(&digest, Rendition::G480),
        expected.with_file_name(format!("{hex}.g480.webp"))
    );
    assert!(temp_files(&media).is_empty(), "no temporary file is left");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&expected), 0o640, "object file");
        assert_eq!(mode(expected.parent().unwrap()), 0o750, "shard directory");
        assert_eq!(mode(media.root()), 0o750, "media directory");
    }
}

#[test]
fn the_same_content_is_stored_once() {
    let t = TempStore::new();
    let media = t.user(USER);
    let bytes = png(&DynamicImage::ImageRgb8(halves(40, 30, RED, BLUE)));
    let first = media
        .ingest(bytes.as_slice(), IngestLimits::UPLOAD)
        .unwrap()
        .publish()
        .unwrap();
    let mtime = fs::metadata(media.path(&first.name()))
        .unwrap()
        .modified()
        .unwrap();
    let second = media
        .ingest(bytes.as_slice(), IngestLimits::UPLOAD)
        .unwrap()
        .publish()
        .unwrap();
    assert!(second.deduplicated);
    assert_eq!(first.digest, second.digest);
    let hex = first.digest.to_string();
    assert_eq!(
        files(media.root()),
        vec![format!("{}/{hex}.png", &hex[..2])]
    );
    let unchanged = fs::metadata(media.path(&first.name()))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(mtime, unchanged, "the stored file was not rewritten");
}

#[test]
fn concurrent_publishes_of_the_same_content_converge() {
    let t = TempStore::new();
    let media = t.user(USER);
    let bytes = jpeg(&photo(200, 150, 12), 90, None);
    let stored: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    media
                        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
                        .unwrap()
                        .publish()
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(stored.iter().all(|s| s.digest == Digest::of(&bytes)));
    assert_eq!(fs::read(media.path(&stored[0].name())).unwrap(), bytes);
    assert_eq!(files(media.root()).len(), 1, "one object file");
    assert!(temp_files(&media).is_empty());
}

#[test]
fn a_damaged_copy_is_replaced_on_the_next_publish() {
    let t = TempStore::new();
    let media = t.user(USER);
    let bytes = jpeg(&photo(80, 60, 2), 90, None);
    let path = media.object_path(&Digest::of(&bytes), MediaKind::Jpeg);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap(); // truncated by a crash or a disk error

    let stored = media
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap()
        .publish()
        .unwrap();
    assert!(!stored.deduplicated);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn nothing_reaches_the_final_name_before_publish() {
    let t = TempStore::new();
    let media = t.user(USER);
    let bytes = jpeg(&photo(300, 200, 3), 90, None);
    let final_path = media.object_path(&Digest::of(&bytes), MediaKind::Jpeg);

    // A writer that is dropped half way leaves nothing behind.
    let mut writer = media.writer(IngestLimits::ARCHIVE_IMAGE).unwrap();
    writer.write_chunk(&bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(
        temp_files(&media).len(),
        1,
        "the bytes go to a temporary file"
    );
    assert!(!final_path.exists());
    drop(writer);
    assert!(temp_files(&media).is_empty());

    // A staged object is complete but invisible until it is published.
    let mut writer = media.writer(IngestLimits::ARCHIVE_IMAGE).unwrap();
    for chunk in bytes.chunks(1000) {
        writer.write_chunk(chunk).unwrap();
    }
    assert_eq!(writer.size(), bytes.len() as u64);
    let staged = writer.finish().unwrap();
    assert_eq!(fs::read(staged.path()).unwrap(), bytes);
    assert!(staged.path().starts_with(media.root().join(TEMP_DIR)));
    assert!(!final_path.exists());
    drop(staged);
    assert!(temp_files(&media).is_empty());
    assert!(!final_path.exists());

    // Published, the file appears complete in one rename.
    let staged = media
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap();
    assert_eq!(
        staged.name(),
        ObjectName::original(Digest::of(&bytes), MediaKind::Jpeg)
    );
    staged.publish().unwrap();
    assert_eq!(fs::read(&final_path).unwrap(), bytes);
    assert!(temp_files(&media).is_empty());
}

#[test]
fn ingest_enforces_size_and_type() {
    let t = TempStore::new();
    let media = t.user(USER);
    let image = jpeg(&photo(100, 100, 4), 90, None);
    let exact = IngestLimits {
        max_bytes: image.len() as u64,
        accept: KindSet::IMAGES,
    };
    assert!(
        media.ingest(image.as_slice(), exact).is_ok(),
        "the limit itself is allowed"
    );
    let one_less = IngestLimits {
        max_bytes: image.len() as u64 - 1,
        ..exact
    };
    match media.ingest(image.as_slice(), one_less) {
        Err(IngestError::TooLarge { limit }) => assert_eq!(limit, image.len() as u64 - 1),
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert!(matches!(
        media.ingest(&b""[..], IngestLimits::UPLOAD),
        Err(IngestError::Empty)
    ));
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    assert!(matches!(
        media.ingest(&svg[..], IngestLimits::UPLOAD),
        Err(IngestError::UnknownType)
    ));
    assert!(matches!(
        media.ingest(&b"%PDF-1.7\n"[..], IngestLimits::ARCHIVE_IMAGE),
        Err(IngestError::NotAccepted(MediaKind::Pdf))
    ));
    assert!(matches!(
        media.ingest(image.as_slice(), IngestLimits::VIDEO),
        Err(IngestError::NotAccepted(MediaKind::Jpeg))
    ));
    // A short file is sniffed from what it has.
    let tiny_pdf = media
        .ingest(&b"%PDF-1.4"[..], IngestLimits::UPLOAD)
        .unwrap();
    assert_eq!(tiny_pdf.kind(), MediaKind::Pdf);
    drop(tiny_pdf);
    assert!(
        temp_files(&media).is_empty(),
        "refused ingests leave nothing behind"
    );
}

#[test]
fn a_refused_type_fails_as_soon_as_it_can_be_sniffed() {
    let t = TempStore::new();
    let media = t.user(USER);
    let mut writer = media.writer(IngestLimits::ARCHIVE_IMAGE).unwrap();
    let html = b"<!DOCTYPE html><html><head><title>not an image</title></head><body>";
    assert!(html.len() > SNIFF_LEN);
    writer.write_chunk(&html[..10]).unwrap(); // too early to tell
    assert!(matches!(
        writer.write_chunk(&html[10..]),
        Err(IngestError::UnknownType)
    ));
}

#[tokio::test]
async fn async_ingest_matches_the_blocking_one() {
    let t = TempStore::new();
    let media = t.user(USER);
    // Larger than one async batch, so it is written in several.
    let mut bytes = jpeg(&photo(64, 64, 6), 90, None);
    bytes.resize(3 * 1024 * 1024 + 123, 0); // trailing bytes after the JPEG end
    let staged = media
        .ingest_async(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .await
        .unwrap();
    assert_eq!(staged.digest(), Digest::of(&bytes));
    assert_eq!(staged.size(), bytes.len() as u64);
    let stored = staged.publish().unwrap();
    assert_eq!(fs::read(media.path(&stored.name())).unwrap(), bytes);

    let small = IngestLimits {
        max_bytes: 1024 * 1024,
        accept: KindSet::IMAGES,
    };
    assert!(matches!(
        media.ingest_async(bytes.as_slice(), small).await,
        Err(IngestError::TooLarge { .. })
    ));
    let html = b"<html>".repeat(100);
    assert!(matches!(
        media
            .ingest_async(html.as_slice(), IngestLimits::UPLOAD)
            .await,
        Err(IngestError::UnknownType)
    ));
    assert!(matches!(
        media.ingest_async(&b""[..], IngestLimits::UPLOAD).await,
        Err(IngestError::Empty)
    ));
    assert!(temp_files(&media).is_empty());
}

#[tokio::test]
async fn async_ingest_reports_read_errors() {
    struct Failing;
    impl tokio::io::AsyncRead for Failing {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset")))
        }
    }
    let t = TempStore::new();
    let media = t.user(USER);
    match media.ingest_async(Failing, IngestLimits::UPLOAD).await {
        Err(IngestError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
        other => panic!("expected Io, got {other:?}"),
    }
    assert!(temp_files(&media).is_empty());
}

#[test]
fn renditions_are_written_atomically_and_replaced() {
    let t = TempStore::new();
    let media = t.user(USER);
    let digest = Digest::of(b"source");
    media
        .store_rendition(&digest, Rendition::G480, b"first")
        .unwrap();
    media
        .store_rendition(&digest, Rendition::G480, b"second")
        .unwrap();
    let path = media.rendition_path(&digest, Rendition::G480);
    assert_eq!(fs::read(&path).unwrap(), b"second");
    assert!(temp_files(&media).is_empty());
}

#[test]
fn remove_deletes_the_object_and_its_renditions() {
    let t = TempStore::new();
    let media = t.user(USER);
    let bytes = jpeg(&photo(32, 32, 8), 90, None);
    let stored = media
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap()
        .publish()
        .unwrap();
    media
        .store_rendition(&stored.digest, Rendition::G480, b"webp")
        .unwrap();
    assert!(media.remove(&stored.digest, stored.kind).unwrap());
    assert!(!media.contains(&stored.digest, stored.kind));
    assert!(
        !media
            .rendition_path(&stored.digest, Rendition::G480)
            .exists()
    );
    assert!(
        !media.remove(&stored.digest, stored.kind).unwrap(),
        "idempotent"
    );
}

#[test]
fn the_sweep_removes_only_stale_temporary_files() {
    let t = TempStore::new();
    let media = t.user(USER);
    assert_eq!(
        media.sweep_temp(Duration::from_secs(3600)).unwrap(),
        0,
        "no directory yet"
    );

    // Two interrupted writes: one from two days ago, one in progress.
    fs::create_dir_all(media.root().join(TEMP_DIR)).unwrap();
    let old = media.root().join(TEMP_DIR).join("obj-crashed.part");
    fs::write(&old, b"partial").unwrap();
    let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 86_400);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();
    let mut live = media.writer(IngestLimits::UPLOAD).unwrap();
    live.write_chunk(b"%PDF-1.7 in progress").unwrap();

    assert_eq!(media.sweep_temp(Duration::from_secs(86_400)).unwrap(), 1);
    assert!(!old.exists());
    assert_eq!(temp_files(&media).len(), 1, "the live write stays");
    assert!(live.finish().is_ok());
}

#[test]
fn every_user_has_a_separate_store() {
    let t = TempStore::new();
    let (mine, theirs) = (t.user(USER), t.user(OTHER_USER));
    assert_ne!(mine.root(), theirs.root());
    let bytes = jpeg(&photo(32, 32, 9), 90, None);
    let stored = mine
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap()
        .publish()
        .unwrap();
    assert!(mine.contains(&stored.digest, stored.kind));
    assert!(!theirs.contains(&stored.digest, stored.kind));

    // The same bytes saved by both users are two files.
    let copy = theirs
        .ingest(bytes.as_slice(), IngestLimits::ARCHIVE_IMAGE)
        .unwrap()
        .publish()
        .unwrap();
    assert!(!copy.deduplicated);
    mine.remove(&stored.digest, stored.kind).unwrap();
    assert!(theirs.contains(&copy.digest, copy.kind));
}

#[test]
fn user_ids_must_be_a_single_safe_path_component() {
    let store = MediaStore::new("/data/shelfy/users");
    for bad in [
        "",
        ".",
        "..",
        "../x",
        "a/b",
        "a\\b",
        "user id",
        "é",
        &"a".repeat(65),
    ] {
        assert!(store.user(bad).is_err(), "{bad:?} was accepted");
    }
    assert!(store.user(USER).is_ok());
    assert!(store.user(&"a".repeat(64)).is_ok());
}
