//! Manifest-bound local cover recovery, on a synthetic owner only.
mod support;

use clap::Parser as _;
use image::codecs::jpeg::JpegEncoder;
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_core::repo::posts::{self, NewMedia, NewPost};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_media::{Digest, MediaKind};
use shelfy_server::admin::AdminCommand;
use shelfy_server::admin::backfill_covers::{self, apply_manifest, inspect_manifest};
use shelfy_server::cli::{Cli, Command};
use support::TestState;

fn bytes() -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new(&mut out)
        .encode_image(&image::RgbImage::from_pixel(
            40,
            30,
            image::Rgb([20, 90, 180]),
        ))
        .unwrap();
    out
}

async fn fixture() -> (TestState, String, tempfile::TempDir, Value) {
    let t = TestState::new();
    let user = support::auth::owner(&t);
    let db = t.state.user_db(&user).await.unwrap();
    db.write(|tx| {
        for n in 1..=2 {
            let mut p = NewPost::new(
                format!("ig_{n}"),
                Platform::Instagram,
                n.to_string(),
                "video",
                1,
            );
            p.shortcode = Some(format!("synthetic{n}"));
            p.caption = Some("synthetic caption stays".into());
            p.user_note = Some("manual note stays".into());
            p.cover_url = Some("https://scontent-example.cdninstagram.com/expired.jpg".into());
            p.cover_url_expires_at = Some(1);
            p.archive_state = Some("client".into());
            p.media = vec![NewMedia {
                kind: "video".into(),
                ..NewMedia::default()
            }];
            posts::insert(tx, &p, 1)?;
        }
        Ok::<_, shelfy_core::repo::RepoError>(())
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let image = bytes();
    std::fs::write(dir.path().join("poster.jpg"), &image).unwrap();
    let manifest = json!({"version":1,"userId":user,"entries":(1..=2).map(|n|json!({
        "postKey":format!("ig_{n}"),"nativeId":n.to_string(),"shortcode":format!("synthetic{n}"),"mediaType":"video",
        "file":"poster.jpg","ext":"jpg","sha256":Digest::of(&image).to_string(),"bytes":image.len()
    })).collect::<Vec<_>>()});
    (t, user, dir, manifest)
}

fn save(dir: &tempfile::TempDir, manifest: &Value) -> std::path::PathBuf {
    let p = dir.path().join("manifest.json");
    std::fs::write(&p, serde_json::to_vec(manifest).unwrap()).unwrap();
    p
}

#[tokio::test]
async fn dry_run_changes_no_files_and_apply_deduplicates_and_is_idempotent() {
    let (t, user, dir, manifest) = fixture().await;
    let path = save(&dir, &manifest);
    let db = t.state.user_db(&user).await.unwrap();
    let before = db.read(|c| Ok::<_, RepoError>(c.total_changes())).unwrap();
    let inspected = inspect_manifest(&t.data_dir(), &path).unwrap();
    assert_eq!(
        (inspected.entries, inspected.eligible, inspected.stored),
        (2, 2, 0)
    );
    assert!(!t.data_dir().users_dir().join(&user).join("media").exists());
    assert_eq!(
        before,
        db.read(|c| Ok::<_, RepoError>(c.total_changes())).unwrap()
    );
    let report = apply_manifest(&t.state, &path).await.unwrap();
    assert_eq!(report.stored, 2);
    assert!(report.added_bytes > 0);
    let (objects, stored_bytes) = db
        .read(|c| {
            c.query_row("SELECT count(*),sum(bytes) FROM media_objects", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(RepoError::from)
        })
        .unwrap();
    assert_eq!(objects, 1, "same source poster is one CAS master");
    assert_eq!(report.added_bytes, stored_bytes as u64);
    let usage = t
        .state
        .control()
        .read(|c| {
            c.query_row(
                "select usage_media_bytes from users where id=?1",
                [&user],
                |r| r.get::<_, i64>(0),
            )
            .map_err(RepoError::from)
        })
        .unwrap();
    assert_eq!(usage, stored_bytes);
    db.read(|c| {
        let mut s=c.prepare("select p.caption,p.user_note,p.archive_state,p.cover_object=m.object_id,p.ai_status from posts p join post_media m on m.post_id=p.id")?;
        for row in s.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,bool>(3)?,r.get::<_,Option<String>>(4)?)))? {
            let(caption,note,state,shared,ai)=row?;
            assert_eq!(caption,"synthetic caption stays");assert_eq!(note,"manual note stays");assert_eq!(state,"done");assert!(shared);assert!(ai.is_none());
        }
        Ok::<_,RepoError>(())
    }).unwrap();
    let again = apply_manifest(&t.state, &path).await.unwrap();
    assert_eq!(
        (again.stored, again.skipped_existing, again.added_bytes),
        (0, 2, 0)
    );
    assert_eq!(t.state.quota().reserved_total(), 0);
    let usage_again = t
        .state
        .control()
        .read(|c| {
            c.query_row(
                "select usage_media_bytes from users where id=?1",
                [&user],
                |r| r.get::<_, i64>(0),
            )
            .map_err(RepoError::from)
        })
        .unwrap();
    assert_eq!(usage_again, usage);
}

#[tokio::test]
async fn validation_is_all_before_write_and_errors_never_print_private_values() {
    let (t, user, dir, mut manifest) = fixture().await;
    let path = save(&dir, &manifest);
    manifest["entries"][1]["shortcode"] = "private-wrong-shortcode".into();
    save(&dir, &manifest);
    let err = apply_manifest(&t.state, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("private-wrong-shortcode"));
    assert!(!err.contains("ig_"));
    assert!(!err.contains(dir.path().to_str().unwrap()));
    let db = t.state.user_db(&user).await.unwrap();
    assert_eq!(
        db.read(|c| c
            .query_row("select count(*) from media_objects", [], |r| r
                .get::<_, i64>(0))
            .map_err(RepoError::from))
            .unwrap(),
        0
    );
    assert_eq!(t.state.quota().reserved_total(), 0);
    manifest["entries"][1]["shortcode"] = "synthetic2".into();
    manifest["entries"][1]["sha256"] = "0".repeat(64).into();
    save(&dir, &manifest);
    assert!(inspect_manifest(&t.data_dir(), &path).is_err());
    manifest["entries"][1]["sha256"] = Digest::of(&bytes()).to_string().into();
    manifest["userId"] = "different-owner".into();
    save(&dir, &manifest);
    assert!(inspect_manifest(&t.data_dir(), &path).is_err());
}

#[tokio::test]
async fn quota_failure_and_trash_cannot_attach_or_count_a_cover() {
    let (t, user, dir, manifest) = fixture().await;
    let path = save(&dir, &manifest);
    let db = t.state.user_db(&user).await.unwrap();
    db.write(|tx| {
        tx.execute("update posts set deleted_at=10 where key='ig_1'", [])
            .map_err(RepoError::from)
    })
    .unwrap();
    let dry = inspect_manifest(&t.data_dir(), &path).unwrap();
    assert_eq!((dry.eligible, dry.skipped_trashed), (1, 1));
    t.state
        .control()
        .write(|tx| {
            tx.execute("update users set quota_bytes=1 where id=?1", params![user])
                .map_err(RepoError::from)
        })
        .unwrap();
    assert!(apply_manifest(&t.state, &path).await.is_err());
    let count = db
        .read(|c| {
            c.query_row(
                "select count(*) from posts where cover_object is not null",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(RepoError::from)
        })
        .unwrap();
    assert_eq!(count, 0);
    let usage = t
        .state
        .control()
        .read(|c| {
            c.query_row(
                "select usage_media_bytes from users where id=?1",
                [&user],
                |r| r.get::<_, i64>(0),
            )
            .map_err(RepoError::from)
        })
        .unwrap();
    assert_eq!(usage, 0);
    assert_eq!(t.state.quota().reserved_total(), 0);
}

#[tokio::test]
async fn traversal_corrupt_images_and_header_type_mismatch_are_refused() {
    let (t, _user, dir, mut manifest) = fixture().await;
    let path = save(&dir, &manifest);
    manifest["entries"][0]["file"] = "../poster.jpg".into();
    save(&dir, &manifest);
    assert!(inspect_manifest(&t.data_dir(), &path).is_err());
    manifest["entries"][0]["file"] = "poster.jpg".into();
    manifest["entries"][0]["ext"] = "mp4".into();
    save(&dir, &manifest);
    assert!(inspect_manifest(&t.data_dir(), &path).is_err());
    let bad = b"\xff\xd8\xffcorrupt";
    assert_eq!(MediaKind::sniff(bad), Some(MediaKind::Jpeg));
    std::fs::write(dir.path().join("poster.jpg"), bad).unwrap();
    for e in manifest["entries"].as_array_mut().unwrap() {
        e["ext"] = "jpg".into();
        e["bytes"] = bad.len().into();
        e["sha256"] = Digest::of(bad).to_string().into();
    }
    save(&dir, &manifest);
    assert!(inspect_manifest(&t.data_dir(), &path).is_err());
}

#[tokio::test]
async fn missing_cover_with_existing_slide_is_never_overwritten() {
    let (t, user, dir, manifest) = fixture().await;
    let path = save(&dir, &manifest);
    apply_manifest(&t.state, &path).await.unwrap();
    let db = t.state.user_db(&user).await.unwrap();
    db.write(|tx| {
        tx.execute("update posts set cover_object=null where key='ig_1'", [])?;
        Ok::<_, RepoError>(())
    })
    .unwrap();
    let dry = inspect_manifest(&t.data_dir(), &path).unwrap();
    assert_eq!((dry.eligible, dry.skipped_existing), (0, 2));
    let applied = apply_manifest(&t.state, &path).await.unwrap();
    assert_eq!(
        (
            applied.stored,
            applied.skipped_existing,
            applied.added_bytes
        ),
        (0, 2, 0)
    );
    assert!(
        db.read(|c| c
            .query_row(
                "select cover_object is null from posts where key='ig_1'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .map_err(RepoError::from))
            .unwrap()
    );
}

#[tokio::test]
async fn cli_defaults_to_aggregate_read_only_and_requires_offline_apply() {
    let (t, _user, dir, manifest) = fixture().await;
    let path = save(&dir, &manifest);
    let argv = [
        "shelfy-server",
        "admin",
        "backfill-covers",
        "--manifest",
        path.to_str().unwrap(),
    ];
    let cli = Cli::try_parse_from(argv).unwrap();
    let Command::Admin(admin) = cli.command else {
        panic!("expected admin")
    };
    let AdminCommand::BackfillCovers(args) = admin.command else {
        panic!("expected backfill")
    };
    assert!(!args.apply);
    let mut output = Vec::new();
    backfill_covers::run(&t.data_dir(), &args, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("mode=dry-run "));
    assert!(text.contains("\"eligible\":2"));
    assert!(!text.contains("synthetic"));
    assert!(!text.contains(dir.path().to_str().unwrap()));
    assert!(Cli::try_parse_from(argv.into_iter().chain(["--apply"])).is_err());
    assert!(Cli::try_parse_from(argv.into_iter().chain(["--server-stopped"])).is_err());
    assert!(Cli::try_parse_from(argv.into_iter().chain(["--apply", "--server-stopped"])).is_ok());
}
