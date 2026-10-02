//! `admin bench`: the server budgets of plan §6.2 on one user's library
//! (P1-05 on a synthetic library, P1-26 on the VPS).
//!
//! ```text
//! shelfy-server admin bench --user <USER_ID> [--requests 400] [--strict]
//! ```
//!
//! It runs the work of the read routes after authentication, in process: the
//! service functions behind `GET /api/v1/posts`, `GET /api/v1/search`, `GET
//! /api/v1/posts/{key}` and `GET /media/{sha}.g480.webp`
//! ([`crate::routes::posts::serve_list`], [`crate::routes::search::serve_search`],
//! [`crate::routes::posts::serve_post`], [`crate::routes::media::serve`]), one
//! request at a time after a warm-up. It goes through no HTTP stack and no
//! authentication, and adds no way around them: it is an operator command on
//! the data directory. What it times is a route's server time: the queries,
//! the JSON body and, for media, reading the file.
//!
//! **Workload.** Gallery pages (`GET /posts`): the first pages of the library
//! and of its common filters, and pages deep into it by cursor. Searches
//! (`GET /search` and `GET /posts?q=`): first pages of queries made from the
//! library's own words, common, mid and rare ones, one to three words, infix
//! fragments, stopword-only queries and tags with text; no query repeats, so
//! no cached ranking or count makes a search look cheaper than a new one.
//! Post details of random posts; `g480` renditions of random objects.
//!
//! **Privacy.** It reads the library and prints aggregates only: no key,
//! caption, query, tag or file name leaves the process.
//!
//! **Where.** Run it on a release build (`--release`, or the image); a debug
//! build says so in its output. It may run next to the server, whose own
//! latency it then shares the machine with.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use clap::Args;
use shelfy_core::repo::RepoError;
use shelfy_media::store::MediaStore;

use super::synth::resolve_user;
use crate::config::{Config, DataDir};
use crate::error::ApiError;
use crate::rate_limit::RateLimitConfig;
use crate::routes::listing::{MatchMode, PostSort, YesNo};
use crate::routes::media;
use crate::routes::model::{MediaType, Platform};
use crate::routes::posts::{self, PostsQuery};
use crate::routes::search::{self, SearchQuery};
use crate::state::{AppState, blocking};

/// Requests per route when not given.
pub const DEFAULT_REQUESTS: u32 = 400;
/// Untimed requests per route before the timed ones.
const WARM_UP: u32 = 20;

/// Arguments of `admin bench`.
#[derive(Debug, Args)]
pub struct BenchArgs {
    /// The user whose library to measure.
    #[arg(long, value_name = "USER_ID")]
    pub user: String,

    /// Timed requests per route, after a warm-up.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_REQUESTS,
          value_parser = clap::value_parser!(u32).range(10..=100_000))]
    pub requests: u32,

    /// Exit with an error when a route misses its budget or fails.
    #[arg(long)]
    pub strict: bool,

    /// Seed of the workload: the same seed draws the same requests.
    #[arg(long, value_name = "SEED", default_value_t = 0xBE_4C8)]
    pub seed: u64,
}

/// A route the bench measures, and its §6.2 budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    /// `GET /api/v1/posts`, a page of 60 without search text.
    List,
    /// `GET /api/v1/search`, a first page with its total.
    Search,
    /// `GET /api/v1/posts?q=…`, a first page ranked by relevance.
    ListSearch,
    /// `GET /api/v1/posts/{key}`.
    Detail,
    /// `GET /media/{sha}.g480.webp`.
    Rendition,
}

impl Route {
    /// Every route, in report order.
    pub const ALL: [Self; 5] = [
        Self::List,
        Self::Search,
        Self::ListSearch,
        Self::Detail,
        Self::Rendition,
    ];

    /// The route as the report names it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::List => "GET /posts",
            Self::Search => "GET /search",
            Self::ListSearch => "GET /posts?q=",
            Self::Detail => "GET /posts/{key}",
            Self::Rendition => "GET /media g480",
        }
    }

    /// The §6.2 budget: p95 and, for the list, p99.
    #[must_use]
    pub const fn budget(self) -> (Duration, Option<Duration>) {
        match self {
            Self::List => (Duration::from_millis(40), Some(Duration::from_millis(100))),
            Self::Search | Self::ListSearch => (Duration::from_millis(60), None),
            Self::Detail => (Duration::from_millis(15), None),
            Self::Rendition => (Duration::from_millis(5), None),
        }
    }
}

/// The timings of one route.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteStats {
    /// Timed requests that answered 200.
    pub samples: Vec<Duration>,
    /// Timed requests that failed.
    pub errors: usize,
}

impl RouteStats {
    /// The nearest-rank `q`-quantile (`q` in `0..=1`).
    #[must_use]
    pub fn quantile(&self, q: f64) -> Duration {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let rank = ((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len());
        sorted[rank - 1]
    }

    /// Whether the route met its budget, with no error.
    #[must_use]
    pub fn meets(&self, route: Route) -> bool {
        let (p95, p99) = route.budget();
        self.errors == 0
            && !self.samples.is_empty()
            && self.quantile(0.95) <= p95
            && p99.is_none_or(|p99| self.quantile(0.99) <= p99)
    }
}

/// The outcome of a bench run: aggregates only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BenchReport {
    /// Live posts in the library.
    pub posts: u64,
    /// Stored objects with a `g480` rendition.
    pub renditions: u64,
    /// Timings per route; a route with nothing to request is missing.
    pub routes: BTreeMap<Route, RouteStats>,
    /// Timings of the searches per kind of query (common word, two words,
    /// infix fragment, …).
    pub kinds: BTreeMap<(Route, &'static str), RouteStats>,
}

impl BenchReport {
    /// Whether every measured route met its budget.
    #[must_use]
    pub fn meets_budgets(&self) -> bool {
        self.routes.iter().all(|(route, stats)| stats.meets(*route))
    }
}

/// Runs `admin bench`.
///
/// # Errors
///
/// The user or the library does not exist, a request failed to run, or, with
/// `--strict`, a route missed its budget.
pub fn run(data: &DataDir, args: &BenchArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let user_id = resolve_user(data, Some(&args.user), None)?;
    if !data.library_db(&user_id).is_file() {
        bail!("user {user_id} has no library yet");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("cannot start the runtime")?;
    // The routes' rate limits are layers the service functions never pass
    // through; they are off here all the same, so a later change that moves
    // a limit into a handler does not throttle the bench.
    let config = Config {
        rate_limits: RateLimitConfig::disabled(),
        ..Config::with_data_dir(data.clone())
    };
    let report = runtime.block_on(async {
        let state = blocking(move || AppState::open(config).map_err(ApiError::internal))
            .await
            .map_err(|e| anyhow::anyhow!("cannot open the data directory: {e:?}"))?;
        bench(&state, &user_id, args.requests, args.seed).await
    })?;
    print_report(out, &user_id, args.requests, &report)?;
    if args.strict && !report.meets_budgets() {
        bail!("a route missed its §6.2 budget");
    }
    Ok(())
}

/// Measures every route on `user_id`'s library: `requests` timed requests
/// each, after a warm-up, drawn with `seed`.
///
/// # Errors
///
/// The library cannot be read.
pub async fn bench(
    state: &AppState,
    user_id: &str,
    requests: u32,
    seed: u64,
) -> anyhow::Result<BenchReport> {
    let db = state
        .user_db(user_id)
        .await
        .map_err(|e| anyhow::anyhow!("cannot open the library: {e:?}"))?;
    let sample = blocking(move || db.read(Sample::read))
        .await
        .map_err(|e| anyhow::anyhow!("cannot read the library: {e:?}"))?;
    let mut report = BenchReport {
        posts: sample.posts,
        renditions: sample.renditions.len() as u64,
        ..BenchReport::default()
    };
    let mut workload = Workload::new(sample, seed);
    let store = MediaStore::new(state.config().data_dir.users_dir());
    let headers = HeaderMap::new();
    for route in Route::ALL {
        if !workload.covers(route) {
            continue;
        }
        let mut stats = RouteStats::default();
        for n in 0..WARM_UP + requests {
            let (request, kind) = workload.next(route);
            let started = Instant::now();
            let outcome = request.send(state, user_id, &store, &headers).await;
            let elapsed = started.elapsed();
            // Untimed: where the walk through the library goes next.
            if route == Route::List
                && let Ok(body) = &outcome
            {
                workload.saw_list_page(body);
            }
            if n < WARM_UP {
                continue;
            }
            match outcome {
                Ok(_) => {
                    stats.samples.push(elapsed);
                    if let Some(kind) = kind {
                        report
                            .kinds
                            .entry((route, kind))
                            .or_default()
                            .samples
                            .push(elapsed);
                    }
                }
                Err(_) => stats.errors += 1,
            }
        }
        report.routes.insert(route, stats);
    }
    Ok(report)
}

fn print_report(
    out: &mut dyn Write,
    user_id: &str,
    requests: u32,
    report: &BenchReport,
) -> std::io::Result<()> {
    let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
    writeln!(
        out,
        "bench: user {user_id}: {} posts, {} g480 renditions; {requests} timed requests per \
         route after {WARM_UP} untimed; {} build",
        report.posts,
        report.renditions,
        if cfg!(debug_assertions) {
            "DEBUG (not comparable to the budgets)"
        } else {
            "release"
        }
    )?;
    writeln!(
        out,
        "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7}  {:<20} result",
        "route", "requests", "p50 ms", "p95 ms", "p99 ms", "max ms", "errors", "budget"
    )?;
    for (route, stats) in &report.routes {
        let (p95, p99) = route.budget();
        let budget = match p99 {
            Some(p99) => format!("p95 ≤ {}, p99 ≤ {}", p95.as_millis(), p99.as_millis()),
            None => format!("p95 ≤ {}", p95.as_millis()),
        };
        writeln!(
            out,
            "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7}  {:<20} {}",
            route.label(),
            stats.samples.len(),
            ms(stats.quantile(0.5)),
            ms(stats.quantile(0.95)),
            ms(stats.quantile(0.99)),
            ms(stats.quantile(1.0)),
            stats.errors,
            budget,
            if stats.meets(*route) { "ok" } else { "MISSED" }
        )?;
    }
    for route in Route::ALL {
        if !report.routes.contains_key(&route) {
            writeln!(out, "{:<18} skipped: nothing to request", route.label())?;
        }
    }
    let searches: Vec<_> = report
        .kinds
        .iter()
        .filter(|((route, _), _)| *route == Route::Search)
        .collect();
    if !searches.is_empty() {
        writeln!(out, "GET /search by kind of query (p50 / p95 / max ms):")?;
        for ((_, kind), stats) in searches {
            writeln!(
                out,
                "  {kind:<20} {:>4} requests  {:>7} / {:>7} / {:>7}",
                stats.samples.len(),
                ms(stats.quantile(0.5)),
                ms(stats.quantile(0.95)),
                ms(stats.quantile(1.0))
            )?;
        }
    }
    Ok(())
}

// ── The workload ────────────────────────────────────────────────────────────

/// What the requests are drawn from, read once from the library.
struct Sample {
    posts: u64,
    keys: Vec<String>,
    renditions: Vec<String>,
    collections: Vec<i64>,
    tags: Vec<String>,
    /// Words of the captions, most frequent first, with their counts.
    words: Vec<(String, u32)>,
}

impl Sample {
    fn read(conn: &rusqlite::Connection) -> Result<Self, RepoError> {
        let posts: i64 = conn.query_row(
            "SELECT count(*) FROM posts WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        let keys = conn
            .prepare("SELECT key FROM posts WHERE deleted_at IS NULL ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let renditions = conn
            .prepare("SELECT sha256 FROM media_objects WHERE variants & 1 = 1 ORDER BY id")?
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .map(|sha| {
                sha.map(|bytes| {
                    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    format!("{hex}.g480.webp")
                })
            })
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let collections = conn
            .prepare("SELECT id FROM collections ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        let tags = conn
            .prepare(
                "SELECT tag_norm FROM post_tags GROUP BY tag_norm ORDER BY count(*) DESC, tag_norm
                 LIMIT 50",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        // Words of up to 3,000 captions, spread over the library.
        let stride = (posts / 3_000).max(1);
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        let mut stmt = conn.prepare(
            "SELECT caption FROM posts WHERE deleted_at IS NULL AND caption IS NOT NULL
               AND id % ?1 = 0",
        )?;
        let mut rows = stmt.query([stride])?;
        while let Some(row) = rows.next()? {
            let caption: String = row.get(0)?;
            for word in caption
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.chars().count() >= 3)
            {
                *counts.entry(word.to_lowercase()).or_default() += 1;
            }
        }
        let mut words: Vec<(String, u32)> = counts.into_iter().collect();
        words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(Self {
            posts: u64::try_from(posts).unwrap_or(0),
            keys,
            renditions,
            collections,
            tags,
            words,
        })
    }
}

/// One request to time.
enum Request {
    List(Box<PostsQuery>),
    Search(Box<SearchQuery>),
    Detail(String),
    Rendition(String),
}

impl Request {
    /// Runs the route's work and reads the whole body; `Err` unless 200.
    async fn send(
        self,
        state: &AppState,
        user_id: &str,
        store: &MediaStore,
        headers: &HeaderMap,
    ) -> Result<Bytes, ApiError> {
        let response: Response = match self {
            Self::List(query) => posts::serve_list(state, user_id, headers, *query).await?,
            Self::Search(query) => search::serve_search(state, user_id, headers, *query).await?,
            Self::Detail(key) => posts::serve_post(state, user_id, headers, key).await?,
            Self::Rendition(file) => {
                media::serve(store, user_id, &file, &Method::GET, headers).await?
            }
        };
        if response.status() != StatusCode::OK {
            return Err(ApiError::from_status(response.status()));
        }
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .map_err(ApiError::internal)
    }
}

/// Draws the requests.
struct Workload {
    sample: Sample,
    rng: u64,
    /// Search texts already used: a search is never repeated.
    used: HashSet<String>,
    /// The last gallery page asked for, and the next page of the walk.
    last_list: Option<PostsQuery>,
    pages: Option<PostsQuery>,
    list_turn: usize,
    search_turn: usize,
}

/// Queries every library is asked: stopword-only (the slowest) and generic
/// words, besides the library's own.
const GENERIC: &[&str] = &[
    "the", "a", "di", "design", "video", "foto", "photo", "new", "art", "lamp", "food", "travel",
    "style", "home", "studio", "music",
];

/// Kinds of search, for the breakdown of the report.
const KIND_COMMON: &str = "a common word";
const KIND_MID: &str = "a mid word";
const KIND_RARE: &str = "a rare word";
const KIND_TWO: &str = "two words";
const KIND_SENTENCE: &str = "a sentence";
const KIND_GENERIC: &str = "a generic word";
const KIND_INFIX: &str = "an infix fragment";
const KIND_RARE_MID: &str = "rare + mid word";
const KIND_HYBRID: &str = "text, concept, tag";

impl Workload {
    fn new(sample: Sample, seed: u64) -> Self {
        Self {
            sample,
            rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            used: HashSet::new(),
            last_list: None,
            pages: None,
            list_turn: 0,
            search_turn: 0,
        }
    }

    /// Notes the next page after the gallery page `body` answered.
    fn saw_list_page(&mut self, body: &[u8]) {
        let Some(query) = self.last_list.take() else {
            return;
        };
        let next = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|page| page["nextCursor"].as_str().map(str::to_owned));
        self.pages = next.map(|cursor| PostsQuery {
            cursor: Some(cursor),
            include_total: None,
            ..query
        });
    }

    fn covers(&self, route: Route) -> bool {
        match route {
            Route::List | Route::Detail => !self.sample.keys.is_empty(),
            Route::Search | Route::ListSearch => !self.sample.words.is_empty(),
            Route::Rendition => !self.sample.renditions.is_empty(),
        }
    }

    fn below(&mut self, n: usize) -> usize {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        usize::try_from(x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n.max(1) as u64).unwrap_or(0)
    }

    /// The next request of `route`, and for a search its kind.
    fn next(&mut self, route: Route) -> (Request, Option<&'static str>) {
        match route {
            Route::List => (Request::List(Box::new(self.list_query())), None),
            Route::Search => {
                let (query, kind) = self.search_query();
                (Request::Search(Box::new(query)), Some(kind))
            }
            Route::ListSearch => {
                let (search, kind) = self.search_query();
                let query = PostsQuery {
                    q: search.q,
                    concept: search.concept,
                    tags: search.tags,
                    tag_mode: search.tag_mode,
                    ..PostsQuery::default()
                };
                (Request::List(Box::new(query)), Some(kind))
            }
            Route::Detail => {
                let at = self.below(self.sample.keys.len());
                (Request::Detail(self.sample.keys[at].clone()), None)
            }
            Route::Rendition => {
                let at = self.below(self.sample.renditions.len());
                (Request::Rendition(self.sample.renditions[at].clone()), None)
            }
        }
    }

    /// Gallery pages: two requests in three go one page deeper into the
    /// view of the one before, by cursor; the others open a first page, plain
    /// or filtered.
    fn list_query(&mut self) -> PostsQuery {
        self.list_turn += 1;
        if !self.list_turn.is_multiple_of(3)
            && let Some(query) = self.pages.take()
        {
            self.last_list = Some(query.clone());
            return query;
        }
        let collection = self.pick_collection();
        let tag = self.pick_tag();
        let base = PostsQuery::default();
        let query = match self.below(12) {
            0 => PostsQuery {
                include_total: Some(true),
                ..base
            },
            1 => PostsQuery {
                platform: Some(Platform::Instagram),
                ..base
            },
            2 => PostsQuery {
                platform: Some(Platform::Twitter),
                include_total: Some(true),
                ..base
            },
            3 => PostsQuery {
                media_type: vec![MediaType::Video],
                ..base
            },
            4 => PostsQuery {
                media_type: vec![MediaType::Carousel, MediaType::Images],
                ..base
            },
            5 => PostsQuery {
                stored: Some(YesNo::Yes),
                include_total: Some(true),
                ..base
            },
            6 => PostsQuery {
                stored: Some(YesNo::No),
                ..base
            },
            7 => PostsQuery {
                sort: Some(PostSort::Oldest),
                ..base
            },
            8 => PostsQuery {
                collection,
                include_total: Some(true),
                ..base
            },
            9 => PostsQuery { tag, ..base },
            10 => PostsQuery {
                ai_tagged: Some(YesNo::No),
                media_type: vec![MediaType::Image],
                ..base
            },
            _ => base,
        };
        self.last_list = Some(query.clone());
        query
    }

    /// Search texts drawn from the library's own words, never repeated, and
    /// their kind.
    fn search_query(&mut self) -> (SearchQuery, &'static str) {
        for _ in 0..64 {
            let (query, kind) = self.draw_search();
            let text = format!("{:?}|{:?}|{:?}", query.q, query.tags, query.concept);
            if self.used.insert(text) {
                return (query, kind);
            }
        }
        // The words ran out: allow a repeat.
        self.draw_search()
    }

    fn draw_search(&mut self) -> (SearchQuery, &'static str) {
        self.search_turn += 1;
        let words = self.sample.words.len();
        let band = |share: f64| ((words as f64 * share) as usize).max(1);
        let (common, mid) = (band(0.01), band(0.2));
        let common_word = self.word(0, common);
        let mid_word = self.word(common, mid);
        let rare_word = self.word(mid, words);
        let (q, kind) = match self.search_turn % 10 {
            0 => (common_word, KIND_COMMON),
            1 | 2 => (mid_word, KIND_MID),
            3 => (rare_word, KIND_RARE),
            4 => (format!("{mid_word} {common_word}"), KIND_TWO),
            5 => (
                format!("{} di {mid_word} e {rare_word}", self.word(0, mid)),
                KIND_SENTENCE,
            ),
            6 => (GENERIC[self.below(GENERIC.len())].to_owned(), KIND_GENERIC),
            7 => {
                // A fragment inside a longer word: the infix index's case.
                let long = self.word(0, mid);
                let chars: Vec<char> = long.chars().collect();
                let fragment = if chars.len() >= 7 {
                    let start = 1 + self.below(chars.len() - 5);
                    chars[start..start + 4].iter().collect()
                } else {
                    long
                };
                (fragment, KIND_INFIX)
            }
            8 => (format!("{rare_word} {mid_word}"), KIND_RARE_MID),
            _ => {
                let tag = self.pick_tag();
                let query = SearchQuery {
                    q: Some(mid_word),
                    tags: tag.into_iter().collect(),
                    tag_mode: Some(MatchMode::Or),
                    concept: vec![common_word],
                    ..SearchQuery::default()
                };
                return (query, KIND_HYBRID);
            }
        };
        let query = SearchQuery {
            q: Some(q),
            ..SearchQuery::default()
        };
        (query, kind)
    }

    fn pick_collection(&mut self) -> Option<i64> {
        let n = self.sample.collections.len();
        (n > 0).then(|| {
            let at = self.below(n);
            self.sample.collections[at]
        })
    }

    fn pick_tag(&mut self) -> Option<String> {
        let n = self.sample.tags.len();
        (n > 0).then(|| {
            let at = self.below(n);
            self.sample.tags[at].clone()
        })
    }

    /// A word whose frequency rank is in `from..to`.
    fn word(&mut self, from: usize, to: usize) -> String {
        let words = self.sample.words.len();
        let (from, to) = (from.min(words.saturating_sub(1)), to.clamp(1, words));
        let at = from + self.below(to.saturating_sub(from).max(1));
        self.sample.words[at.min(words - 1)].0.clone()
    }
}
