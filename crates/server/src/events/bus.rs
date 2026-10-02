//! One user's bus: the broadcast channel that feeds their open streams, the
//! replay ring, event ids, and the throttles of their events.
//!
//! Everything that orders events happens under one mutex: an event gets its
//! sequence number, enters the ring and goes out on the channel in one step,
//! and a subscriber joins the channel and copies the ring in one step. So a
//! replay and the live events after it never overlap and never leave a gap.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio::time::Instant;

use super::coalesce::{Offer, PostKeys, Throttle};
use super::model::{
    ChangeReason, EventTopic, JobUpdatedEvent, PostsChangedEvent, ResyncReason, StatsChangedEvent,
};
use super::{
    CHANNEL_CAPACITY, JOB_WINDOW, POSTS_WINDOW, REPLAY_EVENTS, REPLAY_WINDOW, STATS_WINDOW,
};

/// A published event, shared by the ring and every subscriber.
#[derive(Debug)]
pub struct Published {
    /// Position in the user's stream, from 1.
    pub seq: u64,
    /// The SSE `id:`, `<epoch>-<seq>`.
    pub id: Box<str>,
    /// The SSE `event:`.
    pub topic: EventTopic,
    /// The payload as one line of JSON, the SSE `data:`.
    pub data: Box<str>,
    /// When it was published.
    pub at: Instant,
}

/// Which held change a flush sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FlushKey {
    Posts(ChangeReason),
    Stats,
    Job(i64),
}

struct State {
    /// Sequence number of the newest event; 0 before the first.
    head: u64,
    /// The newest events, oldest first: at most [`REPLAY_EVENTS`], none
    /// older than [`REPLAY_WINDOW`].
    ring: VecDeque<Arc<Published>>,
    posts: HashMap<ChangeReason, Throttle<PostKeys>>,
    stats: Throttle<StatsChangedEvent>,
    jobs: HashMap<i64, Throttle<JobUpdatedEvent>>,
    /// Last publish, subscription or disconnection.
    last_active: Instant,
}

pub(super) struct UserBus {
    /// Unique per bus: ids of another bus (another user, or this user before
    /// a restart) never resolve here.
    epoch: u64,
    sender: broadcast::Sender<Arc<Published>>,
    state: Mutex<State>,
}

impl UserBus {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            epoch: new_epoch(),
            sender: broadcast::Sender::new(CHANNEL_CAPACITY),
            state: Mutex::new(State {
                head: 0,
                ring: VecDeque::with_capacity(REPLAY_EVENTS),
                posts: HashMap::new(),
                stats: Throttle::new(STATS_WINDOW),
                jobs: HashMap::new(),
                last_active: now,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock in a way that breaks the
        // state's invariants: recover.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn event_id(&self, seq: u64) -> String {
        format!("{:016x}-{seq}", self.epoch)
    }

    /// The id of the newest event.
    pub(super) fn head_id(&self) -> String {
        let head = self.lock().head;
        self.event_id(head)
    }

    /// Open connections.
    pub(super) fn receivers(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Publishes `data` (a payload as JSON) as `topic` now, under the lock.
    fn emit(&self, state: &mut State, topic: EventTopic, data: String, now: Instant) {
        state.head += 1;
        let event = Arc::new(Published {
            seq: state.head,
            id: self.event_id(state.head).into(),
            topic,
            data: data.into(),
            at: now,
        });
        if state.ring.len() == REPLAY_EVENTS {
            state.ring.pop_front();
        }
        state.ring.push_back(Arc::clone(&event));
        prune_ring(&mut state.ring, now);
        state.last_active = now;
        // An error only means nobody is connected; the ring keeps the event.
        let _ = self.sender.send(event);
    }

    /// Publishes an unthrottled event.
    pub(super) fn publish(&self, topic: EventTopic, payload: &impl Serialize, now: Instant) {
        let data = to_json(payload);
        let mut state = self.lock();
        self.emit(&mut state, topic, data, now);
    }

    /// Offers a `posts.changed`; returns when to flush, if the change is held.
    pub(super) fn offer_posts(
        &self,
        reason: ChangeReason,
        keys: PostKeys,
        now: Instant,
    ) -> Option<Instant> {
        let mut state = self.lock();
        let offer = state
            .posts
            .entry(reason)
            .or_insert_with(|| Throttle::new(POSTS_WINDOW))
            .offer(keys, now);
        self.settle(&mut state, offer, now, |keys| posts_changed(reason, keys))
    }

    /// Offers a `stats.changed`; returns when to flush, if it is held.
    pub(super) fn offer_stats(&self, now: Instant) -> Option<Instant> {
        let mut state = self.lock();
        let offer = state.stats.offer(StatsChangedEvent {}, now);
        self.settle(&mut state, offer, now, |stats| {
            (EventTopic::StatsChanged, to_json(&stats))
        })
    }

    /// Offers a `job.updated`; returns when to flush, if it is held.
    pub(super) fn offer_job(&self, job: JobUpdatedEvent, now: Instant) -> Option<Instant> {
        let mut state = self.lock();
        let id = job.id;
        if !state.jobs.contains_key(&id) {
            // Forget jobs whose throttle has nothing left to do, so the map
            // holds only the jobs that moved within the last window.
            state.jobs.retain(|_, throttle| !throttle.is_idle(now));
        }
        let offer = state
            .jobs
            .entry(id)
            .or_insert_with(|| Throttle::new(JOB_WINDOW))
            .offer(job, now);
        self.settle(&mut state, offer, now, |job| {
            (EventTopic::JobUpdated, to_json(&job))
        })
    }

    /// Sends what the throttle released, or reports when to flush.
    fn settle<P>(
        &self,
        state: &mut State,
        offer: Offer<P>,
        now: Instant,
        encode: impl FnOnce(P) -> (EventTopic, String),
    ) -> Option<Instant> {
        match offer {
            Offer::Now(change) => {
                let (topic, data) = encode(change);
                self.emit(state, topic, data, now);
                None
            }
            Offer::FlushAt(at) => Some(at),
            Offer::Merged => None,
        }
    }

    /// Sends the held change of `key`, if any.
    pub(super) fn flush(&self, key: FlushKey, now: Instant) {
        let mut state = self.lock();
        let released = match key {
            FlushKey::Posts(reason) => state
                .posts
                .get_mut(&reason)
                .and_then(|t| t.flush(now))
                .map(|keys| posts_changed(reason, keys)),
            FlushKey::Stats => state
                .stats
                .flush(now)
                .map(|stats| (EventTopic::StatsChanged, to_json(&stats))),
            FlushKey::Job(id) => state
                .jobs
                .get_mut(&id)
                .and_then(|t| t.flush(now))
                .map(|job| (EventTopic::JobUpdated, to_json(&job))),
        };
        if let Some((topic, data)) = released {
            self.emit(&mut state, topic, data, now);
        }
    }

    /// Joins the stream. `resume` is the id of the last event the client
    /// has, from `Last-Event-ID` or `lastEventId`.
    pub(super) fn subscribe(self: &Arc<Self>, resume: Option<&str>, now: Instant) -> Subscription {
        let mut state = self.lock();
        // Under the lock: no event can slip between the copy of the ring and
        // the first live event.
        let receiver = self.sender.subscribe();
        prune_ring(&mut state.ring, now);
        state.last_active = now;
        let head_id = self.event_id(state.head);
        let mut queue = VecDeque::new();
        if let Some(text) = resume {
            match self.resume_point(&state, text) {
                Ok(seq) => queue.extend(
                    state
                        .ring
                        .iter()
                        .filter(|event| event.seq > seq)
                        .map(|event| Delivery::Event(Arc::clone(event))),
                ),
                Err(reason) => queue.push_back(Delivery::Resync {
                    reason,
                    id: head_id.clone(),
                }),
            }
        }
        drop(state);
        Subscription {
            bus: Arc::clone(self),
            receiver,
            head_id,
            fresh: resume.is_none(),
            queue,
        }
    }

    /// The sequence number of `text` if every event after it is still in
    /// the ring.
    fn resume_point(&self, state: &State, text: &str) -> Result<u64, ResyncReason> {
        let (epoch, seq) = text
            .split_once('-')
            .and_then(|(epoch, seq)| {
                Some((
                    u64::from_str_radix(epoch, 16).ok()?,
                    seq.parse::<u64>().ok()?,
                ))
            })
            .ok_or(ResyncReason::Unknown)?;
        if epoch != self.epoch || seq > state.head {
            return Err(ResyncReason::Unknown);
        }
        let first_kept = state.ring.front().map_or(state.head + 1, |event| event.seq);
        if seq + 1 < first_kept {
            return Err(ResyncReason::Expired);
        }
        Ok(seq)
    }

    /// Drops ring events older than the replay window and throttles with
    /// nothing left to do.
    pub(super) fn prune(&self, now: Instant) {
        let mut state = self.lock();
        prune_ring(&mut state.ring, now);
        state.jobs.retain(|_, throttle| !throttle.is_idle(now));
    }

    /// Whether the bus can be dropped at `now`: no change held, and nothing
    /// published, subscribed or disconnected for the replay window. (The
    /// caller checks that nobody else holds it.)
    pub(super) fn is_idle(&self, now: Instant) -> bool {
        let state = self.lock();
        now >= state.last_active + REPLAY_WINDOW
            && !state.stats.is_holding()
            && state.posts.values().all(|t| !t.is_holding())
            && state.jobs.values().all(|t| !t.is_holding())
    }

    fn touch(&self, now: Instant) {
        self.lock().last_active = now;
    }
}

fn posts_changed(reason: ChangeReason, keys: PostKeys) -> (EventTopic, String) {
    let payload = PostsChangedEvent {
        keys: keys.into_keys(),
        reason,
    };
    (EventTopic::PostsChanged, to_json(&payload))
}

fn to_json(payload: &impl Serialize) -> String {
    serde_json::to_string(payload).expect("event payloads serialize")
}

fn prune_ring(ring: &mut VecDeque<Arc<Published>>, now: Instant) {
    while ring
        .front()
        .is_some_and(|event| now >= event.at + REPLAY_WINDOW)
    {
        ring.pop_front();
    }
}

/// A process-unique, unpredictable bus epoch.
fn new_epoch() -> u64 {
    getrandom::u64().unwrap_or_else(|_| {
        // Without an OS generator, the clock still separates restarts.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        #[allow(clippy::cast_possible_truncation)] // the low 64 bits are enough
        let nanos = nanos as u64;
        nanos ^ 0x9E37_79B9_7F4A_7C15
    })
}

/// What a subscription delivers next.
#[derive(Debug)]
pub enum Delivery {
    /// A published event.
    Event(Arc<Published>),
    /// Events were lost; `id` is the position the stream continues from.
    Resync {
        /// Why.
        reason: ResyncReason,
        /// The SSE `id:` of the `resync` event.
        id: String,
    },
}

/// One open stream's view of a user's bus: replayed events first, then live
/// ones, with a `resync` where events were lost.
pub struct Subscription {
    bus: Arc<UserBus>,
    receiver: broadcast::Receiver<Arc<Published>>,
    head_id: String,
    fresh: bool,
    queue: VecDeque<Delivery>,
}

impl Subscription {
    /// The id of the newest event when the subscription started.
    #[must_use]
    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    /// Whether the client gave no resume point.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// The next delivery. It waits for one as long as it takes.
    pub async fn next(&mut self) -> Delivery {
        if let Some(delivery) = self.queue.pop_front() {
            return delivery;
        }
        match self.receiver.recv().await {
            Ok(event) => Delivery::Event(event),
            Err(RecvError::Lagged(_)) => Delivery::Resync {
                reason: ResyncReason::Lagged,
                id: self.skip_to_newest(),
            },
            // The subscription holds the bus, and with it the sender.
            Err(RecvError::Closed) => std::future::pending().await,
        }
    }

    /// After a lag: drops what the channel still holds for this connection
    /// (the client reloads everything anyway) and returns the id the stream
    /// continues from.
    fn skip_to_newest(&mut self) -> String {
        let mut newest = None;
        loop {
            match self.receiver.try_recv() {
                Ok(event) => newest = Some(event),
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        newest.map_or_else(|| self.bus.head_id(), |event| event.id.to_string())
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.touch(Instant::now());
    }
}
