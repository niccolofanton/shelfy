//! `admin synth`: fills one user's empty library with synthetic posts (P1-05;
//! plan §6.3, Appendix C), for benchmarks (`admin bench`), the gallery's
//! performance runs (P1-08) and the web end-to-end suite (P1-21).
//!
//! ```text
//! shelfy-server admin synth --email owner@example.test --posts 20000 --profile reference
//! ```
//!
//! **The `reference` profile** reproduces the aggregate shape of the reference
//! desktop library (Appendix C) as the web stores it after migration:
//!
//! - platforms: Instagram 65 %, X 35 %, one website in about 6,000 posts;
//! - media types: video 75 %, carousel 13.5 % (2–12 slides), image 6.4 %,
//!   X multi-image 1.9 %, text 2.8 %;
//! - two posts in three have their media stored (the reference library had
//!   a local cover for 67 % of its posts): a video post its poster (WebP),
//!   an image post its image (JPEG), a multi-image post every slide; covers
//!   and slides 1–3 have their `g480` rendition and the cover its ThumbHash
//!   (plan §2.13); 6 % of the stored video posts keep their video;
//! - captions in Italian and English, a few words to a few hundred
//!   characters, about one or two of 150 topics over a vocabulary of some
//!   40,000 words, so word frequencies look real (see `Vocab`), with
//!   hashtags (some compound), mentions and emojis; dates over ten years,
//!   more of them recent; about one post in twenty shares its time with the
//!   one before;
//! - one Instagram folder holding 57 % of the Instagram posts, manual tags on
//!   about 0.5 % of the posts, notes on 0.3 %, an AI layer on 0.1 %.
//!
//! **Files.** Renditions are real WebP files of about 25 KB (p50), rendered
//! by the media pipeline from synthetic pictures, so `GET /media/…g480.webp`
//! serves what it will serve in production. Masters are sparse
//! placeholders: a type header and an id, then a hole up to a realistic size
//! (a JPEG of 120–480 KB, a poster of 60–150 KB, a video of 3–26 MB). Their
//! names are the SHA-256 of that content, as in any store, but they take
//! almost no disk and no tool can decode them. Synthetic posts carry no
//! remote URL (no cover URL, no CDN slide URL), so a browser test on a
//! synthetic library makes no third-party request.
//!
//! **When.** It fills an empty library only, and writes from this process:
//! run it while the server is stopped, or restart the server afterwards,
//! because a running server's ETags and caches do not see writes from
//! another process. The same seed and post count give the same library.
//! It prints aggregate counts only.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use rusqlite::Connection;
use sha2::{Digest as _, Sha256};
use shelfy_core::db::{DbError, UserDb, UserDbConfig, is_library_locked};
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::media::{self, NewMediaObject};
use shelfy_core::repo::posts::{self, AiLayer, NewMedia, NewPost};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_media::render::{self, Rendered};
use shelfy_media::store::{MediaStore, UserMedia};
use shelfy_media::{Digest, MediaKind, Rendition};

use super::open_existing_control;
use crate::config::{DataDir, create_private_dir};
use crate::control::users::{self, Status};
use crate::ids::now_ms;

/// Most posts one run creates.
pub const MAX_POSTS: u32 = 200_000;
/// Seed when none is given.
pub const DEFAULT_SEED: u64 = 0x5EED_2026;

/// Arguments of `admin synth`.
#[derive(Debug, Args)]
pub struct SynthArgs {
    /// The user whose library to fill, by id.
    #[arg(
        long,
        value_name = "USER_ID",
        required_unless_present = "email",
        conflicts_with = "email"
    )]
    pub user: Option<String>,

    /// The user whose library to fill, by email.
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    /// How many posts to create.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_POSTS)))]
    pub posts: u32,

    /// The shape of the library.
    #[arg(long, value_enum, default_value_t = Profile::Reference)]
    pub profile: Profile,

    /// Seed of the generator: the same seed and count give the same library.
    #[arg(long, value_name = "SEED", default_value_t = DEFAULT_SEED)]
    pub seed: u64,

    /// Share of posts with deterministic topic-based AI data (0 keeps reference profile).
    #[arg(long, default_value_t = 0.0, value_parser = parse_ai_share)]
    pub ai_share: f64,
}

/// The shape of a synthetic library.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    /// The reference desktop library (plan Appendix C) as the web stores it.
    Reference,
}

impl Profile {
    /// The name on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
        }
    }
}

/// What to synthesize.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SynthOptions {
    /// Posts to create.
    pub posts: u32,
    /// Their shape.
    pub profile: Profile,
    /// Seed of the generator.
    pub seed: u64,
    /// 0 retains the reference AI mix; otherwise the analyzed topic-data share.
    pub ai_share: f64,
}

/// Aggregate counts of a synthesized library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SynthReport {
    /// Posts created.
    pub posts: usize,
    /// Posts per platform.
    pub platforms: BTreeMap<&'static str, usize>,
    /// Posts per media type.
    pub media_types: BTreeMap<&'static str, usize>,
    /// Slides.
    pub slides: usize,
    /// Stored objects (masters).
    pub objects: usize,
    /// Their sizes as recorded (placeholders: almost nothing on disk).
    pub object_bytes: u64,
    /// `g480` renditions written.
    pub renditions: usize,
    /// Their sizes on disk.
    pub rendition_bytes: u64,
    /// Members of the Instagram folder.
    pub folder_posts: usize,
    /// Posts with an AI layer, manual tags or a note.
    pub with_ai: usize,
    /// Posts with manual tags.
    pub with_tags: usize,
    /// Posts with a note.
    pub with_note: usize,
    /// Wall time, in milliseconds.
    pub millis: u128,
}

/// Runs `admin synth`.
///
/// # Errors
///
/// The user does not exist or is not active, the library is locked or not
/// empty, or writing failed.
pub fn run(data: &DataDir, args: &SynthArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let user_id = resolve_user(data, args.user.as_deref(), args.email.as_deref())?;
    let options = SynthOptions {
        posts: args.posts,
        profile: args.profile,
        seed: args.seed,
        ai_share: args.ai_share,
    };
    let report = synth(data, &user_id, &options)?;
    print_report(out, &user_id, &options, &report)?;
    Ok(())
}

/// The id of the active user named by `user` (an id) or `email`.
///
/// # Errors
///
/// No such user, or the user is not active.
pub fn resolve_user(
    data: &DataDir,
    user: Option<&str>,
    email: Option<&str>,
) -> anyhow::Result<String> {
    let control = open_existing_control(data)?;
    let found = control
        .read(|conn| match (user, email) {
            (Some(id), _) => users::get(conn, id),
            (None, Some(email)) => users::find_by_email(conn, &users::normalize_email(email)?),
            (None, None) => Ok(None),
        })
        .map_err(|e: RepoError| anyhow::anyhow!("cannot read the users: {e}"))?;
    let Some(found) = found else {
        bail!("no such user: name an existing account with --user or --email");
    };
    if found.status != Status::Active {
        bail!("user {} is not active", found.id);
    }
    Ok(found.id)
}

/// Fills `user_id`'s empty library with `options.posts` synthetic posts and
/// their media.
///
/// # Errors
///
/// The library is locked or not empty, or writing failed.
pub fn synth(data: &DataDir, user_id: &str, options: &SynthOptions) -> anyhow::Result<SynthReport> {
    anyhow::ensure!(
        options.ai_share.is_finite() && (0.0..=1.0).contains(&options.ai_share),
        "ai share must be between 0 and 1"
    );
    let started = Instant::now();
    let users_dir = data.users_dir();
    if is_library_locked(&users_dir, user_id)? {
        bail!("the library of user {user_id} is locked for maintenance");
    }
    let path = data.library_db(user_id);
    if let Some(dir) = path.parent() {
        create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let db = UserDb::open(&path, &UserDbConfig::default())
        .with_context(|| format!("cannot open {}", path.display()))?;
    let existing: i64 = db
        .read(|conn| {
            Ok::<_, DbError>(conn.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))?)
        })
        .context("cannot read the library")?;
    if existing > 0 {
        bail!(
            "the library of user {user_id} has {existing} posts: synth fills an empty library only"
        );
    }
    let store = MediaStore::new(&users_dir)
        .user(user_id)
        .map_err(|_| anyhow::anyhow!("invalid user id"))?;

    let pool = rendition_pool(options.seed)?;
    let plan = plan(options);
    let files = write_files(&store, &plan.objects, &pool, salt(options.seed, user_id))?;
    let now = now_ms();
    let mut report = db
        .write(|tx| record(tx, &plan, &files, &pool, now))
        .context("cannot write the library")?;
    db.checkpoint().context("cannot checkpoint the library")?;
    report.objects = plan.objects.len();
    report.object_bytes = plan.objects.iter().map(|o| o.bytes).sum();
    report.renditions = plan.objects.iter().filter(|o| o.g480.is_some()).count();
    report.rendition_bytes = plan
        .objects
        .iter()
        .filter_map(|o| o.g480.map(|g| pool[g].webp.len() as u64))
        .sum();
    report.millis = started.elapsed().as_millis();
    Ok(report)
}

fn print_report(
    out: &mut dyn Write,
    user_id: &str,
    options: &SynthOptions,
    r: &SynthReport,
) -> io::Result<()> {
    let list = |counts: &BTreeMap<&str, usize>| {
        counts
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    writeln!(
        out,
        "synthesized {} posts for user {user_id} in {:.1} s (profile {}, seed {})",
        r.posts,
        r.millis as f64 / 1000.0,
        options.profile.name(),
        options.seed
    )?;
    writeln!(out, "  platforms: {}", list(&r.platforms))?;
    writeln!(out, "  media types: {}", list(&r.media_types))?;
    writeln!(
        out,
        "  slides {}; stored objects {} ({:.2} GB recorded, sparse placeholders); \
         g480 renditions {} ({:.1} MB on disk)",
        r.slides,
        r.objects,
        r.object_bytes as f64 / 1e9,
        r.renditions,
        r.rendition_bytes as f64 / 1e6
    )?;
    writeln!(
        out,
        "  folder members {}; AI layers {}; manual tags {}; notes {}",
        r.folder_posts, r.with_ai, r.with_tags, r.with_note
    )
}

// ── The plan ────────────────────────────────────────────────────────────────

/// A deterministic xorshift64* generator.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`.
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    fn index(&mut self, n: usize) -> usize {
        usize::try_from(self.below(n as u64)).unwrap_or(0)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// `true` with probability `per_mille / 1000`.
    fn per_mille(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.index(items.len())]
    }

    /// An item, the first ones much more often (a Zipf-like skew).
    fn skewed<'a>(&mut self, items: &[&'a str]) -> &'a str {
        let at = (self.unit().powf(2.2) * items.len() as f64) as usize;
        items[at.min(items.len() - 1)]
    }

    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }

    fn digits(&mut self, n: usize) -> String {
        let mut s = String::with_capacity(n);
        s.push(char::from(b'1' + u8::try_from(self.below(9)).unwrap_or(0)));
        while s.len() < n {
            s.push(char::from(b'0' + u8::try_from(self.below(10)).unwrap_or(0)));
        }
        s
    }
}

/// An object to store: what its placeholder master is, and which pool
/// rendition (if any) is its `g480`.
#[derive(Clone, Debug)]
struct ObjectPlan {
    /// Unique per library: part of the placeholder's content.
    ordinal: u64,
    kind: MediaKind,
    role: &'static str,
    bytes: u64,
    width: u32,
    height: u32,
    duration_ms: Option<i64>,
    g480: Option<usize>,
}

/// A slide and the indexes of its objects in [`Plan::objects`].
struct SlidePlan {
    media: NewMedia,
    object: Option<usize>,
    video_object: Option<usize>,
}

struct PostPlan {
    post: NewPost,
    cover: Option<usize>,
    slides: Vec<SlidePlan>,
    in_folder: bool,
}

struct Plan {
    posts: Vec<PostPlan>,
    objects: Vec<ObjectPlan>,
    seed: u64,
}

/// 2026-10-02T00:00:00Z, the newest post time.
const NEWEST: i64 = 1_790_899_200_000;
const DAY: i64 = 86_400_000;

/// Shapes of pictures, as `(width, height)` of the source; the pool renders
/// each at its `g480` size.
const ASPECTS: [(u32, u32); 4] = [(1080, 1350), (1080, 1080), (1080, 1920), (1280, 720)];
/// Pool renditions per aspect.
const POOL_PER_ASPECT: usize = 32;

fn plan(options: &SynthOptions) -> Plan {
    match options.profile {
        Profile::Reference => {}
    }
    let mut rng = Rng::new(options.seed);
    let vocab = Vocab::new(&mut rng);
    let n = options.posts as usize;
    let mut ai_rng = Rng::new(options.seed ^ 0xA170_51A4_E202_6004);
    // One website in about 6,000 posts, at least one, spread out.
    let websites = n.div_ceil(6_138);
    let stride = (n / websites).max(1);
    let mut posts = Vec::with_capacity(n);
    let mut objects = Vec::new();
    let mut last_time = NEWEST;
    for i in 0..n {
        let mut post = if i % stride == stride / 2 && i / stride < websites {
            website(&mut rng, i)
        } else {
            social(&mut rng, &vocab, i, &mut objects, &mut last_time)
        };
        if options.ai_share > 0.0 {
            post.post.ai =
                (ai_rng.unit() < options.ai_share).then(|| topic_ai_layer(&mut ai_rng, &vocab));
        }
        posts.push(post);
    }
    Plan {
        posts,
        objects,
        seed: options.seed,
    }
}

fn social(
    rng: &mut Rng,
    vocab: &Vocab,
    i: usize,
    objects: &mut Vec<ObjectPlan>,
    last_time: &mut i64,
) -> PostPlan {
    let instagram = rng.per_mille(651);
    let roll = rng.below(1000);
    let media_type = if instagram {
        match roll {
            0..=722 => "video",
            723..=929 => "carousel",
            _ => "image",
        }
    } else {
        match roll {
            0..=813 => "video",
            814..=865 => "image",
            866..=918 => "images",
            _ => "text",
        }
    };
    let (key, native, platform) = if instagram {
        let pk = rng.digits(19);
        (format!("ig_{pk}"), pk, Platform::Instagram)
    } else {
        let id = rng.digits(19);
        (format!("x_{id}"), id, Platform::Twitter)
    };
    // Dates: ten years, more of them recent; about one post in twenty
    // shares the previous post's time (keyset ties).
    let posted_at = if i > 0 && rng.per_mille(50) {
        *last_time
    } else {
        let days = (rng.unit().powf(1.6) * 3_650.0) as i64;
        NEWEST - days * DAY - i64::try_from(rng.below(86_400_000)).unwrap_or(0)
    };
    *last_time = posted_at;
    let imported_at = NEWEST - i64::try_from(rng.below(400)).unwrap_or(0) * DAY;
    let mut post = NewPost::new(key, platform, native.clone(), media_type, imported_at);
    post.posted_at = (!rng.per_mille(30)).then_some(posted_at);
    let author = author(rng);
    if instagram {
        let shortcode = shortcode(rng);
        post.post_url = Some(format!("https://www.instagram.com/p/{shortcode}/"));
        post.profile_url = Some(format!("https://www.instagram.com/{author}/"));
        post.shortcode = Some(shortcode);
    } else {
        post.post_url = Some(format!("https://x.com/{author}/status/{native}"));
        post.profile_url = Some(format!("https://x.com/{author}"));
    }
    post.author_name = Some(display_name(&author));
    post.author_username = Some(author);
    post.caption = Some(caption(rng, vocab, instagram, media_type == "text"));

    // Media: stored for two posts in three. A text post has nothing to
    // store; an Instagram post whose media is missing has an expired URL.
    let stored = media_type != "text" && rng.per_mille(668);
    post.archive_state = Some(
        if media_type == "text" || stored {
            "done"
        } else if instagram {
            "failed"
        } else {
            "pending"
        }
        .to_owned(),
    );
    let slides = slides(rng, media_type, instagram, stored, objects);
    post.media = slides.iter().map(|s| s.media.clone()).collect();
    let cover = slides.first().and_then(|s| s.object);

    // The user layer and the AI layer, rare as in the reference library.
    if rng.per_mille(5) {
        post.user_tags = vec![rng.skewed(WORDS).to_owned()];
    }
    if rng.per_mille(3) {
        post.user_note = Some(format!("{} {}", rng.pick(NOTES), rng.skewed(WORDS)));
    }
    if rng.per_mille(1) {
        post.ai = Some(ai_layer(rng));
    }
    let in_folder = instagram && rng.per_mille(570);
    PostPlan {
        post,
        cover,
        slides,
        in_folder,
    }
}

fn website(rng: &mut Rng, i: usize) -> PostPlan {
    let hex: String = (0..40)
        .map(|_| char::from(b"0123456789abcdef"[rng.index(16)]))
        .collect();
    let domain = format!("{}-{i}.example.test", rng.pick(WORDS));
    let mut post = NewPost::new(
        format!("web_{}", &hex[..20]),
        Platform::Web,
        hex,
        "website",
        NEWEST - i64::try_from(rng.below(400)).unwrap_or(0) * DAY,
    );
    post.caption = Some(format!(
        "{} — {} {} studio",
        capitalize(rng.pick(WORDS)),
        rng.pick(WORDS),
        rng.pick(WORDS)
    ));
    post.web_url = Some(format!("https://{domain}/"));
    post.web_final_url = post.web_url.clone();
    post.author_username = Some(domain.clone());
    post.web_domain = Some(domain);
    post.archive_state = Some("link_only".to_owned());
    PostPlan {
        post,
        cover: None,
        slides: Vec::new(),
        in_folder: false,
    }
}

/// The slides of a post, and the objects of a stored one.
fn slides(
    rng: &mut Rng,
    media_type: &str,
    instagram: bool,
    stored: bool,
    objects: &mut Vec<ObjectPlan>,
) -> Vec<SlidePlan> {
    let count = match media_type {
        "carousel" => 2 + rng.below(11),
        "images" => 2 + rng.below(3),
        "text" => 0,
        _ => 1,
    };
    let mut out = Vec::new();
    for position in 0..count {
        let video = match media_type {
            "video" => true,
            "carousel" => rng.per_mille(160),
            _ => false,
        };
        let (width, height) = if video {
            if instagram {
                if rng.per_mille(700) {
                    (1080, 1920)
                } else {
                    (1080, 1350)
                }
            } else if rng.per_mille(700) {
                (1280, 720)
            } else {
                (720, 1280)
            }
        } else if instagram {
            *[(1080, 1350), (1080, 1350), (1080, 1080), (1080, 566)]
                .get(rng.index(4))
                .unwrap_or(&(1080, 1350))
        } else {
            *[(1200, 675), (1200, 1500), (1200, 1200)]
                .get(rng.index(3))
                .unwrap_or(&(1200, 675))
        };
        let duration_ms = video.then(|| i64::try_from(rng.range(5_000, 90_000)).unwrap_or(5_000));
        let mut slide = SlidePlan {
            media: NewMedia {
                kind: if video { "video" } else { "image" }.to_owned(),
                width: Some(i64::from(width)),
                height: Some(i64::from(height)),
                duration_ms,
                ..NewMedia::default()
            },
            object: None,
            video_object: None,
        };
        if stored {
            // Covers and slides 1–3 have their rendition (plan §2.13).
            let g480 = (position <= 3).then(|| pool_index(rng, width, height));
            let (kind, role, bytes) = if video {
                (MediaKind::Webp, "poster", rng.range(60_000, 150_000))
            } else {
                (MediaKind::Jpeg, "image", rng.range(120_000, 480_000))
            };
            slide.object = Some(push_object(
                objects,
                ObjectPlan {
                    ordinal: 0,
                    kind,
                    role,
                    bytes,
                    width,
                    height,
                    duration_ms: None,
                    g480,
                },
            ));
            if video && media_type == "video" && rng.per_mille(61) {
                slide.video_object = Some(push_object(
                    objects,
                    ObjectPlan {
                        ordinal: 0,
                        kind: MediaKind::Mp4,
                        role: "video",
                        bytes: rng.range(3_000_000, 26_000_000),
                        width,
                        height,
                        duration_ms,
                        g480: None,
                    },
                ));
            }
        }
        out.push(slide);
    }
    out
}

fn push_object(objects: &mut Vec<ObjectPlan>, mut object: ObjectPlan) -> usize {
    object.ordinal = objects.len() as u64;
    objects.push(object);
    objects.len() - 1
}

/// A pool rendition of the aspect nearest to `width` × `height`.
fn pool_index(rng: &mut Rng, width: u32, height: u32) -> usize {
    let ratio = f64::from(height) / f64::from(width);
    let aspect = ASPECTS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (f64::from(a.1) / f64::from(a.0) - ratio).abs();
            let db = (f64::from(b.1) / f64::from(b.0) - ratio).abs();
            da.total_cmp(&db)
        })
        .map_or(0, |(i, _)| i);
    aspect * POOL_PER_ASPECT + rng.index(POOL_PER_ASPECT)
}

fn author(rng: &mut Rng) -> String {
    let n = rng.below(2_400);
    let stem = HANDLES[usize::try_from(n).unwrap_or(0) % HANDLES.len()];
    format!("{stem}{}", n / HANDLES.len() as u64)
}

fn display_name(handle: &str) -> String {
    let words: Vec<String> = handle
        .split(['.', '_'])
        .filter(|w| !w.is_empty())
        .map(capitalize)
        .collect();
    words.join(" ")
}

fn shortcode(rng: &mut Rng) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    (0..11)
        .map(|_| char::from(ALPHABET[rng.index(ALPHABET.len())]))
        .collect()
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The words of the captions: a topic model over a large vocabulary, so
/// document frequencies look like a real library's (a few generic words in
/// 5–15 % of the posts, each topic's words in its own share, a long tail of
/// rare words) and the search index holds tens of thousands of terms.
///
/// - 40,000 pseudo-words made of Italian-like syllables;
/// - 150 topics of different popularity (Zipf), each with two real words
///   from [`WORDS`] at its head and 80 pseudo-words;
/// - a background of the real words, then 5,000 pseudo-words (Zipf).
struct Vocab {
    topics: Vec<Vec<String>>,
    topic_weights: Vec<f64>,
    topic_word_weights: Vec<f64>,
    background: Vec<String>,
    background_weights: Vec<f64>,
}

const PSEUDO_WORDS: usize = 40_000;
const TOPICS: usize = 150;
const TOPIC_WORDS: usize = 80;
const BACKGROUND_PSEUDO: usize = 5_000;

impl Vocab {
    fn new(rng: &mut Rng) -> Self {
        const SYLLABLES: &[&str] = &[
            "ma", "ri", "lo", "ta", "ne", "con", "ver", "sta", "pro", "de", "si", "gno", "la",
            "mi", "to", "ra", "ve", "li", "no", "fe", "sa", "ti", "co", "ren", "pa", "dor", "vi",
            "be", "ca", "fio", "lu", "mo", "ter", "gra", "pi", "so", "ze", "bar", "tel", "an",
            "ste", "cri", "vol", "por", "len", "mar", "tin", "fu", "go", "ul", "es", "art", "ex",
        ];
        let mut seen = std::collections::HashSet::new();
        let mut pseudo = Vec::with_capacity(PSEUDO_WORDS);
        while pseudo.len() < PSEUDO_WORDS {
            let parts = 2 + rng.index(3);
            let word: String = (0..parts).map(|_| rng.pick(SYLLABLES)).collect();
            if word.len() >= 4 && !WORDS.contains(&word.as_str()) && seen.insert(word.clone()) {
                pseudo.push(word);
            }
        }
        let topics = (0..TOPICS)
            .map(|t| {
                let mut words = vec![
                    WORDS[(t * 2) % WORDS.len()].to_owned(),
                    WORDS[(t * 2 + 1) % WORDS.len()].to_owned(),
                ];
                words.extend((0..TOPIC_WORDS).map(|_| pseudo[rng.index(PSEUDO_WORDS)].clone()));
                words
            })
            .collect();
        let background = WORDS
            .iter()
            .map(|w| (*w).to_owned())
            .chain((0..BACKGROUND_PSEUDO).map(|i| pseudo[(i * 7 + 3) % PSEUDO_WORDS].clone()))
            .collect::<Vec<_>>();
        Self {
            topics,
            topic_weights: zipf(TOPICS, 0.8),
            topic_word_weights: zipf(TOPIC_WORDS + 2, 1.0),
            background_weights: zipf(background.len(), 0.9),
            background,
        }
    }

    fn topic(&self, rng: &mut Rng) -> usize {
        sample(&self.topic_weights, rng)
    }

    fn topic_word(&self, rng: &mut Rng, topic: usize) -> &str {
        &self.topics[topic][sample(&self.topic_word_weights, rng)]
    }

    fn background_word(&self, rng: &mut Rng) -> &str {
        &self.background[sample(&self.background_weights, rng)]
    }
}

/// The cumulative weights of a Zipf distribution over `n` ranks.
fn zipf(n: usize, exponent: f64) -> Vec<f64> {
    let mut total = 0.0;
    (1..=n)
        .map(|rank| {
            total += (rank as f64).powf(-exponent);
            total
        })
        .collect()
}

/// A rank drawn from the cumulative weights `cumulative`.
fn sample(cumulative: &[f64], rng: &mut Rng) -> usize {
    let target = rng.unit() * cumulative.last().copied().unwrap_or(0.0);
    cumulative
        .partition_point(|&c| c < target)
        .min(cumulative.len().saturating_sub(1))
}

/// A caption: a few words to a few hundred characters (Instagram longer
/// than X), Italian and English, about one or two topics, with hashtags
/// (some compound), mentions and emojis.
fn caption(rng: &mut Rng, vocab: &Vocab, instagram: bool, text_post: bool) -> String {
    let words = if text_post {
        rng.range(12, 45)
    } else if instagram {
        rng.range(3, 60)
    } else {
        rng.range(2, 28)
    };
    let topic = vocab.topic(rng);
    let second = rng.per_mille(300).then(|| vocab.topic(rng));
    let mut out = String::new();
    let mut sentence_start = true;
    for w in 0..words {
        if w > 0 {
            out.push(' ');
        }
        let word = match rng.below(100) {
            0..=27 => rng.pick(STOPWORDS),
            28..=91 => {
                let t = match second {
                    Some(second) if rng.per_mille(350) => second,
                    _ => topic,
                };
                vocab.topic_word(rng, t)
            }
            _ => vocab.background_word(rng),
        };
        if sentence_start {
            out.push_str(&capitalize(word));
        } else {
            out.push_str(word);
        }
        sentence_start = false;
        if rng.per_mille(80) {
            out.push_str(rng.pick(&[".", ",", "!", "?", ":"]));
            sentence_start = out.ends_with(['.', '!', '?']);
        }
    }
    if rng.per_mille(250) {
        out.push(' ');
        out.push_str(rng.pick(EMOJIS));
    }
    let hashtags = if instagram {
        rng.below(10)
    } else {
        rng.below(3)
    };
    for _ in 0..hashtags {
        out.push_str(" #");
        out.push_str(vocab.topic_word(rng, topic));
        if rng.per_mille(400) {
            // A compound hashtag: two words run together.
            out.push_str(vocab.topic_word(rng, topic));
        }
    }
    if rng.per_mille(150) {
        out.push_str(&format!(" @{}", author(rng)));
    }
    out
}

fn parse_ai_share(s: &str) -> Result<f64, String> {
    let value: f64 = s
        .parse()
        .map_err(|_| "expected a fraction from 0 to 1".to_owned())?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("expected a finite fraction from 0 to 1".to_owned());
    }
    Ok(value)
}
fn topic_ai_layer(rng: &mut Rng, vocab: &Vocab) -> AiLayer {
    let topic = vocab.topic(rng);
    let general = vec![
        vocab.topics[topic][0].clone(),
        vocab.topics[topic][1].clone(),
    ];
    let specific = vec![
        vocab.topics[topic][2 + rng.index(TOPIC_WORDS)].clone(),
        vocab.topics[topic][2 + rng.index(TOPIC_WORDS)].clone(),
    ];
    let mut layer = ai_layer(rng);
    layer.tags = general.iter().chain(&specific).cloned().collect();
    layer.general_tags = Some(general);
    layer.specific_tags = Some(specific);
    layer.entities = vec![format!("Studio {topic}")];
    layer.keywords = vec![vocab.topic_word(rng, topic).to_owned()];
    layer.language = Some(rng.pick(&["it", "en", "fr", "es"]).to_owned());
    layer
}

fn ai_layer(rng: &mut Rng) -> AiLayer {
    let tags: Vec<String> = (0..rng.range(3, 6))
        .map(|_| rng.skewed(WORDS).to_owned())
        .collect();
    AiLayer {
        status: Some("done".to_owned()),
        model: Some("synthetic".to_owned()),
        description: Some(format!(
            "{} {} {}.",
            capitalize(rng.skewed(WORDS)),
            rng.pick(STOPWORDS),
            rng.skewed(WORDS)
        )),
        category: Some(rng.pick(CATEGORIES).to_owned()),
        content_type: Some(rng.pick(CONTENT_TYPES).to_owned()),
        keywords: vec![rng.skewed(WORDS).to_owned()],
        entities: vec![format!("Studio {}", rng.below(40))],
        tags,
        analyzed_at: Some(NEWEST - DAY),
        ..AiLayer::default()
    }
}

// ── Files ───────────────────────────────────────────────────────────────────

/// One pool rendition: a real `g480` WebP and the ThumbHash of its picture.
struct PoolEntry {
    webp: Vec<u8>,
    thumbhash: Vec<u8>,
}

/// Renders [`POOL_PER_ASPECT`] pictures of each of [`ASPECTS`] through the
/// media pipeline: real `g480` files of about 25 KB.
fn rendition_pool(seed: u64) -> anyhow::Result<Vec<PoolEntry>> {
    let jobs: Vec<(usize, usize)> = (0..ASPECTS.len())
        .flat_map(|a| (0..POOL_PER_ASPECT).map(move |n| (a, n)))
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let chunk = jobs.len().div_ceil(threads);
    let rendered: Vec<anyhow::Result<Vec<PoolEntry>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|&(aspect, n)| render_one(seed, aspect, n))
                        .collect::<anyhow::Result<Vec<_>>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("a render panicked")))
            })
            .collect()
    });
    let mut pool = Vec::with_capacity(jobs.len());
    for part in rendered {
        pool.extend(part?);
    }
    Ok(pool)
}

fn render_one(seed: u64, aspect: usize, n: usize) -> anyhow::Result<PoolEntry> {
    // Pictures at their g480 size, so the encoder sees the texture as is:
    // a gradient, discs and noise tuned for about 25 KB at q75.
    let (sw, sh) = ASPECTS[aspect];
    let (width, height) = render::fit(sw, sh, Rendition::G480.spec().max_side);
    let pixels = u64::from(width) * u64::from(height);
    let base_noise: u64 = if pixels >= 200_000 {
        15
    } else if pixels >= 170_000 {
        17
    } else {
        22
    };
    let mut rng = Rng::new(seed ^ ((aspect as u64) << 32) ^ n as u64);
    let noise = base_noise - 3 + rng.below(7);
    let rgb = picture(&mut rng, width, height, noise);
    let Rendered {
        webp, thumbhash, ..
    } = render::render_bytes(&png(width, height, &rgb), Rendition::G480.spec())
        .context("cannot render a synthetic picture")?;
    Ok(PoolEntry { webp, thumbhash })
}

/// An RGB picture: a diagonal gradient, a dozen discs and noise of
/// amplitude `noise`.
fn picture(rng: &mut Rng, width: u32, height: u32, noise: u64) -> Vec<u8> {
    let channel = |rng: &mut Rng| i64::try_from(rng.below(200)).unwrap_or(0);
    let base = [channel(rng), channel(rng), channel(rng)];
    let discs: Vec<(i64, i64, i64, [i64; 3])> = (0..10)
        .map(|_| {
            let x = i64::try_from(rng.below(u64::from(width))).unwrap_or(0);
            let y = i64::try_from(rng.below(u64::from(height))).unwrap_or(0);
            let r = 10 + i64::try_from(rng.below(u64::from(width / 3))).unwrap_or(0);
            (x, y, r, [channel(rng), channel(rng), channel(rng)])
        })
        .collect();
    let (w, h) = (i64::from(width), i64::from(height));
    let mut rgb = Vec::with_capacity(usize::try_from(w * h * 3).unwrap_or(0));
    let spread = i64::try_from(2 * noise + 1).unwrap_or(1);
    let amplitude = i64::try_from(noise).unwrap_or(0);
    for y in 0..h {
        for x in 0..w {
            let mut pixel = [
                base[0] + x * 55 / w,
                base[1] + y * 55 / h,
                base[2] + (x + y) * 30 / (w + h),
            ];
            for &(cx, cy, r, color) in &discs {
                if (x - cx).pow(2) + (y - cy).pow(2) < r * r {
                    pixel = color;
                }
            }
            for value in pixel {
                let jitter =
                    i64::try_from(rng.next_u64() % spread.unsigned_abs()).unwrap_or(0) - amplitude;
                rgb.push(u8::try_from((value + jitter).clamp(0, 255)).unwrap_or(0));
            }
        }
    }
    rgb
}

/// A PNG of `rgb`, uncompressed (stored deflate blocks): input for the
/// renderer, never written to disk.
fn png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row = width as usize * 3;
    let mut raw = Vec::with_capacity(height as usize * (row + 1));
    for line in rgb.chunks(row) {
        raw.push(0); // filter: none
        raw.extend_from_slice(line);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    png_chunk(&mut out, b"IHDR", &header);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in &png[start..] {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    png.extend_from_slice(&(!crc).to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// The first bytes of a placeholder master: its type's signature, then the
/// seed and the object's ordinal, so every placeholder has its own digest.
fn placeholder_head(object: &ObjectPlan, seed: u64) -> Vec<u8> {
    let mut head = match object.kind {
        MediaKind::Jpeg => {
            b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00".to_vec()
        }
        MediaKind::Webp => {
            let size = u32::try_from(object.bytes.saturating_sub(8)).unwrap_or(u32::MAX);
            let mut riff = b"RIFF".to_vec();
            riff.extend_from_slice(&size.to_le_bytes());
            riff.extend_from_slice(b"WEBPVP8 ");
            riff
        }
        _ => b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec(),
    };
    head.extend_from_slice(b"shelfy-synth");
    head.extend_from_slice(&seed.to_le_bytes());
    head.extend_from_slice(&object.ordinal.to_le_bytes());
    head
}

/// The SHA-256 of `head` followed by zeros up to `size` bytes.
fn placeholder_digest(head: &[u8], size: u64) -> Digest {
    static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];
    let mut hash = Sha256::new();
    hash.update(head);
    let mut left = size.saturating_sub(head.len() as u64);
    while left > 0 {
        let take = usize::try_from(left.min(ZEROS.len() as u64)).unwrap_or(ZEROS.len());
        hash.update(&ZEROS[..take]);
        left -= take as u64;
    }
    Digest::from_bytes(hash.finalize().into())
}

/// Writes every object's placeholder master (sparse) and `g480` rendition,
/// on all cores; returns their digests in the order of `objects`. `salt`
/// goes into every placeholder, so two libraries never share one.
fn write_files(
    store: &UserMedia,
    objects: &[ObjectPlan],
    pool: &[PoolEntry],
    salt: u64,
) -> anyhow::Result<Vec<Digest>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    create_private_dir(store.root())
        .with_context(|| format!("cannot create {}", store.root().display()))?;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let chunk = objects.len().div_ceil(threads);
    let parts: Vec<anyhow::Result<Vec<Digest>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = objects
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|object| write_object(store, object, pool, salt))
                        .collect::<anyhow::Result<Vec<_>>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("a writer panicked")))
            })
            .collect()
    });
    let mut digests = Vec::with_capacity(objects.len());
    for part in parts {
        digests.extend(part?);
    }
    Ok(digests)
}

fn write_object(
    store: &UserMedia,
    object: &ObjectPlan,
    pool: &[PoolEntry],
    salt: u64,
) -> anyhow::Result<Digest> {
    let head = placeholder_head(object, salt);
    let digest = placeholder_digest(&head, object.bytes);
    let path = store.object_path(&digest, object.kind);
    write_sparse(&path, &head, object.bytes)
        .with_context(|| format!("cannot write {}", path.display()))?;
    if let Some(entry) = object.g480 {
        let path = store.rendition_path(&digest, Rendition::G480);
        create_file(&path)
            .and_then(|mut file| file.write_all(&pool[entry].webp))
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(digest)
}

/// The placeholders' salt: the seed and the user id (FNV-1a), so two
/// libraries never share a placeholder.
fn salt(seed: u64, user_id: &str) -> u64 {
    user_id.bytes().fold(seed ^ 0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `head`, then a hole up to `size` bytes: a sparse file on file systems
/// that have them.
fn write_sparse(path: &Path, head: &[u8], size: u64) -> io::Result<()> {
    let mut file = create_file(path)?;
    file.write_all(head)?;
    file.set_len(size)
}

/// Creates (or truncates) the file at `path` and its directories, with the
/// store's modes: 0750 directories, 0640 files.
fn create_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o640);
    }
    options.open(path)
}

// ── The database ────────────────────────────────────────────────────────────

fn record(
    tx: &Connection,
    plan: &Plan,
    digests: &[Digest],
    pool: &[PoolEntry],
    now: i64,
) -> Result<SynthReport, RepoError> {
    let mut ids = Vec::with_capacity(plan.objects.len());
    for (object, digest) in plan.objects.iter().zip(digests) {
        ids.push(media::upsert_object(
            tx,
            &NewMediaObject {
                sha256: *digest.as_bytes(),
                ext: object.kind.ext().to_owned(),
                mime: object.kind.mime().to_owned(),
                bytes: i64::try_from(object.bytes).unwrap_or(i64::MAX),
                width: Some(i64::from(object.width)),
                height: Some(i64::from(object.height)),
                duration_ms: object.duration_ms,
                role: object.role.to_owned(),
                variants: if object.g480.is_some() {
                    Rendition::G480.bit()
                } else {
                    0
                },
                origin: "server".to_owned(),
            },
            now,
        )?);
    }
    let folder = collections::create(
        tx,
        &NewCollection {
            name: "Saved".to_owned(),
            platform: Some(Platform::Instagram),
            external_id: Some(format!("1790{:013}", plan.seed % 10_000_000_000_000)),
            source_name: Some("saved".to_owned()),
            ..NewCollection::default()
        },
        now,
    )?;
    let mut report = SynthReport::default();
    let mut members = Vec::new();
    for planned in &plan.posts {
        let mut post = planned.post.clone();
        for (slide, media) in planned.slides.iter().zip(post.media.iter_mut()) {
            media.object_id = slide.object.map(|o| ids[o]);
            media.video_object_id = slide.video_object.map(|o| ids[o]);
        }
        if let Some(cover) = planned.cover {
            post.cover_object = Some(ids[cover]);
            post.thumbhash = plan.objects[cover]
                .g480
                .map(|entry| pool[entry].thumbhash.clone());
        }
        let id = posts::insert(tx, &post, now)?;
        if planned.in_folder {
            members.push(id);
        }
        report.posts += 1;
        *report.platforms.entry(post.platform.as_str()).or_default() += 1;
        *report
            .media_types
            .entry(media_type_name(&post.media_type))
            .or_default() += 1;
        report.slides += post.media.len();
        report.with_ai += usize::from(post.ai.is_some());
        report.with_tags += usize::from(!post.user_tags.is_empty());
        report.with_note += usize::from(post.user_note.is_some());
    }
    collections::add_posts(tx, &members, &[folder.id], now)?;
    report.folder_posts = members.len();
    Ok(report)
}

fn media_type_name(media_type: &str) -> &'static str {
    [
        "video", "carousel", "image", "images", "text", "website", "file",
    ]
    .into_iter()
    .find(|t| *t == media_type)
    .unwrap_or("other")
}

// ── Vocabulary ──────────────────────────────────────────────────────────────

/// Content words, Italian and English, most frequent first.
#[rustfmt::skip]
const WORDS: &[&str] = &[
    "design", "video", "nuovo", "new", "art", "studio", "foto", "photo", "casa", "home", "style",
    "progetto", "project", "idea", "colori", "color", "light", "luce", "food", "ricetta",
    "recipe", "travel", "viaggio", "motion", "animation", "3d", "render", "typography",
    "tipografia", "lampada", "lamp", "interior", "arredo", "architecture", "architettura",
    "minimal", "vintage", "moda", "fashion", "outfit", "brand", "logo", "branding", "poster",
    "illustration", "illustrazione", "drawing", "disegno", "painting", "pittura", "ceramica",
    "ceramics", "wood", "legno", "marble", "marmo", "glass", "vetro", "concrete", "cemento",
    "texture", "pattern", "palette", "kitchen", "cucina", "chair", "sedia", "table", "tavolo",
    "sofa", "divano", "garden", "giardino", "plants", "piante", "flowers", "fiori", "nature",
    "natura", "mountain", "montagna", "sea", "mare", "beach", "spiaggia", "city", "città",
    "street", "strada", "night", "notte", "sunset", "tramonto", "coffee", "caffè", "pasta",
    "pizza", "dessert", "dolce", "bread", "pane", "wine", "vino", "cocktail", "breakfast",
    "colazione", "dinner", "cena", "music", "musica", "vinyl", "concert", "concerto", "festival",
    "camera", "film", "analog", "analogico", "portrait", "ritratto", "landscape", "paesaggio",
    "streetphotography", "photography", "fotografia", "workspace", "desk", "scrivania", "setup",
    "keyboard", "tastiera", "headphones", "cuffie", "speaker", "gadget", "tech", "tecnologia",
    "product", "prodotto", "industrial", "packaging", "sneakers", "scarpe", "watch", "orologio",
    "bag", "borsa", "jewelry", "gioielli", "accessories", "accessori", "car", "auto", "bike",
    "bici", "motorcycle", "moto", "running", "yoga", "fitness", "workout", "allenamento",
    "tutorial", "tips", "consigli", "guide", "guida", "process", "processo", "sketch",
    "schizzo", "inspiration", "ispirazione", "mood", "moodboard", "aesthetic", "estetica",
    "shader", "glsl", "webgl", "creativecoding", "generative", "generativa", "simulation",
    "simulazione", "fluid", "fluidi", "particles", "particelle", "houdini", "blender",
    "cinema4d", "touchdesigner", "projection", "installation", "installazione", "realtime",
    "interactive", "interattivo", "ui", "ux", "webdesign", "app", "website", "sito",
    "landing", "dashboard", "icon", "icone", "font", "lettering", "calligraphy", "calligrafia",
    "kinetic", "type", "grid", "griglia", "layout", "editorial", "editoriale", "magazine",
    "rivista", "book", "libro", "print", "stampa", "risograph", "screenprint", "serigrafia",
    "collage", "paper", "carta", "origami", "textile", "tessuto", "knitting", "maglia",
    "embroidery", "ricamo", "furniture", "mobili", "lighting", "illuminazione", "loft",
    "apartment", "appartamento", "renovation", "ristrutturazione", "bathroom", "bagno",
    "bedroom", "camera", "terrace", "terrazza", "pool", "piscina", "hotel", "restaurant",
    "ristorante", "bar", "shop", "negozio", "market", "mercato", "museum", "museo",
    "exhibition", "mostra", "gallery", "galleria", "sculpture", "scultura", "installation",
    "neon", "sign", "insegna", "mural", "murale", "graffiti", "tattoo", "tatuaggio",
    "skate", "surf", "snow", "neve", "winter", "inverno", "summer", "estate", "spring",
    "primavera", "autumn", "autunno", "milano", "roma", "napoli", "torino", "firenze",
    "venezia", "paris", "london", "tokyo", "berlin", "lisbon", "copenhagen", "nyc",
];

const STOPWORDS: &[&str] = &[
    "di", "la", "il", "per", "con", "un", "una", "che", "è", "the", "and", "of", "a", "in", "to",
    "with", "for", "this", "my", "our", "nel", "della", "del", "sul",
];

const EMOJIS: &[&str] = &["✨", "🔥", "😍", "🙌", "📸", "🎨", "☕️", "🌿", "💡", "➡️"];

const NOTES: &[&str] = &[
    "idea per", "rivedere", "for the", "check", "cliente:", "ref",
];

const HANDLES: &[&str] = &[
    "studio.", "design_", "atelier.", "the.", "daily_", "visual.", "archi_", "food.", "lab_",
    "maker.", "photo_", "type.", "motion_", "casa.", "tech_", "art.",
];

const CATEGORIES: &[&str] = &[
    "design",
    "interior",
    "food",
    "travel",
    "technology",
    "fashion",
];

const CONTENT_TYPES: &[&str] = &["inspiration", "tutorial", "product", "reference", "recipe"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_hash_like_their_content() {
        let object = ObjectPlan {
            ordinal: 7,
            kind: MediaKind::Webp,
            role: "poster",
            bytes: 200_000,
            width: 1080,
            height: 1920,
            duration_ms: None,
            g480: None,
        };
        let head = placeholder_head(&object, 3);
        let mut content = head.clone();
        content.resize(200_000, 0);
        assert_eq!(placeholder_digest(&head, 200_000), Digest::of(&content));
        assert_eq!(MediaKind::sniff(&content), Some(MediaKind::Webp));
        for kind in [MediaKind::Jpeg, MediaKind::Mp4] {
            let head = placeholder_head(
                &ObjectPlan {
                    kind,
                    ..object.clone()
                },
                3,
            );
            let mut content = head.clone();
            content.resize(4_096, 0);
            assert_eq!(MediaKind::sniff(&content), Some(kind), "{kind:?}");
        }
    }

    #[test]
    fn topic_ai_share_is_deterministic_and_preserves_base_posts() {
        let options = SynthOptions {
            posts: 1000,
            profile: Profile::Reference,
            seed: 7,
            ai_share: 0.0,
        };
        let reference = plan(&options);
        let topic = plan(&SynthOptions {
            ai_share: 0.7,
            ..options
        });
        let again = plan(&SynthOptions {
            ai_share: 0.7,
            ..options
        });
        let analyzed = topic.posts.iter().filter(|p| p.post.ai.is_some()).count();
        assert!((650..750).contains(&analyzed));
        for ((r, t), a) in reference.posts.iter().zip(&topic.posts).zip(&again.posts) {
            assert_eq!(t.post.ai, a.post.ai);
            assert_eq!(r.post.key, t.post.key);
            assert_eq!(r.post.caption, t.post.caption);
            assert_eq!(r.post.media, t.post.media);
            if let Some(ai) = &t.post.ai {
                assert_eq!(ai.status.as_deref(), Some("done"));
                assert!(!ai.general_tags.as_ref().unwrap().is_empty());
                assert!(!ai.specific_tags.as_ref().unwrap().is_empty());
                assert!(!ai.entities.is_empty());
                assert!(!ai.keywords.is_empty());
                assert!(
                    ai.category.is_some() && ai.content_type.is_some() && ai.language.is_some()
                );
            }
        }
        for bad in ["-1", "1.1", "NaN", "inf"] {
            assert!(parse_ai_share(bad).is_err());
        }
    }

    #[test]
    fn the_plan_follows_the_reference_mix() {
        let plan = plan(&SynthOptions {
            posts: 6_138,
            profile: Profile::Reference,
            seed: 1,
            ai_share: 0.0,
        });
        let count = |f: &dyn Fn(&PostPlan) -> bool| plan.posts.iter().filter(|p| f(p)).count();
        let share = |n: usize| n as f64 / plan.posts.len() as f64;
        let instagram = count(&|p| p.post.platform == Platform::Instagram);
        let videos = count(&|p| p.post.media_type == "video");
        let carousels = count(&|p| p.post.media_type == "carousel");
        let websites = count(&|p| p.post.platform == Platform::Web);
        let stored = count(&|p| p.cover.is_some());
        assert!((0.62..0.68).contains(&share(instagram)), "{instagram}");
        assert!((0.72..0.79).contains(&share(videos)), "{videos}");
        assert!((0.11..0.16).contains(&share(carousels)), "{carousels}");
        assert!((0.62..0.70).contains(&share(stored)), "{stored}");
        assert_eq!(websites, 1);
        let keys: std::collections::HashSet<&str> =
            plan.posts.iter().map(|p| p.post.key.as_str()).collect();
        assert_eq!(keys.len(), plan.posts.len(), "keys are distinct");
        // No remote URL a browser would fetch.
        assert!(
            plan.posts.iter().all(|p| p.post.cover_url.is_none()
                && p.post.media.iter().all(|m| m.source_url.is_none()))
        );
    }
}
