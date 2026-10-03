//! Ephemeral conversational search. One run per account, no persisted text or replay.
use super::{CallHints, Caller, Task};
use crate::error::{ApiError, ErrorCode};
use crate::ids::new_ulid;
use crate::state::{AppState, blocking};
use axum::response::sse::Event;
use serde::Serialize;
use shelfy_ai::{ChatRequest, Message, TextCallback};
use shelfy_core::ai::{chat as core, chat_prompt, prompts};
use shelfy_core::repo::posts::SourceBucket;
use shelfy_core::search::vocab::{Pools, VocabCache};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

pub const HEARTBEAT: Duration = Duration::from_secs(15);
const FIRST_TOKEN: Duration = Duration::from_secs(20);
const TOTAL: Duration = Duration::from_secs(60);
const TOKEN_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct ChatRuns {
    runs: Mutex<HashMap<String, (String, CancellationToken)>>,
    vocab: VocabCache,
}
impl ChatRuns {
    fn runs(&self) -> MutexGuard<'_, HashMap<String, (String, CancellationToken)>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn start(self: &Arc<Self>, user: &str) -> RunGuard {
        let id = new_ulid();
        let cancel = CancellationToken::new();
        if let Some((_, old)) = self
            .runs()
            .insert(user.to_owned(), (id.clone(), cancel.clone()))
        {
            old.cancel();
        }
        RunGuard {
            runs: self.clone(),
            user: user.to_owned(),
            id,
            cancel,
        }
    }
    pub fn cancel(&self, user: &str, id: &str) -> bool {
        let mut runs = self.runs();
        if runs.get(user).is_some_and(|(found, _)| found == id) {
            let (_, token) = runs.remove(user).expect("matched run");
            token.cancel();
            true
        } else {
            false
        }
    }
}
/// The response owns this guard. Dropping its body cancels the provider and
/// removes only this run, so closing an old stream cannot cancel its successor.
pub struct RunGuard {
    runs: Arc<ChatRuns>,
    user: String,
    pub id: String,
    pub cancel: CancellationToken,
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.runs.cancel(&self.user, &self.id);
    }
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Tags {
    pub general: Vec<String>,
    pub specific: Vec<String>,
}
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_code: Option<String>,
    pub tags: Tags,
    pub keywords: Vec<String>,
    pub remove: Vec<String>,
    pub model_used: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub run_id: String,
}
#[derive(Serialize, ToSchema)]
pub struct TokenEvent {
    pub text: String,
}
#[derive(Serialize, ToSchema)]
pub struct ErrorEvent {
    pub code: String,
}

pub struct Prepared {
    history: Vec<core::Turn>,
    active: Vec<String>,
    pools: Pools,
    fallback: ResultEvent,
    system: String,
}
pub async fn prepare(
    state: &AppState,
    runs: Arc<ChatRuns>,
    user: &str,
    history: Vec<core::Turn>,
    active: Vec<String>,
    source: Option<SourceBucket>,
) -> Result<Prepared, ApiError> {
    let db = state.user_db(user).await?;
    let user = user.to_owned();
    blocking(move || {
        let vocab = runs.vocab.get_for_source(&user, &db, source)?;
        db.read(|conn| {
            let active = vocab.intersect(conn, &active)?;
            let message = history
                .iter()
                .rev()
                .find(|t| t.role == "user")
                .map_or("", |t| t.content.as_str());
            let pools = vocab.pools(conn, message, &active)?;
            let system = chat_prompt::system(&pools.broad, &pools.specific, &active)
                .map_err(|_| ApiError::new(ErrorCode::Internal))?;
            let fallback = core::fallback(conn, &vocab, message, &active)?;
            let fallback = ResultEvent {
                reply_code: Some(
                    match fallback.reply_code {
                        core::ReplyCode::Suggestions => "suggestions",
                        core::ReplyCode::NoMatches => "no_matches",
                    }
                    .into(),
                ),
                tags: Tags {
                    general: fallback.tags.broad,
                    specific: fallback.tags.specific,
                },
                keywords: fallback.keywords,
                remove: Vec::new(),
                model_used: false,
            };
            Ok::<_, ApiError>(Prepared {
                history,
                active,
                pools,
                fallback,
                system,
            })
        })
    })
    .await
}
fn event<T: Serialize>(name: &str, value: &T) -> Event {
    Event::default()
        .event(name)
        .json_data(value)
        .expect("event serialization")
}
fn parsed(prepared: &Prepared, text: &str) -> ResultEvent {
    let broad = prepared.pools.broad.iter().cloned().collect();
    let specific = prepared.pools.specific.iter().cloned().collect();
    let active: HashSet<String> = prepared.active.iter().map(|t| t.to_lowercase()).collect();
    let tags = core::parse_tags(text, &broad, &specific, &active);
    let remove = core::parse_tag_block(text, "[[REMOVE]]", "[[/REMOVE]]", &active)
        .into_iter()
        .take(core::PER_TIER_CAP * 2)
        .collect();
    let mut keywords = core::parse_keyword_block(text, "[[KEYWORDS]]", "[[/KEYWORDS]]");
    if keywords.is_empty() {
        keywords = prepared.fallback.keywords.clone();
    }
    ResultEvent {
        reply_code: None,
        tags: Tags {
            general: tags.broad,
            specific: tags.specific,
        },
        keywords,
        remove,
        model_used: true,
    }
}
/// Sends only prose before the first sentinel; hold two characters so a marker
/// split across chunks never leaks. Callback values are cumulative, not deltas.
fn visible(text: &str, finished: bool) -> &str {
    if let Some(end) = text.find("[[") {
        return &text[..end];
    }
    if finished {
        return text;
    }
    let end = text.char_indices().rev().nth(1).map_or(0, |(i, _)| i);
    &text[..end]
}
async fn send_token(tx: &mpsc::Sender<Event>, text: &str, emitted: &mut String) -> bool {
    if let Some(tail) = text.strip_prefix(emitted.as_str()) {
        if tail.is_empty() {
            return true;
        }
        match tx.try_send(event(
            "token",
            &TokenEvent {
                text: tail.to_owned(),
            },
        )) {
            Ok(()) => *emitted = text.to_owned(),
            Err(mpsc::error::TrySendError::Full(_)) => {} // coalesce at the next tick
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
    true
}

async fn terminal(tx: &mpsc::Sender<Event>, frame: Event, cancel: &CancellationToken) -> bool {
    tokio::select! {result=tx.send(frame)=>result.is_ok(),()=cancel.cancelled()=>false}
}
pub async fn run(
    state: AppState,
    user: String,
    id: String,
    cancel: CancellationToken,
    provider_id: Option<String>,
    prepared: Prepared,
    tx: mpsc::Sender<Event>,
) {
    if tx
        .send(event("run", &RunEvent { run_id: id }))
        .await
        .is_err()
    {
        return;
    }
    if cancel.is_cancelled() {
        let _ = tx.try_send(event(
            "error",
            &ErrorEvent {
                code: "cancelled".into(),
            },
        ));
        return;
    }
    let owner = match crate::jobs::ai_drain::is_owner(&state, &user).await {
        Ok(v) => v,
        Err(_) => {
            let _ = tx
                .send(event(
                    "error",
                    &ErrorEvent {
                        code: "unauthorized".into(),
                    },
                ))
                .await;
            return;
        }
    };
    let caller = Caller::new(&user, owner);
    let route = match state
        .ai()
        .route_for_provider(&state, caller, Task::Chat, provider_id.as_deref())
        .await
    {
        Ok(r) => r,
        Err(_) => {
            let _ = terminal(&tx, event("result", &prepared.fallback), &cancel).await;
            return;
        }
    };
    let messages = prepared
        .history
        .iter()
        .map(|t| {
            if t.role == "user" {
                Message::user_text(t.content.clone())
            } else {
                Message::assistant_text(t.content.clone())
            }
        })
        .collect();
    let request = ChatRequest::new(route.model, messages)
        .with_system(prepared.system.clone())
        .with_temperature(prompts::spec(prompts::Task::Chat).temperature)
        .with_max_tokens(prompts::max_tokens(prompts::Task::Chat, 0))
        .streamed(true);
    let call_cancel = cancel.child_token();
    let cumulative = Arc::new(Mutex::new(String::new()));
    let seen = cumulative.clone();
    let callback: TextCallback = Arc::new(move |text| {
        let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.clear();
        seen.push_str(text);
    });
    let mut hints = CallHints::new()
        .with_cancel(call_cancel.clone())
        .with_text_callback(callback);
    hints.provider_id = provider_id;
    let call = state.ai().chat(&state, caller, Task::Chat, &request, hints);
    tokio::pin!(call);
    let first = tokio::time::sleep(FIRST_TOKEN);
    tokio::pin!(first);
    let total = tokio::time::sleep(TOTAL);
    tokio::pin!(total);
    let mut ticks = tokio::time::interval_at(Instant::now() + TOKEN_INTERVAL, TOKEN_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut emitted = String::new();
    let mut first_seen = false;
    loop {
        tokio::select! {
            biased;
            ()=cancel.cancelled()=> {let _=tx.try_send(event("error",&ErrorEvent {code:"cancelled".into()}));return;}
            ()=state.shutdown_token().cancelled()=> {cancel.cancel();return;}
            ()=&mut total=> {call_cancel.cancel();let _=terminal(&tx,event("result",&prepared.fallback),&cancel).await;return;}
            ()=&mut first,if !first_seen=> {if cumulative.lock().unwrap_or_else(PoisonError::into_inner).is_empty() {call_cancel.cancel();let _=terminal(&tx,event("result",&prepared.fallback),&cancel).await;return;} first_seen=true;}
            _=ticks.tick()=> {
                let text=cumulative.lock().unwrap_or_else(PoisonError::into_inner).clone();first_seen|=!text.is_empty();
                if !send_token(&tx,visible(&text,false),&mut emitted).await {cancel.cancel();return;}
            }
            result=&mut call=> {
                match result {
                    Ok(answer)=> {
                        let text=visible(&answer.text,true);
                        if let Some(tail)=text.strip_prefix(&emitted) && !tail.is_empty() {
                            tokio::select! {_=ticks.tick()=>{},()=cancel.cancelled()=>return};
                            if !terminal(&tx,event("token",&TokenEvent {text:tail.into()}),&cancel).await {return;}
                        }
                        let _=terminal(&tx,event("result",&parsed(&prepared,&answer.text)),&cancel).await;
                    }
                    Err(_)=> {let _=terminal(&tx,event("result",&prepared.fallback),&cancel).await;}
                }
                return;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sentinel_prefixes_never_escape() {
        assert_eq!(visible("reply[", false), "repl");
        assert_eq!(visible("reply[[GEN", false), "reply");
        assert_eq!(visible("café🎧", false), "caf");
        assert_eq!(visible("plain", true), "plain");
    }
    #[test]
    fn old_stream_cannot_cancel_its_successor_or_another_user() {
        let runs = Arc::new(ChatRuns::default());
        let old = runs.start("a");
        let next = runs.start("a");
        assert!(old.cancel.is_cancelled());
        drop(old);
        assert!(!next.cancel.is_cancelled());
        assert!(!runs.cancel("b", &next.id));
        assert!(runs.cancel("a", &next.id));
        assert!(next.cancel.is_cancelled());
    }
}
