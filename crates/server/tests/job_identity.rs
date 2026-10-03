//! Durable identities across retention, transaction aborts and old orphan markers.
mod support;
use rusqlite::{Connection, params};
use shelfy_core::repo::RepoError;
use shelfy_core::schema::{self, Kind};
use shelfy_server::ai::runs::{self, RunKind};
use shelfy_server::control::jobs::{self, Inserted, NewJobRow};
use shelfy_server::imports::{self, Checkpoint};
use shelfy_server::jobs::NewJob;
use shelfy_server::state::AppState;
use std::collections::BTreeSet;
use support::TestState;
use support::library::{ALICE, NOW};

fn new<'a>(user: &'a str, dedupe: Option<&'a str>) -> NewJobRow<'a> {
    NewJobRow {
        user_id: user,
        kind: "ai.run",
        dedupe_key: dedupe,
        priority: 100,
        payload_json: "{}",
        max_attempts: 1,
        run_at: NOW,
    }
}
fn created(value: Inserted) -> jobs::JobRow {
    match value {
        Inserted::Created(row) => row,
        Inserted::Existing(_) => panic!("new job required"),
    }
}
fn counter(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT last_id FROM job_id_counter WHERE singleton=1",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[tokio::test]
async fn enqueue_finished_marker_clear_or_prune_then_enqueue_never_loads_the_marker() {
    for clear in [true, false] {
        let t = TestState::new();
        t.add_user(ALICE);
        let old = t
            .state
            .jobs()
            .enqueue(NewJob::new(ALICE, "ai.run"))
            .await
            .unwrap()
            .job;
        t.write(ALICE, |tx| {
            let mut plan = runs::snapshot(tx, RunKind::Aliases)?;
            plan.finished = true;
            plan.incarnation = Some(old.incarnation.clone());
            runs::save(tx, old.id, &plan, NOW)?;
            Ok(())
        })
        .await;
        t.control()
            .execute(
                "UPDATE jobs SET state='succeeded',finished_at=?2 WHERE id=?1",
                params![old.id, NOW],
            )
            .unwrap();
        if clear {
            assert_eq!(
                t.state
                    .jobs()
                    .clear_finished(ALICE, "ai.run")
                    .await
                    .unwrap(),
                1
            );
        } else {
            assert_eq!(jobs::prune_finished(&t.control(), NOW + 1).unwrap(), 1);
        }
        let reopened = AppState::open(t.state.config().clone()).unwrap();
        let current = reopened
            .jobs()
            .enqueue(NewJob::new(ALICE, "ai.run"))
            .await
            .unwrap()
            .job;
        assert!(current.id > old.id);
        assert_ne!(current.incarnation, old.incarnation);
        let db = t.state.user_db(ALICE).await.unwrap();
        assert!(
            db.read(|c| runs::load_for(c, current.id, &current.incarnation))
                .unwrap()
                .is_none()
        );
        assert!(
            db.read(|c| runs::load_for(c, old.id, &old.incarnation))
                .unwrap()
                .unwrap()
                .finished
        );
    }
}

#[tokio::test]
async fn unbound_or_mismatched_orphan_plans_and_reports_do_not_ghost_a_new_job() {
    let t = TestState::new();
    t.add_user(ALICE);
    let job = t
        .state
        .jobs()
        .enqueue(NewJob::new(ALICE, "ai.run"))
        .await
        .unwrap()
        .job;
    let db = t.state.user_db(ALICE).await.unwrap();
    for nonce in [None, Some("job:prior-incarnation".to_owned())] {
        db.write(|tx| {
            let mut plan = runs::snapshot(tx, RunKind::Aliases)?;
            plan.finished = true;
            plan.incarnation = nonce.clone();
            runs::save(tx, job.id, &plan, NOW)?;
            imports::save(
                tx,
                job.id,
                &Checkpoint {
                    incarnation: nonce.clone(),
                    upload_id: Some("old-upload".into()),
                    complete: true,
                    next: 999,
                    ..Checkpoint::default()
                },
            )?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        assert!(
            db.read(|c| runs::load_for(c, job.id, &job.incarnation))
                .unwrap()
                .is_none()
        );
        let fresh = db
            .read(|c| imports::checkpoint_for(c, job.id, &job.incarnation, "new-upload"))
            .unwrap();
        assert_eq!(fresh.next, 0);
        assert!(!fresh.complete);
        assert_eq!(fresh.incarnation.as_deref(), Some(job.incarnation.as_str()));
    }
}

#[tokio::test]
async fn legacy_import_checkpoint_fails_without_mutation_and_bound_retry_retains_the_cursor() {
    let t = TestState::new();
    t.add_user(ALICE);
    let job = t
        .state
        .jobs()
        .enqueue(NewJob::new(ALICE, "import"))
        .await
        .unwrap()
        .job;
    let db = t.state.user_db(ALICE).await.unwrap();
    db.write(|tx| {
        imports::save(
            tx,
            job.id,
            &Checkpoint {
                next: 500,
                definitions_done: true,
                ..Checkpoint::default()
            },
        )
    })
    .unwrap();
    let generation = db.generation();
    let legacy = "legacy:00000000000000000000000000000000";
    let error = db
        .read(|c| imports::checkpoint_for(c, job.id, legacy, "source"))
        .unwrap_err();
    assert_eq!(
        error.code(),
        shelfy_server::error::ErrorCode::ImportCheckpointUnbound
    );
    assert_eq!(generation, db.generation());
    assert_eq!(
        db.read(|c| imports::checkpoint(c, job.id)).unwrap().next,
        500
    );
    db.write(|tx| {
        imports::save(
            tx,
            job.id,
            &Checkpoint {
                incarnation: Some(job.incarnation.clone()),
                upload_id: Some("source".into()),
                next: 500,
                definitions_done: true,
                ..Checkpoint::default()
            },
        )
    })
    .unwrap();
    let saved = db
        .read(|c| imports::checkpoint_for(c, job.id, &job.incarnation, "source"))
        .unwrap();
    assert_eq!(saved.next, 500);
    assert!(saved.definitions_done);
    // A source mismatch cannot borrow even a bound cursor of this lifetime.
    assert_eq!(
        db.read(|c| imports::checkpoint_for(c, job.id, &job.incarnation, "other-source"))
            .unwrap()
            .next,
        0
    );
}

#[tokio::test]
async fn failed_insert_dedupe_and_aborted_transaction_do_not_advance_the_highwater() {
    let t = TestState::new();
    t.add_user(ALICE);
    let old = t
        .state
        .control()
        .write(|tx| jobs::insert(tx, &new(ALICE, Some("same")), NOW))
        .unwrap();
    let old = created(old);
    assert_eq!(counter(&t.control()), old.id);
    assert!(matches!(
        t.state
            .control()
            .write(|tx| jobs::insert(tx, &new(ALICE, Some("same")), NOW))
            .unwrap(),
        Inserted::Existing(_)
    ));
    assert_eq!(counter(&t.control()), old.id);
    assert!(
        t.state
            .control()
            .write(|tx| jobs::insert(tx, &new("missing-user", None), NOW))
            .is_err()
    );
    assert_eq!(counter(&t.control()), old.id);
    let rolled = t.state.control().write(|tx| -> Result<(), RepoError> {
        jobs::insert(tx, &new(ALICE, None), NOW)?;
        Err(RepoError::Conflict("synthetic rollback"))
    });
    assert!(rolled.is_err());
    assert_eq!(counter(&t.control()), old.id);
    let current = created(
        t.state
            .control()
            .write(|tx| jobs::insert(tx, &new(ALICE, None), NOW))
            .unwrap(),
    );
    assert_eq!(current.id, old.id + 1);
    assert_ne!(current.incarnation, old.incarnation);
    assert!(t.control().execute("INSERT INTO jobs(id,user_id,kind,max_attempts,run_at,created_at,updated_at,state) VALUES (?1,?2,'ai.run',1,1,1,1,'queued')",params![old.id-1,ALICE]).is_err());
}

#[tokio::test]
async fn overflow_stops_allocation_without_falling_back_to_random_rowids() {
    let t = TestState::new();
    t.add_user(ALICE);
    t.control()
        .execute("UPDATE job_id_counter SET last_id=?1", [i64::MAX - 1])
        .unwrap();
    let last = created(
        t.state
            .control()
            .write(|tx| jobs::insert(tx, &new(ALICE, Some("last")), NOW))
            .unwrap(),
    );
    assert_eq!(last.id, i64::MAX);
    assert_eq!(counter(&t.control()), i64::MAX);
    assert!(matches!(
        t.state
            .control()
            .write(|tx| jobs::insert(tx, &new(ALICE, Some("last")), NOW))
            .unwrap(),
        Inserted::Existing(_)
    ));
    assert!(
        t.state
            .control()
            .write(|tx| jobs::insert(tx, &new(ALICE, None), NOW))
            .is_err()
    );
    assert_eq!(counter(&t.control()), i64::MAX);
    assert!(
        t.control()
            .execute("UPDATE job_id_counter SET last_id=last_id+1", [])
            .is_err()
    );
}

#[tokio::test]
async fn independent_control_connections_allocate_unique_identities_atomically() {
    let t = TestState::new();
    t.add_user(ALICE);
    let second = AppState::open(t.state.config().clone()).unwrap();
    let mut tasks = Vec::new();
    for i in 0..32 {
        let state = if i % 2 == 0 {
            t.state.clone()
        } else {
            second.clone()
        };
        tasks.push(tokio::spawn(async move {
            state
                .jobs()
                .enqueue(NewJob::new(ALICE, "ai.run"))
                .await
                .unwrap()
                .job
        }));
    }
    let mut ids = BTreeSet::new();
    let mut nonces = BTreeSet::new();
    for task in tasks {
        let row = task.await.unwrap();
        assert!(row.incarnation.starts_with("job:"));
        ids.insert(row.id);
        nonces.insert(row.incarnation);
    }
    assert_eq!(ids.len(), 32);
    assert_eq!(nonces.len(), 32);
    assert_eq!(counter(&t.control()), *ids.last().unwrap());
}

#[test]
fn upgrade_v6_seeds_from_jobs_and_exports_and_distinguishes_legacy_jobs() {
    let mut control = Connection::open_in_memory().unwrap();
    control
        .execute_batch(include_str!(
            "../../core/tests/fixtures/schema/control-v6.sql"
        ))
        .unwrap();
    let highest: i64 = control
        .query_row("SELECT max(id) FROM jobs", [], |r| r.get(0))
        .unwrap();
    control
        .execute("UPDATE exports SET job_id=?1", [highest + 20])
        .unwrap();
    schema::migrate(&mut control, Kind::Control).unwrap();
    assert_eq!(counter(&control), highest + 20);
    let user: String = control
        .query_row("SELECT user_id FROM jobs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let old = jobs::get(&control, &user, highest).unwrap().unwrap();
    assert!(old.incarnation.starts_with("legacy:"));
    let current = created(jobs::insert(&control, &new(&user, None), NOW).unwrap());
    assert_eq!(current.id, highest + 21);
    assert!(current.incarnation.starts_with("job:"));
    let minimum: i64 = control
        .query_row("SELECT min_reader_version FROM schema_compat", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(minimum, 7);
}
