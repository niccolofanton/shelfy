use rusqlite::{Connection, params};
use shelfy_core::{
    schema::{self, Kind},
    tags::{
        clusters::{self, RefinedGroup},
        embeddings,
    },
};
fn library() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut c, Kind::Library).unwrap();
    c
}
#[test]
fn cache_normalizes_namespaces_and_rejects_invalid_batch_atomically() {
    let mut c = library();
    let tags = vec!["lamp".into(), "brass".into()];
    let tx = c.transaction().unwrap();
    assert_eq!(
        embeddings::save(
            &tx,
            "provider-a/model",
            &tags,
            &[vec![3., 4.], vec![0., 2.]]
        )
        .unwrap(),
        2
    );
    assert!(
        embeddings::save(
            &tx,
            "provider-a/model",
            &tags,
            &[vec![1., 2.], vec![f32::NAN, 1.]]
        )
        .is_err()
    );
    assert!(
        embeddings::save(
            &tx,
            "provider-a/model",
            &tags,
            &[vec![1., 2., 3.], vec![1., 2., 3.]]
        )
        .is_err()
    );
    tx.commit().unwrap();
    let cached = embeddings::cached(&c, "provider-a/model", &tags).unwrap();
    assert!((cached["lamp"][0] - 0.6).abs() < 1e-6);
    assert_eq!(cached["brass"], vec![0., 1.]);
    assert!(
        embeddings::cached(&c, "provider-b/model", &tags)
            .unwrap()
            .is_empty()
    );
    let bad = vec!["negative".into(), "length".into(), "infinite".into()];
    for (tag, dim, bytes) in [
        (&bad[0], -1, vec![0u8; 4]),
        (&bad[1], 2, vec![0u8; 4]),
        (&bad[2], 1, f32::INFINITY.to_le_bytes().to_vec()),
    ] {
        c.execute(
            "INSERT INTO tag_embeddings(tag_norm,model,dim,vec) VALUES(?1,'corrupt',?2,?3)",
            params![tag, dim, bytes],
        )
        .unwrap();
    }
    assert!(embeddings::cached(&c, "corrupt", &bad).unwrap().is_empty());
}
#[test]
fn incremental_chunks_keep_prior_and_accepted_memberships_and_monotonic_ids() {
    let mut c = library();
    let g = |label: &str, tags: &[&str]| RefinedGroup {
        label: label.into(),
        tags: tags.iter().map(|t| (*t).into()).collect(),
    };
    let tx = c.transaction().unwrap();
    clusters::save_run(&tx, &[g("accepted", &["a", "b"])], 1, 1).unwrap();
    clusters::review(&tx, 1, true, None, 1).unwrap();
    clusters::clear_proposals(&tx, 2).unwrap();
    assert_eq!(
        clusters::append_proposals(&tx, &[g("first", &["a", "c", "d"])], 2, 2)
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        clusters::append_proposals(&tx, &[g("second", &["c", "e", "f"])], 2, 2)
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        clusters::append_proposals(&tx, &[g("duplicate", &["e", "f"])], 2, 2)
            .unwrap()
            .count,
        0
    );
    clusters::clear_proposals(&tx, 2).unwrap();
    assert_eq!(
        clusters::append_proposals(&tx, &[g("replacement", &["c", "d"])], 3, 3)
            .unwrap()
            .count,
        1
    );
    assert!(clusters::review(&tx, 2, true, None, 4).is_err());
    let ids: Vec<i64> = tx
        .prepare("SELECT id FROM tag_cluster ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids, vec![1, 4]);
    tx.commit().unwrap();
}

#[test]
fn clearing_legacy_proposals_initializes_sequence_before_removing_max_id() {
    let mut c = library();
    let tx = c.transaction().unwrap();
    tx.execute("INSERT INTO tag_cluster(id,label,label_norm,status,run_id,created_at,updated_at) VALUES(99,'legacy','legacy','proposed',1,1,1)",[]).unwrap();
    tx.execute(
        "INSERT INTO tag_cluster_membership(tag_norm,cluster_id) VALUES('a',99),('b',99)",
        [],
    )
    .unwrap();
    clusters::save_run(
        &tx,
        &[RefinedGroup {
            label: "new".into(),
            tags: vec!["a".into(), "b".into()],
        }],
        2,
        2,
    )
    .unwrap();
    assert_eq!(
        tx.query_row("SELECT id FROM tag_cluster", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        100
    );
}
