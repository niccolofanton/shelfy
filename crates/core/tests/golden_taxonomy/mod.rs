//! Synthetic byte-for-byte golden checks against desktop taxonomy helpers.
use super::check;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shelfy_core::schema::{self, Kind};
use shelfy_core::tags::{Status, aliases, clusters, graph};
fn library() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    c
}
fn post(c: &Connection, id: i64, tags: &[String]) {
    c.execute("INSERT INTO posts (id,key,platform,native_id,media_type,imported_at,sort_ts,updated_at) VALUES (?1,?2,'instagram',?2,'image',1,1,1)",params![id,format!("ig_{id}")]).unwrap();
    for t in tags {
        c.execute(
            "INSERT INTO post_tags (post_id,tag_norm,tag_form,source) VALUES (?1,?2,?3,'ai')",
            params![id, t.to_lowercase(), t],
        )
        .unwrap();
    }
}
#[derive(Deserialize)]
struct GraphInput {
    freq: Vec<(String, u64)>,
    edges: Vec<graph::Edge>,
    vectors: Option<graph::Vectors>,
    options: graph::Options,
}
#[test]
fn communities_match_desktop_bytes() {
    check("ai/clusters/graph", |(g,): (GraphInput,)| {
        graph::build_tag_communities(
            &g.freq.into_iter().collect(),
            &g.edges,
            g.vectors.as_ref(),
            g.options,
        )
    });
}
#[test]
fn refinement_matches_desktop_bytes() {
    check("ai/clusters/parse", |(text,): (Value,)| {
        clusters::parse_refine_response(&text)
    });
    check(
        "ai/clusters/refine",
        |(tags, value): (Vec<String>, Value)| clusters::validate_refined_groups(&tags, &value),
    );
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClusterRow {
    label: String,
    status: String,
    run_id: i64,
    tags: Vec<String>,
}
#[derive(Serialize)]
struct RunOutput {
    saved: clusters::SavedRun,
    rows: Vec<ClusterRow>,
}
#[test]
fn cluster_run_persistence_matches_desktop_bytes() {
    check(
        "ai/clusters/save",
        |(groups,): (Vec<clusters::RefinedGroup>,)| {
            let mut conn = library();
            let tx = conn.transaction().unwrap();
            clusters::save_run(
                &tx,
                &[
                    clusters::RefinedGroup {
                        label: "Accepted".into(),
                        tags: vec!["a".into(), "b".into()],
                    },
                    clusters::RefinedGroup {
                        label: "Old".into(),
                        tags: vec!["x".into(), "y".into()],
                    },
                ],
                1234,
                1234,
            )
            .unwrap();
            clusters::review(&tx, 1, true, None, 1234).unwrap();
            let saved = clusters::save_run(&tx, &groups, 1234, 1234).unwrap();
            let rows=tx.prepare("SELECT id,label,status,run_id FROM tag_cluster ORDER BY label").unwrap().query_map([],|r| {
            let id:i64=r.get(0)?;
            let tags=tx.prepare("SELECT tag_norm FROM tag_cluster_membership WHERE cluster_id=?1 ORDER BY tag_norm")?.query_map([id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ClusterRow {label:r.get(1)?,status:r.get(2)?,run_id:r.get(3)?,tags})
        }).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            RunOutput { saved, rows }
        },
    );
}
#[test]
fn alias_allowlist_matches_desktop_bytes() {
    check(
        "ai/aliases/validate",
        |(batch, vocab, parsed): (
            Vec<shelfy_core::tags::VocabTag>,
            Vec<shelfy_core::tags::VocabTag>,
            Value,
        )| aliases::validate_pairs(&batch, &vocab, &parsed),
    );
}
#[derive(Serialize)]
struct VocabOutput {
    unaliased: Vec<shelfy_core::tags::VocabTag>,
    canonical: Vec<shelfy_core::tags::VocabTag>,
}
#[test]
fn alias_candidates_match_desktop_bytes() {
    check("ai/aliases/candidates", |(posts,): (Vec<Vec<String>>,)| {
        let c = library();
        for (n, tags) in posts.iter().enumerate() {
            post(&c, n as i64 + 1, tags);
        }
        c.execute("INSERT INTO tag_alias (alias_norm,canonical_norm,canonical_form,status,created_at) VALUES ('lamps','lamp','Lamp','proposed',1)",[]).unwrap();
        VocabOutput {
            unaliased: aliases::unaliased_tags(&c, 400).unwrap(),
            canonical: aliases::canonical_vocab(&c, 300).unwrap(),
        }
    });
}
#[derive(Serialize)]
struct SaveResult {
    added: usize,
    rewritten: usize,
}
#[derive(Serialize)]
struct AliasSaveOutput {
    result: SaveResult,
    aliases: Vec<aliases::Alias>,
    norms: Vec<String>,
}
#[test]
fn proposed_alias_persistence_matches_desktop_bytes() {
    check("ai/aliases/save", |(pairs,): (Vec<aliases::AliasPair>,)| {
        let mut c = library();
        post(&c, 1, &["Lamps".into()]);
        let tx = c.transaction().unwrap();
        let added = aliases::save_proposals(&tx, &pairs, 1).unwrap();
        let aliases = aliases::list(&tx, Some(Status::Proposed)).unwrap();
        let norms = tx
            .prepare("SELECT tag_norm FROM post_tags")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        AliasSaveOutput {
            result: SaveResult {
                added,
                rewritten: 0,
            },
            aliases,
            norms,
        }
    });
}

#[test]
fn candidate_groups_match_desktop_bytes() {
    check(
        "ai/clusters/candidates",
        |(posts, vectors): (Vec<Vec<String>>, Option<graph::Vectors>)| {
            let c = library();
            for (n, tags) in posts.iter().enumerate() {
                post(&c, n as i64 + 1, tags);
            }
            graph::candidate_groups(&c, vectors.as_ref(), graph::Options::default()).unwrap()
        },
    );
}
