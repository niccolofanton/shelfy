//! Synthetic import contracts: streaming, identity, merge and collections.
mod support;
use serde_json::{Value, json};
use shelfy_core::import::{
    self, Report, collections, normalize,
    v1::{self, Record},
};
use shelfy_core::repo::posts::{self, UserContentPatch};
use std::collections::BTreeMap;
use std::io::Read;
use support::{NOW, library};
fn records(bytes: &[u8]) -> (Vec<(u64, Value)>, BTreeMap<String, collections::Definition>) {
    let mut posts = Vec::new();
    let mut defs = BTreeMap::new();
    v1::read(bytes, |r| -> Result<(), v1::Error> {
        match r {
            Record::Post { index, value, .. } => posts.push((index, value)),
            Record::Collection(v) => {
                let d = collections::definition(&v).unwrap();
                defs.insert(d.key.clone(), d);
            }
        }
        Ok(())
    })
    .unwrap();
    (posts, defs)
}
#[test]
fn desktop_reimport_is_unchanged_and_preserves_user_layers() {
    let c = library();
    let (raw, defs) = records(include_bytes!("fixtures/import/desktop.json"));
    let mut first = Report::default();
    let keys = import::apply(&c, &raw, &defs, &mut first, NOW).unwrap();
    assert_eq!(
        (
            first.imported,
            first.updated,
            first.links,
            first.collections
        ),
        (5, 0, 2, 2)
    );
    assert_eq!(keys.len(), 5);
    let ig = posts::get(&c, "ig_3191575067010950169").unwrap().unwrap();
    assert_eq!(ig.summary.user_note.as_deref(), Some("Keep this note"));
    posts::update_user_content(
        &c,
        ig.summary.id,
        &UserContentPatch {
            note: Some(Some("Edited".into())),
            tags: Some(vec!["user-edit".into()]),
        },
        NOW + 1,
    )
    .unwrap();
    let before = serde_json::to_string(&posts::get(&c, "ig_3191575067010950169").unwrap()).unwrap();
    let mut second = Report::default();
    assert!(
        import::apply(&c, &raw, &defs, &mut second, NOW + 5000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        (
            second.imported,
            second.updated,
            second.skipped,
            second.links,
            second.collections
        ),
        (0, 0, 5, 0, 0)
    );
    assert_eq!(
        before,
        serde_json::to_string(&posts::get(&c, "ig_3191575067010950169").unwrap()).unwrap()
    );
    let manual = normalize::post(&raw[4].1, NOW).unwrap();
    let again = normalize::post(&raw[4].1, NOW + 50_000).unwrap();
    assert_eq!(manual.incoming.key, again.incoming.key);
    assert_eq!(manual.incoming.archive_state.as_deref(), Some("link_only"));
    let count: i64 = c
        .query_row("SELECT count(*) FROM web_captures", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn aliases_and_ai_presence_fold_before_merge_and_stay_idempotent() {
    let c = library();
    let raw = vec![
        (
            0,
            json!({"id":"3191575067010950169_1","platform":"instagram","shortcode":"CxKwJ0fLmQZ","caption":"extension-first","aiDescription":"first","aiTags":["glass"],"aiStatus":"done","collections":["n:One"]}),
        ),
        (
            1,
            json!({"id":"CxKwJ0fLmQZ","shortcode":"CxKwJ0fLmQZ","aiDescription":"last","collections":["n:Two"]}),
        ),
        (
            2,
            json!({"id":"3191575067010950169","platform":"instagram","shortcode":"CxKwJ0fLmQZ","text":"tail","aiTags":"invalid-not-present"}),
        ),
    ];
    let mut report = Report::default();
    import::apply(&c, &raw, &BTreeMap::new(), &mut report, NOW).unwrap();
    assert_eq!((report.imported, report.skipped, report.links), (1, 2, 2));
    let before = serde_json::to_string(&posts::get(&c, "ig_3191575067010950169").unwrap()).unwrap();
    assert!(before.contains("last"));
    assert!(before.contains("glass"));
    assert_eq!(
        posts::get(&c, "ig_3191575067010950169")
            .unwrap()
            .unwrap()
            .summary
            .caption
            .as_deref(),
        Some("tail")
    );
    let mut report = Report::default();
    assert!(
        import::apply(&c, &raw, &BTreeMap::new(), &mut report, NOW + 20)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        before,
        serde_json::to_string(&posts::get(&c, "ig_3191575067010950169").unwrap()).unwrap()
    );
}
#[test]
fn collections_external_match_precedes_manual_name_and_preserves_renames() {
    let c = library();
    let def = collections::definition(&json!({"name":"Old","externalId":"77"})).unwrap();
    let (id, created) = collections::ensure(&c, &def, NOW).unwrap();
    assert!(created);
    c.execute(
        "UPDATE collections SET name='My rename',color='#abc' WHERE id=?1",
        [id],
    )
    .unwrap();
    let (same, created) = collections::ensure(
        &c,
        &collections::definition(&json!({"name":"New","externalId":"77"})).unwrap(),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(same, id);
    assert!(!created);
    let (manual, _) = collections::ensure(
        &c,
        &collections::definition(&json!({"name":"Fallback"})).unwrap(),
        NOW,
    )
    .unwrap();
    let (same, created) = collections::ensure(
        &c,
        &collections::definition(&json!({"name":"Fallback","externalId":"99"})).unwrap(),
        NOW,
    )
    .unwrap();
    assert_eq!(manual, same);
    assert!(!created);
    assert_eq!(
        c.query_row("SELECT name FROM collections WHERE id=?1", [id], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "My rename"
    );
}
#[test]
fn rejects_unsupported_envelopes_syntax_complexity_and_oversize_before_growth() {
    for text in [
        "{}",
        "null",
        "{\"posts\":{}}",
        "[] trailing",
        "[{},]",
        "{\"posts\":[],\"posts\":[]}",
        "{\"posts\":[],\"collections\":null}",
        "[{\"x\": [}]",
        "{\"posts\":[],}",
    ] {
        assert!(
            v1::read(text.as_bytes(), |_| Ok::<_, v1::Error>(())).is_err(),
            "{text}"
        );
    }
    let huge = format!(
        "[{{\"text\":\"{}\"}}]",
        "a".repeat(v1::MAX_RECORD_BYTES + 1)
    );
    assert!(v1::read(huge.as_bytes(), |_| Ok::<_, v1::Error>(())).is_err());
    let deep = format!("[{}0{}]", "[".repeat(150), "]".repeat(150));
    assert!(v1::read(deep.as_bytes(), |_| Ok::<_, v1::Error>(())).is_err());
    let nodes = format!("[[{}]]", vec!["0"; 20_000].join(","));
    assert!(v1::read(nodes.as_bytes(), |_| Ok::<_, v1::Error>(())).is_err());
}
#[test]
fn rejects_bad_items_individually_with_original_indices_and_bounds_details() {
    let c = library();
    let raw = vec![
        (0, json!({"platform":"twitter","id":"1","text":"ok"})),
        (1, json!({"platform":"unknown","id":"2"})),
        (2, json!({"platform":"twitter","id":"bad"})),
        (3, json!(5)),
    ];
    let mut r = Report::default();
    import::apply(&c, &raw, &BTreeMap::new(), &mut r, NOW).unwrap();
    assert_eq!(r.imported, 1);
    assert_eq!(
        r.rejected.iter().map(|r| r.index).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for i in 0..3000 {
        r.reject(i, "bad_item");
    }
    assert_eq!(r.rejected.len(), 1000);
    assert_eq!(r.rejected_count, 3003);
}
/// Generates >200 MiB through a tiny read buffer; no giant fixture allocation.
struct Repeated {
    record: Vec<u8>,
    offset: usize,
    left: usize,
    first: bool,
    ended: bool,
}
impl Read for Repeated {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.first {
            self.first = false;
            out[0] = b'[';
            return Ok(1);
        }
        if self.left == 0 {
            if self.ended {
                return Ok(0);
            }
            self.ended = true;
            out[0] = b']';
            return Ok(1);
        }
        if self.offset == self.record.len() {
            self.offset = 0;
            self.left -= 1;
            if self.left == 0 {
                return self.read(out);
            }
            out[0] = b',';
            return Ok(1);
        }
        let n = out.len().min(self.record.len() - self.offset);
        out[..n].copy_from_slice(&self.record[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}
#[test]
fn streaming_two_hundred_mib_has_a_bounded_working_set() {
    let item =
        serde_json::to_vec(&json!({"id":"1","platform":"twitter","text":"x".repeat(16_000)}))
            .unwrap();
    let count = 200 * 1024 * 1024 / item.len() + 1;
    let reader = Repeated {
        record: item,
        offset: 0,
        left: count,
        first: true,
        ended: false,
    };
    let mut visited = 0;
    let mut weight = 0;
    let mut batch = Vec::new();
    v1::read(reader, |r| -> Result<(), v1::Error> {
        if let Record::Post { value, .. } = r {
            let w = v1::weight(&value);
            if batch.len() == 500 || weight + w > v1::BATCH_BYTES {
                batch.clear();
                weight = 0;
            }
            weight += w;
            assert!(weight <= v1::BATCH_BYTES);
            batch.push(value);
            visited += 1;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(visited, count);
}

#[test]
fn manual_and_web_reject_active_url_schemes_and_credentials() {
    for platform in ["manual", "web"] {
        for url in [
            "javascript:alert(1)",
            "file:///private/data",
            "https://user:pass@example.com/",
        ] {
            assert!(
                normalize::post(
                    &json!({"id":"manual:fixture","platform":platform,"postUrl":url}),
                    NOW
                )
                .is_err()
            );
        }
    }
    let mut posts = Vec::new();
    v1::read(
        b"\xef\xbb\xbf[{\"platform\":\"twitter\",\"id\":\"1\"}]".as_slice(),
        |r| {
            posts.push(r);
            Ok::<_, v1::Error>(())
        },
    )
    .unwrap();
    assert_eq!(posts.len(), 1);
}

#[test]
fn folding_unites_memberships_beyond_each_source_record_list_cap() {
    let c = library();
    let mut r = Report::default();
    let raw = vec![
        (
            0,
            json!({"id":"1","platform":"twitter","collections":(0..500).map(|i|format!("n:C{i}")).collect::<Vec<_>>()}),
        ),
        (
            1,
            json!({"id":"1","platform":"twitter","collections":(500..1000).map(|i|format!("n:C{i}")).collect::<Vec<_>>()}),
        ),
    ];
    import::apply(&c, &raw, &BTreeMap::new(), &mut r, NOW).unwrap();
    assert_eq!(
        (r.imported, r.links, r.collections, r.skipped),
        (1, 1000, 1000, 1)
    );
}
