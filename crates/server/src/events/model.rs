//! The events of the realtime stream (plan §2.10): their names, their
//! payloads, and the topic filter of `GET /api/v1/events`.
//!
//! On the wire an event is an SSE frame: `event:` names it, `data:` holds its
//! payload as one line of JSON, and `id:` (on published events) is the
//! position to resume from. Payloads follow the conventions of the rest of the
//! API (camelCase, unix milliseconds, fields always present and `null` when
//! empty). [`ServerEvent`] lists every name with its payload type, for the
//! generated client.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use shelfy_core::repo::notifications as core;
use utoipa::ToSchema;

/// The name of a published event; the `topics` filter selects by it.
///
/// `hello` and `resync` are not topics: every stream gets them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum EventTopic {
    /// Posts were added, changed or removed.
    #[serde(rename = "posts.changed")]
    PostsChanged,
    /// The library counters changed.
    #[serde(rename = "stats.changed")]
    StatsChanged,
    /// A background job moved on.
    #[serde(rename = "job.updated")]
    JobUpdated,
    /// A new notification.
    #[serde(rename = "notification")]
    Notification,
    /// The browser extension connected, disconnected or changed version.
    #[serde(rename = "extension.status")]
    ExtensionStatus,
    /// An AI provider's state changed (P3-09).
    #[serde(rename = "provider.status")]
    ProviderStatus,
    /// A sync run advanced.
    #[serde(rename = "sync.progress")]
    SyncProgress,
}

impl EventTopic {
    /// Every topic, in a stable order.
    pub const ALL: [Self; 7] = [
        Self::PostsChanged,
        Self::StatsChanged,
        Self::JobUpdated,
        Self::Notification,
        Self::ExtensionStatus,
        Self::ProviderStatus,
        Self::SyncProgress,
    ];

    /// The event name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PostsChanged => "posts.changed",
            Self::StatsChanged => "stats.changed",
            Self::JobUpdated => "job.updated",
            Self::Notification => "notification",
            Self::ExtensionStatus => "extension.status",
            Self::ProviderStatus => "provider.status",
            Self::SyncProgress => "sync.progress",
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The topics a stream carries. A `u16` bitmask (P2-G11): P2 added
/// `extension.status` and `sync.progress`, P3 `provider.status`, and more may
/// follow; `ai.stream` (P3-13) is live-only and never a bit here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicSet(u16);

impl TopicSet {
    /// Every topic: the stream of a client that names none. Opt-in topics
    /// (`ai.stream`, P3) will stay out of it.
    pub const ALL: Self = Self(0b111_1111);

    /// The topics named in `topics`, or [`TopicSet::ALL`] when it is empty.
    #[must_use]
    pub fn from_topics(topics: &[EventTopic]) -> Self {
        if topics.is_empty() {
            return Self::ALL;
        }
        Self(topics.iter().fold(0, |bits, topic| bits | topic.bit()))
    }

    /// Whether `topic` is in the set.
    #[must_use]
    pub const fn contains(self, topic: EventTopic) -> bool {
        self.0 & topic.bit() != 0
    }
}

/// `hello`, the first event of every stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelloEvent {
    /// Version of the server build; a change means a new deploy.
    pub version: String,
    /// Milliseconds between two heartbeat comments. A stream silent for much
    /// longer is dead: reconnect.
    pub heartbeat_ms: u64,
    /// The id of the newest event of your stream when it opened: resume from
    /// it (`Last-Event-ID` or `lastEventId`) to miss nothing after this point.
    pub last_event_id: String,
}

/// Why a stream asks the client to reload everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResyncReason {
    /// The events after the resume point are no longer kept (more than 256
    /// events or more than 5 minutes ago).
    Expired,
    /// The resume point is not an event of this stream: the server restarted,
    /// or the id is malformed.
    Unknown,
    /// This connection fell more than 256 events behind; they were dropped.
    Lagged,
}

/// `resync`: events were lost. Reload every view (posts, stats, jobs,
/// notifications), then carry on with the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResyncEvent {
    /// Why; the reaction is the same for every reason.
    pub reason: ResyncReason,
}

/// What changed posts (plan §2.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChangeReason {
    /// New or updated posts from the extension or a shared link.
    Ingest,
    /// Media were stored or removed.
    Archive,
    /// AI results arrived or were cleared.
    Ai,
    /// The user edited posts or folders.
    Edit,
    /// Posts went to or came back from the trash, or were purged.
    Delete,
    /// A website capture finished.
    Capture,
    /// An import or a migration installed posts.
    Import,
}

impl ChangeReason {
    /// Every reason, in a stable order.
    pub const ALL: [Self; 7] = [
        Self::Ingest,
        Self::Archive,
        Self::Ai,
        Self::Edit,
        Self::Delete,
        Self::Capture,
        Self::Import,
    ];
}

/// `posts.changed`: reload the views that show these posts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostsChangedEvent {
    /// The posts that changed, at most 200. `null` when more changed, or the
    /// change was not about listed posts: reload the whole view.
    #[schema(required = true)]
    pub keys: Option<Vec<String>>,
    /// What changed them.
    pub reason: ChangeReason,
}

/// `stats.changed`: reload `GET /stats`. No payload: `{}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatsChangedEvent {}

/// State of a job (plan §2.6 `jobs.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting to run.
    Queued,
    /// Running.
    Running,
    /// Finished.
    Succeeded,
    /// Failed for good; `errorCode` says why.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

impl JobState {
    /// Whether the job is over.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// `job.updated`: the latest state of a job, at most every 250 ms per job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobUpdatedEvent {
    /// Job id.
    pub id: i64,
    /// Job kind (`archive.drain`, `capture.site`, …).
    pub kind: String,
    /// State.
    pub state: JobState,
    /// Progress from 0 to 1, when known.
    #[schema(required = true)]
    pub progress: Option<f64>,
    /// Current stage, a code, when the job has stages.
    #[schema(required = true)]
    pub stage: Option<String>,
    /// The post the job works on, if one.
    #[schema(required = true)]
    pub post_key: Option<String>,
    /// Why the job failed (`failed`), or its last error before a retry.
    #[schema(required = true)]
    pub error_code: Option<String>,
}

/// A notification: an item of `GET /notifications` and the payload of the
/// `notification` event. It carries codes, not text: the client writes the
/// message from `kind`, `code` and `params`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// Id; newer notifications have larger ids.
    pub id: i64,
    /// Area (`job`, `migration`, `quota`, …).
    pub kind: String,
    /// What happened (`job.failed`, …), stable like an error code.
    pub code: String,
    /// Values for the message; `{}` when there are none.
    pub params: BTreeMap<String, serde_json::Value>,
    /// Where the notification leads: a post key or an app route.
    #[schema(required = true)]
    pub target: Option<String>,
    /// When it was created.
    pub created_at: i64,
    /// When it was marked read; `null` while unread.
    #[schema(required = true)]
    pub read_at: Option<i64>,
}

impl From<core::Notification> for Notification {
    fn from(n: core::Notification) -> Self {
        Self {
            id: n.id,
            kind: n.kind,
            code: n.code,
            params: n.params.into_iter().collect(),
            target: n.target,
            created_at: n.created_at,
            read_at: n.read_at,
        }
    }
}

/// `extension.status`: whether the user's browser extension is connected
/// (plan §2.10, contract C8), sent when `connected` or `version` changes.
/// `GET /api/v1/extension/status` answers the same object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatusEvent {
    /// One of the user's extension tokens made a request in the last 10
    /// minutes.
    pub connected: bool,
    /// The extension's version from its `X-Shelfy-Extension` header: the
    /// highest among the connected browsers, or the last one known once
    /// disconnected. `null` when none sent a valid version.
    #[schema(required = true)]
    pub version: Option<String>,
    /// The newest request of the user's extension since the server started,
    /// unix ms; `null` when there was none.
    #[schema(required = true)]
    pub last_seen_at: Option<i64>,
}

/// The state of an AI provider (plan §2.15 "Reliability", P3-09). The circuit
/// breaker and the operator's health probe move a provider between these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    /// Working: calls go through.
    Ok,
    /// Working, but recent calls failed or were slow: calls still go through.
    Degraded,
    /// Unreachable (the operator's node is asleep or offline). Queued work
    /// waits without spending a try until a health probe passes.
    Offline,
    /// The circuit breaker is open after repeated failures: calls are held
    /// for a cool-down.
    Down,
    /// The provider refused the key: the account's AI queue is paused until a
    /// new key or a passing probe.
    InvalidKey,
}

impl ProviderState {
    /// The wire form, for example `invalid_key`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Offline => "offline",
            Self::Down => "down",
            Self::InvalidKey => "invalid_key",
        }
    }
}

/// `provider.status`: an AI provider changed state (P3-09), sent on each
/// change. The web app shows the provider's state and reacts (an offline
/// operator shows AI work as waiting, an invalid key asks for a new one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatusEvent {
    /// The provider: `operator` for the operator's node, else the user's
    /// provider id.
    pub provider_id: String,
    /// The new state.
    pub state: ProviderState,
}

/// The listing a sync run walks, in a `sync.progress` event (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncListing {
    /// `ig_saved`, `ig_collection`, `x_bookmarks` or `pin_board`.
    pub kind: String,
    /// The folder or board id, when the listing names one.
    #[schema(required = true)]
    pub external_id: Option<String>,
    /// The folder or board name as the page showed it.
    #[schema(required = true)]
    pub name: Option<String>,
}

/// `sync.progress`: a sync run advanced (plan §2.10, contract C8), sent at
/// most once a second per run. The counters are the run's running totals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgressEvent {
    /// The run's id.
    pub run_id: String,
    /// The platform (`instagram`, `twitter`, `pinterest`).
    pub platform: String,
    /// The listing it walks.
    pub listing: SyncListing,
    /// What started the run.
    pub trigger: String,
    /// Items scanned so far.
    pub scanned: i64,
    /// New posts so far.
    pub inserted: i64,
    /// Known posts that changed so far.
    pub updated: i64,
    /// Known posts so far.
    pub known: i64,
    /// Pages scanned, as the last patch reported.
    pub pages: i64,
    /// The run's state (`running`, `done`, `stopped`, `failed`).
    pub state: String,
}

/// Every event of `GET /api/v1/events`: `event` is the SSE event name and
/// `data` the JSON of its `data:` line. A type for clients; no response sends
/// this object as such.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "event", content = "data")]
pub enum ServerEvent {
    /// First event of every stream.
    #[serde(rename = "hello")]
    Hello(HelloEvent),
    /// Events were lost: reload everything.
    #[serde(rename = "resync")]
    Resync(ResyncEvent),
    /// Posts changed.
    #[serde(rename = "posts.changed")]
    PostsChanged(PostsChangedEvent),
    /// The counters changed.
    #[serde(rename = "stats.changed")]
    StatsChanged(StatsChangedEvent),
    /// A job moved on.
    #[serde(rename = "job.updated")]
    JobUpdated(JobUpdatedEvent),
    /// A new notification.
    #[serde(rename = "notification")]
    Notification(Notification),
    /// The browser extension connected, disconnected or changed version.
    #[serde(rename = "extension.status")]
    ExtensionStatus(ExtensionStatusEvent),
    /// An AI provider changed state.
    #[serde(rename = "provider.status")]
    ProviderStatus(ProviderStatusEvent),
    /// A sync run advanced.
    #[serde(rename = "sync.progress")]
    SyncProgress(SyncProgressEvent),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names_match_their_wire_form() {
        for topic in EventTopic::ALL {
            assert_eq!(serde_json::to_value(topic).unwrap(), topic.as_str());
        }
    }

    #[test]
    fn topic_sets_default_to_every_topic() {
        let all = TopicSet::from_topics(&[]);
        assert_eq!(all, TopicSet::ALL);
        assert!(EventTopic::ALL.into_iter().all(|t| all.contains(t)));
        let some = TopicSet::from_topics(&[EventTopic::JobUpdated, EventTopic::JobUpdated]);
        assert!(some.contains(EventTopic::JobUpdated));
        assert!(!some.contains(EventTopic::PostsChanged));
        assert!(!some.contains(EventTopic::Notification));
    }

    #[test]
    fn payloads_have_their_documented_shape() {
        let event = ServerEvent::PostsChanged(PostsChangedEvent {
            keys: None,
            reason: ChangeReason::Ai,
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({ "event": "posts.changed", "data": { "keys": null, "reason": "ai" } })
        );
        assert_eq!(serde_json::to_string(&StatsChangedEvent {}).unwrap(), "{}");
        let job = JobUpdatedEvent {
            id: 7,
            kind: "archive.drain".into(),
            state: JobState::Running,
            progress: Some(0.5),
            stage: None,
            post_key: None,
            error_code: None,
        };
        assert_eq!(
            serde_json::to_value(&job).unwrap(),
            serde_json::json!({
                "id": 7, "kind": "archive.drain", "state": "running", "progress": 0.5,
                "stage": null, "postKey": null, "errorCode": null
            })
        );
        assert!(JobState::Cancelled.is_final() && !JobState::Running.is_final());
        let status = ServerEvent::ExtensionStatus(ExtensionStatusEvent {
            connected: true,
            version: Some("0.2.0".into()),
            last_seen_at: None,
        });
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "event": "extension.status",
                "data": { "connected": true, "version": "0.2.0", "lastSeenAt": null }
            })
        );
    }
}
