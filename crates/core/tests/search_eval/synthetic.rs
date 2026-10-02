//! The synthetic evaluation library: a desktop library (`shelfy.sqlite`, the
//! desktop's current schema) written from a fixed seed, so the search-eval
//! gate runs in CI without the owner's library (P1-05).
//!
//! It is shaped for the six cases of `cases.ts`: posts about each case's topic
//! (in captions, in compound hashtags, in AI fields), posts that share a
//! query word without being relevant, and a background of unrelated posts,
//! in Italian and English, with the reference library's platform mix
//! (Appendix C). The desktop's numbers on it are committed in
//! `synthetic-report.json`; `pnpm run eval:search` produced them on the file
//! [`write`] creates (`docs/web-port/spikes/05-fts-relevance.md`, "The gate
//! in CI").
//!
//! Every value comes from integer arithmetic on the seed, so the file holds
//! the same rows on every platform; [`digest`] fingerprints them and the
//! report records it. All data is synthetic.

use std::fmt::Write as _;
use std::path::Path;

use rusqlite::{Connection, params};
use shelfy_core::legacy::fixture::DESKTOP_SCHEMA_CURRENT;

/// Posts in the library.
pub const POSTS: usize = 3_000;
/// Seed of the generator.
const SEED: u64 = 0x005E_A2C4_E7A1;
/// 2026-10-02T00:00:00Z in seconds: the newest post.
const NOW_SECONDS: i64 = 1_790_899_200;
const DAY_SECONDS: i64 = 86_400;

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

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % n as u64).expect("small")
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    /// `n` distinct items (all of them when there are fewer), in random
    /// order: a partial Fisher-Yates shuffle.
    fn pick_distinct<'a>(&mut self, items: &[&'a str], n: usize) -> Vec<&'a str> {
        let mut pool = items.to_vec();
        let n = n.min(pool.len());
        for i in 0..n {
            let j = i + self.below(pool.len() - i);
            pool.swap(i, j);
        }
        pool.truncate(n);
        pool
    }

    fn digits(&mut self, n: usize) -> String {
        let mut s = String::with_capacity(n);
        s.push(char::from(
            b'1' + u8::try_from(self.below(9)).expect("digit"),
        ));
        while s.len() < n {
            s.push(char::from(
                b'0' + u8::try_from(self.below(10)).expect("digit"),
            ));
        }
        s
    }
}

/// A topic: what its posts say, in phrases and hashtags, and what an AI
/// analysis of them says.
struct Topic {
    /// Phrases of a caption (Italian and English).
    phrases: &'static [&'static str],
    /// Hashtags, many of them compound (`#productdesign`).
    hashtags: &'static [&'static str],
    /// AI tags (display forms).
    tags: &'static [&'static str],
    /// An AI description.
    descriptions: &'static [&'static str],
    /// AI keywords.
    keywords: &'static [&'static str],
}

/// The topics of the cases, then the near misses, then the background.
#[rustfmt::skip]
const TOPICS: &[Topic] = &[
    // 0: headphones (case `cuffie`).
    Topic {
        phrases: &[
            "nuove cuffie wireless con cancellazione del rumore",
            "AirPods Max in alluminio, custodia rivista",
            "unboxing delle AirPods Pro e degli accessori in pelle",
            "over-ear headphones with a walnut headband",
            "earbuds case in silicone colorato",
            "studio headphone stand fatto in legno",
            "auricolari in ceramica, edizione limitata",
            "concept di cuffia modulare con padiglioni magnetici",
        ],
        hashtags: &[
            "#headphones", "#airpodsmax", "#cuffiewireless", "#headphonedesign", "#earbuds",
            "#audiophile", "#airpodscase",
        ],
        tags: &["cuffie", "audio", "AirPods", "accessori audio", "product design"],
        descriptions: &[
            "Cuffie over-ear con archetto in legno e padiglioni in pelle.",
            "Custodia per AirPods con finitura opaca.",
        ],
        keywords: &["cuffie", "audio", "custodia"],
    },
    // 1: product design (case `product`).
    Topic {
        phrases: &[
            "product design per un gadget tecnologico da scrivania",
            "concept di wearable device per il fitness",
            "industrial design: una lampada da comodino in alluminio",
            "prodotto finito dopo sei mesi di prototipi",
            "accessori indossabili per lo smartphone",
            "new device mockup, matte black, soft corners",
            "smartwatch strap concept in recycled plastic",
            "gadget tecnologici che vorrei sulla mia scrivania",
            "packaging del prodotto in cartone riciclato",
        ],
        hashtags: &[
            "#productdesign", "#industrialdesign", "#gadget", "#wearabletech", "#designprodotto",
            "#techaccessories", "#devicedesign", "#productphotography",
        ],
        tags: &["product design", "industrial design", "gadget", "wearable", "tecnologia"],
        descriptions: &[
            "Render di un dispositivo indossabile con cinturino in tessuto.",
            "Prodotto di design industriale con superfici morbide.",
            "Gadget tecnologico da scrivania in alluminio anodizzato.",
        ],
        keywords: &["product design", "wearable", "gadget", "device"],
    },
    // 2: typography (case `tipografia`).
    Topic {
        phrases: &[
            "kinetic typography for a music video",
            "tipografia animata per la sigla di un festival",
            "variable font in motion, weight axis",
            "lettering a mano su carta ruvida",
            "type design: a new grotesk with ink traps",
            "poster tipografico con testo animato in loop",
            "testo cinetico su griglia modulare",
            "font specimen for a display typeface",
        ],
        hashtags: &[
            "#typography", "#kinetictypography", "#typedesign", "#lettering", "#fontdesign",
            "#motiontype", "#tipografia", "#typematters",
        ],
        tags: &["tipografia", "typography", "motion graphics", "font", "lettering"],
        descriptions: &[
            "Animazione tipografica con lettere che cambiano peso.",
            "Poster con lettering disegnato a mano.",
        ],
        keywords: &["typography", "kinetic type", "font"],
    },
    // 3: shaders (case `shader`).
    Topic {
        phrases: &[
            "fragment shader with raymarching and soft shadows",
            "GLSL sketch: signed distance fields and domain repetition",
            "raymarched clouds in a single shader",
            "SDF metaballs in WebGL",
            "shader art del giorno, rumore e domain warping",
            "un tunnel infinito in GLSL",
            "ray marching a mandelbulb in real time",
        ],
        hashtags: &[
            "#shader", "#glsl", "#raymarching", "#shaderart", "#creativecoding", "#webgl",
            "#generativeart", "#glslshader",
        ],
        tags: &["shader", "GLSL", "creative coding", "raymarching", "webgl"],
        descriptions: &[
            "Shader GLSL che disegna forme con signed distance fields.",
            "Scena in raymarching con luci morbide.",
        ],
        keywords: &["shader", "glsl", "sdf"],
    },
    // 4: fluids (case `fluidi`).
    Topic {
        phrases: &[
            "fluid simulation in Houdini, FLIP solver",
            "liquid splash sim, 40 million particles",
            "simulazione di un fluido viscoso che cola sul logo",
            "navier-stokes solver written from scratch",
            "smoke and fluid sim for a product shot",
            "fluida e lenta, miele digitale",
        ],
        hashtags: &[
            "#fluidsimulation", "#simulazionefluidi", "#houdinifx", "#fluidsolver", "#liquidsim",
            "#fluidart", "#sidefx",
        ],
        tags: &["simulazione", "fluidi", "Houdini", "VFX", "3D"],
        descriptions: &[
            "Simulazione di fluidi con particelle e schiuma.",
            "Liquido viscoso simulato su un oggetto in 3D.",
        ],
        keywords: &["fluid", "simulation", "houdini"],
    },
    // 5: TouchDesigner (case `p8-idf`).
    Topic {
        phrases: &[
            "progetti realizzati con TouchDesigner per un concerto",
            "installazione audio-reattiva in TouchDesigner",
            "realtime visuals made in touch designer",
            "feedback loop e particelle, patch di TouchDesigner",
            "projection mapping con TouchDesigner e un kinect",
            "tutorial TouchDesigner: instancing e noise",
        ],
        hashtags: &[
            "#touchdesigner", "#touchdesignercommunity", "#realtimevisuals", "#audioreactive",
            "#projectionmapping", "#tdcommunity",
        ],
        tags: &["TouchDesigner", "visual", "installazione", "realtime"],
        descriptions: &[
            "Visual in tempo reale generati in TouchDesigner.",
            "Installazione interattiva con proiezioni.",
        ],
        keywords: &["touchdesigner", "realtime", "installation"],
    },
    // 6: near miss: fashion accessories and jewellery (shares "accessori").
    Topic {
        phrases: &[
            "accessori moda per l'autunno, borse e cinture",
            "gioielli minimal in argento",
            "accessori per capelli fatti a mano",
            "fashion accessories flat lay",
            "borsa in pelle intrecciata",
        ],
        hashtags: &["#fashion", "#accessori", "#jewelry", "#ootd", "#accessories", "#borse"],
        tags: &["moda", "accessori", "gioielli"],
        descriptions: &["Accessori moda fotografati dall'alto."],
        keywords: &["moda", "accessori"],
    },
    // 7: near miss: animation that is not type (shares "animata").
    Topic {
        phrases: &[
            "illustrazione animata di un gatto che dorme",
            "animazione 2d frame by frame",
            "loop animato per una storia",
            "character animation, walk cycle",
            "una scena animata in stop motion",
        ],
        hashtags: &["#animation", "#2danimation", "#illustration", "#animazione", "#loop"],
        tags: &["animazione", "illustrazione", "2D"],
        descriptions: &["Animazione disegnata a mano di un personaggio."],
        keywords: &["animazione", "illustrazione"],
    },
    // 8: near miss: other simulations (shares "simulazioni").
    Topic {
        phrases: &[
            "simulazioni di guida in realtà virtuale",
            "simulazione di volo, cockpit fatto in casa",
            "crowd simulation for an architecture render",
            "simulazioni di tessuto per un abito digitale",
            "cloth sim test, silk and wind",
        ],
        hashtags: &["#simracing", "#vr", "#clothsim", "#simulation", "#marvelousdesigner"],
        tags: &["simulazione", "VR", "3D"],
        descriptions: &["Simulazione di tessuto su un avatar."],
        keywords: &["simulazione", "realtà virtuale"],
    },
    // 9: near miss: projects in architecture and interiors (shares "progetti",
    // "realizzati").
    Topic {
        phrases: &[
            "progetti realizzati nel 2025, ristrutturazione di un loft",
            "uno dei progetti di interior che abbiamo realizzato a Milano",
            "progetti di architettura in legno e pietra",
            "realizzati a mano, mobili in rovere",
            "casa al lago, progetto e direzione lavori",
        ],
        hashtags: &["#architecture", "#interiordesign", "#progetti", "#homedecor", "#archilovers"],
        tags: &["architettura", "interior design", "progetti"],
        descriptions: &["Interno di un loft ristrutturato con travi a vista."],
        keywords: &["interior", "architettura"],
    },
    // 10: near miss: graphic and web design (shares "design").
    Topic {
        phrases: &[
            "brand identity for a coffee roaster",
            "web design case study, landing page",
            "graphic design poster series",
            "logo design process, sketches to vector",
            "ui design for a banking app",
        ],
        hashtags: &["#graphicdesign", "#branding", "#webdesign", "#logodesign", "#uidesign"],
        tags: &["graphic design", "branding", "web design"],
        descriptions: &["Identità visiva con logo e palette."],
        keywords: &["branding", "logo"],
    },
    // 11: near miss: music and sound (close to headphones, not relevant).
    Topic {
        phrases: &[
            "vinili e giradischi nel nostro studio",
            "speaker bluetooth in legno",
            "studio di registrazione con pannelli fonoassorbenti",
            "synthesizer modulare, patch del giorno",
        ],
        hashtags: &["#vinyl", "#synth", "#music", "#homestudio", "#hifi"],
        tags: &["musica", "audio", "studio"],
        descriptions: &["Uno studio di registrazione casalingo."],
        keywords: &["musica", "studio"],
    },
];

/// Topics a background post is drawn from: food, travel, interiors and
/// other everyday saves.
#[rustfmt::skip]
const BACKGROUND: &[&str] = &[
    "ricetta", "pasta fresca", "tiramisù", "colazione", "pizza napoletana", "travel", "mountain",
    "tramonto sul lago", "weekend a Lisbona", "street photography", "film camera", "35mm",
    "ceramica fatta a mano", "vaso", "plants", "giardino", "sedia vintage", "chair", "lampada",
    "cucina in marmo", "kitchen tiles", "bookshelf", "workspace setup", "desk", "minimal",
    "palette di colori", "texture", "concrete", "neon", "sneakers", "outfit", "running",
    "yoga", "coffee", "latte art", "bicicletta", "mercatino", "museo", "mostra", "concerto",
    "festival", "libro del mese", "poster vintage", "illustration", "watercolor", "acquerello",
    "oil painting", "sculpture", "photography", "fotografia", "ritratto", "paesaggio",
];

#[rustfmt::skip]
const FILLER: &[&str] = &[
    "che ne pensate?", "salvato per dopo", "link in bio", "credits to the artist",
    "repost", "tutorial nel prossimo post", "swipe ➡️", "✨", "🔥", "wow", "finally done",
    "work in progress", "dettagli", "details", "behind the scenes", "process", "ispirazione",
    "inspo", "mood", "via @studio.example",
];

/// How many posts of each case topic, then each near miss (indexes of
/// [`TOPICS`]), out of [`POSTS`]; the rest is background.
const TOPIC_POSTS: [usize; 12] = [18, 150, 90, 80, 28, 45, 70, 60, 40, 60, 90, 45];

/// Writes the library to `path` (which must not exist) and returns its
/// [`digest`].
pub fn write(path: &Path) -> String {
    let conn = Connection::open(path).expect("create the synthetic library");
    conn.execute_batch(DESKTOP_SCHEMA_CURRENT)
        .expect("desktop schema");
    let tx = conn.unchecked_transaction().expect("transaction");
    let mut rng = Rng::new(SEED);
    let mut plan: Vec<Option<usize>> = Vec::with_capacity(POSTS);
    for (topic, &n) in TOPIC_POSTS.iter().enumerate() {
        plan.extend(std::iter::repeat_n(Some(topic), n));
    }
    plan.resize(POSTS, None);
    // Interleave topics through time: a Fisher-Yates shuffle.
    for i in (1..plan.len()).rev() {
        plan.swap(i, rng.below(i + 1));
    }
    for topic in plan {
        let post = post(&mut rng, topic);
        insert(&tx, &post);
    }
    tx.commit().expect("commit");
    digest(&conn)
}

/// A fingerprint of the rows search reads: two 64-bit FNV-1a hashes (one
/// over the posts, one over the tag rows) of their values, length-prefixed,
/// in key order. It detects drift; it is not a security measure.
pub fn digest(conn: &Connection) -> String {
    let posts = fnv(
        conn,
        "SELECT id, platform, coalesce(text, ''), coalesce(timestamp, ''),
                coalesce(ai_description, ''), coalesce(ai_tags, ''),
                coalesce(ai_keywords, ''), coalesce(user_note, ''),
                coalesce(user_tags, ''), coalesce(author_username, '')
         FROM posts ORDER BY id",
    );
    let tags = fnv(
        conn,
        "SELECT post_id, tag_norm, tag_form, coalesce(tier, '') FROM post_tags
         ORDER BY post_id, tag_norm",
    );
    let mut out = String::new();
    let _ = write!(out, "{posts:016x}{tags:016x}");
    out
}

/// FNV-1a over the text columns of every row of `sql`.
fn fnv(conn: &Connection, sql: &str) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    let mut stmt = conn.prepare(sql).expect("digest query");
    let columns = stmt.column_count();
    let mut rows = stmt.query([]).expect("digest rows");
    while let Some(row) = rows.next().expect("row") {
        for i in 0..columns {
            let value: String = row.get(i).expect("text");
            feed(&(value.len() as u64).to_le_bytes());
            feed(value.as_bytes());
        }
    }
    hash
}

/// One desktop row and its AI tag rows.
struct Post {
    id: String,
    platform: &'static str,
    shortcode: Option<String>,
    post_url: String,
    author: String,
    text: String,
    media_type: &'static str,
    timestamp: String,
    imported_at: i64,
    ai: Option<Ai>,
    user_note: Option<String>,
    user_tags: Option<String>,
}

struct Ai {
    description: String,
    tags: Vec<&'static str>,
    keywords: Vec<&'static str>,
}

fn post(rng: &mut Rng, topic: Option<usize>) -> Post {
    let (platform, media_type) = match rng.below(100) {
        0..=64 => (
            "instagram",
            *["video", "video", "video", "carousel", "image"]
                .get(rng.below(5))
                .expect("index"),
        ),
        65..=97 => (
            "twitter",
            *["video", "image", "images", "text"]
                .get(rng.below(4))
                .expect("index"),
        ),
        _ => ("pinterest", "image"),
    };
    let (id, shortcode, post_url) = match platform {
        "instagram" => {
            let pk = rng.digits(19);
            let code = format!("C{}", rng.digits(10));
            let url = format!("https://www.instagram.com/p/{code}/");
            (pk, Some(code), url)
        }
        "twitter" => {
            let id = rng.digits(19);
            let url = format!("https://x.com/i/status/{id}");
            (id, None, url)
        }
        _ => {
            let id = rng.digits(18);
            let url = format!("https://www.pinterest.com/pin/{id}/");
            (id, None, url)
        }
    };
    let author = format!(
        "{}.{}",
        rng.pick(&["studio", "atelier", "lab", "daily", "the"]),
        rng.below(400)
    );
    let text = caption(rng, topic, platform);
    let age_days = i64::try_from(rng.below(3_650)).expect("small");
    let seconds =
        NOW_SECONDS - age_days * DAY_SECONDS - i64::try_from(rng.below(86_400)).expect("small");
    let timestamp = iso(seconds);
    let imported_at = NOW_SECONDS - i64::try_from(rng.below(30)).expect("small") * DAY_SECONDS;
    // About one post in twenty has an AI analysis, as in the baseline
    // library; topic posts a little more often.
    let analyzed = rng.chance(if topic.is_some() { 8 } else { 3 });
    let ai = analyzed.then(|| ai_layer(rng, topic));
    let user_note = rng.chance(3).then(|| match topic {
        Some(t) => format!("da rivedere: {}", rng.pick(TOPICS[t].phrases)),
        None => format!("idea per {}", rng.pick(BACKGROUND)),
    });
    let user_tags = rng.chance(4).then(|| {
        let tag = match topic {
            Some(t) => rng.pick(TOPICS[t].tags).to_lowercase(),
            None => rng.pick(BACKGROUND).to_owned(),
        };
        serde_json::to_string(&[tag]).expect("tags serialize")
    });
    Post {
        id,
        platform,
        shortcode,
        post_url,
        author,
        text,
        media_type,
        timestamp,
        imported_at,
        ai,
        user_note,
        user_tags,
    }
}

/// A caption: for a topic post, one or two of its phrases, sometimes only
/// hashtags (the topic is then found inside a compound tag, or not at all
/// by a token search); for a background post, everyday words. Fillers,
/// mentions and emojis around them. A caption never repeats a phrase, a
/// hashtag or a word of its list, as people rarely do.
fn caption(rng: &mut Rng, topic: Option<usize>, platform: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    match topic {
        Some(t) => {
            let topic = &TOPICS[t];
            let hashtags_only = rng.chance(20);
            if !hashtags_only {
                let n = if rng.chance(35) { 2 } else { 1 };
                let phrases = rng.pick_distinct(topic.phrases, n);
                parts.push(capitalize(phrases[0]));
                parts.extend(phrases[1..].iter().map(|p| (*p).to_owned()));
            }
            if rng.chance(40) {
                parts.push(rng.pick(FILLER).to_owned());
            }
            if platform == "instagram" || hashtags_only || rng.chance(30) {
                let n = 1 + rng.below(4);
                parts.push(rng.pick_distinct(topic.hashtags, n).join(" "));
            }
            if rng.chance(25) {
                let other = rng.pick(BACKGROUND);
                parts.push(format!("#{}", other.replace(' ', "")));
            }
        }
        None => {
            let n = 2 + rng.below(4);
            parts.push(capitalize(&rng.pick_distinct(BACKGROUND, n).join(", ")));
            if rng.chance(50) {
                parts.push(rng.pick(FILLER).to_owned());
            }
            if platform == "instagram" && rng.chance(60) {
                let n = 1 + rng.below(3);
                let tags: Vec<String> = rng
                    .pick_distinct(BACKGROUND, n)
                    .into_iter()
                    .map(|word| format!("#{}", word.replace(' ', "")))
                    .collect();
                parts.push(tags.join(" "));
            }
        }
    }
    if rng.chance(15) {
        parts.push(format!(
            "@{}",
            rng.pick(&["studio.example", "maker.lab", "daily.inspo"])
        ));
    }
    parts.join(if rng.chance(50) { "\n" } else { " " })
}

fn ai_layer(rng: &mut Rng, topic: Option<usize>) -> Ai {
    match topic {
        Some(t) => {
            let topic = &TOPICS[t];
            let mut tags: Vec<&'static str> = Vec::new();
            for _ in 0..2 + rng.below(3) {
                let tag = rng.pick(topic.tags);
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            let mut keywords: Vec<&'static str> = Vec::new();
            for _ in 0..1 + rng.below(2) {
                let keyword = rng.pick(topic.keywords);
                if !keywords.contains(&keyword) {
                    keywords.push(keyword);
                }
            }
            Ai {
                description: rng.pick(topic.descriptions).to_owned(),
                tags,
                keywords,
            }
        }
        None => {
            let subject = rng.pick(BACKGROUND);
            let mut tags = vec![subject];
            let other = rng.pick(BACKGROUND);
            if other != subject {
                tags.push(other);
            }
            Ai {
                description: format!("Foto di {subject}, luce naturale."),
                tags,
                keywords: vec![subject],
            }
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn insert(conn: &Connection, post: &Post) {
    let ai_tags = post
        .ai
        .as_ref()
        .map(|ai| serde_json::to_string(&ai.tags).expect("tags serialize"));
    let ai_keywords = post
        .ai
        .as_ref()
        .map(|ai| serde_json::to_string(&ai.keywords).expect("keywords serialize"));
    conn.execute(
        "INSERT INTO posts (id, platform, shortcode, post_url, author_username, author_name, text,
                            media_type, timestamp, imported_at, ai_description, ai_tags,
                            ai_status, ai_model, ai_keywords, user_note, user_tags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            post.id,
            post.platform,
            post.shortcode,
            post.post_url,
            post.author,
            post.author.replace('.', " "),
            post.text,
            post.media_type,
            post.timestamp,
            post.imported_at,
            post.ai.as_ref().map(|ai| ai.description.as_str()),
            ai_tags,
            post.ai.as_ref().map(|_| "done"),
            post.ai.as_ref().map(|_| "synthetic"),
            ai_keywords,
            post.user_note,
            post.user_tags,
        ],
    )
    .expect("insert a post");
    if let Some(ai) = &post.ai {
        for (i, tag) in ai.tags.iter().enumerate() {
            conn.execute(
                "INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, tier)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    post.id,
                    tag.to_lowercase(),
                    tag,
                    if i == 0 { "general" } else { "specific" }
                ],
            )
            .expect("insert a tag");
        }
    }
}

/// `seconds` since the epoch as the desktop stores post times:
/// `YYYY-MM-DDTHH:MM:SS.000Z`.
fn iso(seconds: i64) -> String {
    let days = seconds.div_euclid(DAY_SECONDS);
    let rest = seconds.rem_euclid(DAY_SECONDS);
    // Howard Hinnant's days-from-civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[test]
fn dates_are_iso_utc() {
    assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(iso(NOW_SECONDS), "2026-10-02T00:00:00.000Z");
    assert_eq!(iso(951_782_400 + 3_661), "2000-02-29T01:01:01.000Z");
}
