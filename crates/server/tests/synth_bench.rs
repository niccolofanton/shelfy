//! `admin synth` and `admin bench` (P1-05) on a temporary data directory:
//! the synthetic library is complete and consistent (rows, files, digests,
//! search index), fills only an empty library, carries no remote URL, and the
//! bench times every route through the routes' own service functions and
//! prints aggregates only.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read as _;

use clap::Parser as _;
use rusqlite::{Connection, OpenFlags};
use shelfy_core::search::index;
use shelfy_media::{Digest, MediaKind};
use shelfy_server::admin::bench::{self, BenchArgs, Route};
use shelfy_server::admin::owner::create_owner;
use shelfy_server::admin::synth::{self, Profile, SynthOptions};
use shelfy_server::cli::{Cli, Command};
use shelfy_server::config::{Config, DataDir};
use shelfy_server::state::AppState;
use tempfile::TempDir;

const POSTS: u32 = 300;

fn data_dir() -> (TempDir, DataDir) {
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path()).unwrap();
    (dir, data)
}

fn options(posts: u32) -> SynthOptions {
    SynthOptions {
        posts,
        profile: Profile::Reference,
        seed: 7,
    }
}

fn library(data: &DataDir, user: &str) -> Connection {
    Connection::open_with_flags(data.library_db(user), OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn synth_fills_an_empty_library_with_posts_and_media() {
    let (_dir, data) = data_dir();
    let user = create_owner(&data, "owner@example.test")
        .unwrap()
        .user_id()
        .to_owned();
    let report = synth::synth(&data, &user, &options(POSTS)).unwrap();
    assert_eq!(report.posts, POSTS as usize);
    assert_eq!(report.platforms.values().sum::<usize>(), POSTS as usize);
    assert!(report.objects > 0 && report.renditions > 0, "{report:?}");
    assert_eq!(report.folder_posts as i64, {
        let conn = library(&data, &user);
        count(&conn, "SELECT count(*) FROM post_collections")
    });

    let conn = library(&data, &user);
    assert_eq!(count(&conn, "SELECT count(*) FROM posts"), i64::from(POSTS));
    // No remote URL a browser would fetch.
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM posts WHERE cover_url IS NOT NULL"
        ),
        0
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM post_media WHERE source_url IS NOT NULL"
        ),
        0
    );
    // Every stored cover has its ThumbHash.
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM posts WHERE cover_object IS NOT NULL AND thumbhash IS NULL"
        ),
        0
    );
    // The search indexes match the posts.
    assert_eq!(index::verify(&conn).unwrap(), Vec::<i64>::new());

    // Every object has its file at the recorded size, named by its content's
    // digest, and its rendition when the row says so.
    let media = shelfy_media::store::MediaStore::new(data.users_dir())
        .user(&user)
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT sha256, ext, bytes, variants FROM media_objects")
        .unwrap();
    let rows: Vec<(Vec<u8>, String, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows.len(), report.objects);
    let mut g480 = 0;
    for (sha, ext, bytes, variants) in rows {
        let digest = Digest::from_slice(&sha).unwrap();
        let kind = MediaKind::from_ext(&ext).unwrap();
        let path = media.object_path(&digest, kind);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), u64::try_from(bytes).unwrap());
        if kind != MediaKind::Mp4 {
            let mut content = Vec::new();
            File::open(&path)
                .unwrap()
                .read_to_end(&mut content)
                .unwrap();
            assert_eq!(Digest::of(&content), digest, "{path:?}");
            assert_eq!(MediaKind::sniff(&content), Some(kind));
        }
        let rendition = media.rendition_path(&digest, shelfy_media::Rendition::G480);
        if variants & 1 == 1 {
            let webp = std::fs::read(&rendition).unwrap();
            assert_eq!(MediaKind::sniff(&webp), Some(MediaKind::Webp));
            assert!((5_000..80_000).contains(&webp.len()), "{}", webp.len());
            g480 += 1;
        } else {
            assert!(!rendition.exists());
        }
    }
    assert_eq!(g480, report.renditions);

    // Only an empty library.
    let err = synth::synth(&data, &user, &options(5)).unwrap_err();
    assert!(err.to_string().contains("empty library"), "{err}");
}

#[test]
fn bench_times_every_route_and_prints_aggregates_only() {
    let (_dir, data) = data_dir();
    let user = create_owner(&data, "owner@example.test")
        .unwrap()
        .user_id()
        .to_owned();
    synth::synth(&data, &user, &options(POSTS)).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let report = runtime.block_on(async {
        let config = Config::with_data_dir(data.clone());
        let state = tokio::task::spawn_blocking(move || AppState::open(config))
            .await
            .unwrap()
            .unwrap();
        bench::bench(&state, &user, 25, 3).await.unwrap()
    });
    assert_eq!(report.posts, u64::from(POSTS));
    for route in Route::ALL {
        let stats = &report.routes[&route];
        assert_eq!(stats.errors, 0, "{route:?}");
        assert_eq!(stats.samples.len(), 25, "{route:?}");
    }

    // The command's output: the routes, numbers, and nothing from the library.
    let mut out = Vec::new();
    let args = BenchArgs {
        user: user.clone(),
        requests: 10,
        strict: false,
        seed: 3,
    };
    bench::run(&data, &args, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    for route in Route::ALL {
        assert!(text.contains(route.label()), "{text}");
    }
    let conn = library(&data, &user);
    let mut stmt = conn
        .prepare("SELECT key, coalesce(caption, '') FROM posts")
        .unwrap();
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let words: HashSet<String> = rows
        .iter()
        .flat_map(|(_, caption)| {
            caption
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.chars().count() >= 6)
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .collect();
    for (key, _) in &rows {
        assert!(!text.contains(key.as_str()), "a key in the output");
    }
    let lowered = text.to_lowercase();
    let leaked: Vec<&String> = words
        .iter()
        .filter(|w| lowered.contains(w.as_str()))
        .collect();
    // The report's own words ("requests", "release", "budget", …) aside.
    let allowed = [
        "requests",
        "renditions",
        "release",
        "budget",
        "result",
        "errors",
        "timed",
        "untimed",
        "skipped",
        "nothing",
        "request",
        "library",
    ];
    let leaked: Vec<&&String> = leaked
        .iter()
        .filter(|w| !allowed.contains(&w.as_str()))
        .collect();
    assert!(leaked.is_empty(), "library words in the output: {leaked:?}");
}

#[test]
fn the_commands_parse() {
    let parse = |args: &[&str]| {
        let argv = ["shelfy-server", "admin", "--data-dir", "/srv/shelfy"]
            .iter()
            .chain(args);
        Cli::try_parse_from(argv).map(|cli| cli.command)
    };
    assert!(matches!(
        parse(&["synth", "--email", "owner@example.test", "--posts", "20000"]),
        Ok(Command::Admin(_))
    ));
    assert!(
        parse(&[
            "synth",
            "--user",
            "01J9Z3B8K4QW6TFX0V7G2N5RCA",
            "--posts",
            "5"
        ])
        .is_ok()
    );
    assert!(parse(&["synth", "--posts", "5"]).is_err(), "names a user");
    assert!(
        parse(&[
            "synth",
            "--user",
            "u",
            "--email",
            "e@example.test",
            "--posts",
            "5"
        ])
        .is_err(),
        "one way to name the user"
    );
    assert!(parse(&["synth", "--email", "e@example.test", "--posts", "0"]).is_err());
    assert!(
        parse(&[
            "synth",
            "--email",
            "e@example.test",
            "--posts",
            "5",
            "--profile",
            "huge"
        ])
        .is_err()
    );
    assert!(parse(&["bench", "--user", "01J9Z3B8K4QW6TFX0V7G2N5RCA"]).is_ok());
    assert!(
        parse(&["bench", "--user", "u", "--requests", "5"]).is_err(),
        "at least 10"
    );
    assert!(parse(&["bench"]).is_err());
}
