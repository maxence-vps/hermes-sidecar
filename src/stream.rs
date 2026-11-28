//! Stream relay: starts agent turns in Hermes and relays their events, decoupled from the client.
//!
//! Hermes interrupts a turn as soon as the client of `POST /api/sessions/{id}/chat/stream`
//! disconnects, which happens whenever iOS backgrounds the app or switches networks. The sidecar
//! makes that request itself, from a task that outlives the client request: it reads every event
//! until `done`, keeps them in memory while the turn runs, and forwards them to the client for as
//! long as the client stays connected. See the "Chat streams" section of `README.md`.

use std::{
    collections::{HashMap, hash_map::Entry},
    convert::Infallible,
    pin::pin,
    sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::{Path, State, rejection::BytesRejection},
    http::{HeaderName, header},
    response::{IntoResponse, Response},
};
use futures_util::stream::{self, Stream, StreamExt};
use serde_json::Value;
use tokio::sync::{oneshot, watch};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    error::{AppError, ErrorBody},
    hermes::{HermesClient, HermesError},
    projects::validate_conversation_id,
    state::AppState,
};

/// Tag of the stream routes in the OpenAPI spec.
const TAG: &str = "streams";

/// Longest wait for the next bytes from Hermes before the sidecar gives up on a turn, so that a
/// stuck connection cannot keep a conversation busy forever. Generous: tool calls can be silent.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The stream routes, to merge into the protected router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(chat_stream))
}

/// Body of `POST /sessions/{id}/chat/stream`, the same as for Hermes. Only documents it: the
/// sidecar checks that `message` is there and forwards the whole object, other fields included.
#[derive(ToSchema)]
pub struct ChatStreamRequest {
    /// The user message.
    #[schema(example = "Summarise the open issues")]
    pub message: String,
}

/// Starts a turn in a Hermes conversation and streams its events. The turn survives the client.
///
/// The body is forwarded to Hermes `POST /api/sessions/{id}/chat/stream` as is, and the response
/// is its Server-Sent Events stream, byte for byte (`run.started`, `assistant.delta`, `tool.*`,
/// `assistant.completed`, `run.*`, `done`). If the client disconnects, the sidecar keeps reading
/// the turn until `done`, so Hermes runs it to the end and saves it in the session. If the Hermes
/// stream fails, it ends with an `error` event whose data is an error body (`hermes_unavailable`).
#[utoipa::path(
    post,
    path = "/sessions/{id}/chat/stream",
    tag = TAG,
    security(("bearer" = [])),
    params(
        ("id" = String, Path, description = "Hermes session id", example = "20260412_101500_ab12cd"),
    ),
    request_body = ChatStreamRequest,
    responses(
        (status = OK, description = "The events of the turn, as Server-Sent Events", body = String, content_type = "text/event-stream"),
        (status = BAD_REQUEST, description = "The body is not a JSON object with a `message` (`invalid_request`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = NOT_FOUND, description = "Hermes has no session with this id (`session_not_found`)", body = ErrorBody),
        (status = CONFLICT, description = "A turn is already running in this conversation (`turn_already_running`)", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid session id (`invalid_conversation_id`)", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn chat_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, AppError> {
    validate_session_id(&session_id)?;
    let request = parse_request(body)?;
    let handle = state.streams.begin(&session_id)?;
    let events = handle.turn.events();

    // The turn runs in its own task, so that it goes on if this request is dropped, even before
    // Hermes answers.
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(run_turn(state.hermes.clone(), handle, request, ready_tx));
    match ready_rx.await {
        Ok(Ok(())) => Ok(sse_response(events)),
        Ok(Err(error)) => Err(hermes_error(error)),
        Err(_) => Err(AppError::RelayFailed),
    }
}

/// A conversation id that is also safe as a URL path segment: `.` and `..` would be resolved as
/// dot segments, even percent-encoded.
fn validate_session_id(id: &str) -> Result<(), AppError> {
    validate_conversation_id(id)?;
    if matches!(id, "." | "..") {
        return Err(AppError::InvalidConversationId);
    }
    Ok(())
}

/// The JSON object to forward to Hermes. Only `message` is checked: Hermes validates the rest.
fn parse_request(body: Result<Bytes, BytesRejection>) -> Result<Value, AppError> {
    let body = body.map_err(|rejection| AppError::InvalidRequest(rejection.body_text()))?;
    let request: Value = serde_json::from_slice(&body)
        .map_err(|error| AppError::InvalidRequest(format!("the body is not JSON: {error}")))?;
    match request.get("message") {
        None | Some(Value::Null) => Err(AppError::InvalidRequest(
            "the body must be a JSON object with a `message`".to_owned(),
        )),
        Some(_) => Ok(request),
    }
}

/// Hermes answers 404 for an unknown session and 409 for a turn it already runs.
fn hermes_error(error: HermesError) -> AppError {
    match error {
        HermesError::Status(404) => AppError::SessionNotFound,
        HermesError::Status(409) => AppError::TurnAlreadyRunning,
        error => AppError::Hermes(error),
    }
}

fn sse_response(events: impl Stream<Item = Bytes> + Send + 'static) -> Response {
    let headers = [
        (header::CONTENT_TYPE, "text/event-stream"),
        (header::CACHE_CONTROL, "no-cache"),
        // Asks proxies such as nginx not to hold events back.
        (HeaderName::from_static("x-accel-buffering"), "no"),
    ];
    let body = Body::from_stream(events.map(Ok::<_, Infallible>));
    (headers, body).into_response()
}

/// Runs a turn to its end, whatever happens to the client: calls Hermes, tells the handler how it
/// answered, then reads the events into the log of the turn. Dropping `handle` frees the session.
async fn run_turn(
    hermes: HermesClient,
    handle: TurnHandle,
    request: Value,
    ready: oneshot::Sender<Result<(), HermesError>>,
) {
    let response = match hermes.chat_stream(&handle.session_id, &request).await {
        Ok(response) => response,
        Err(error) => {
            // Free the session first, so that a client retrying at once is not told a turn runs.
            drop(handle);
            let _ = ready.send(Err(error));
            return;
        }
    };
    // The client may be gone already: the turn goes on anyway.
    let _ = ready.send(Ok(()));
    read_events(&handle.turn, response).await;
    tracing::debug!(session_id = %handle.session_id, "turn finished");
}

/// Reads the SSE body of Hermes into `turn`, one event at a time, until `done`, the end of the
/// body, or a failure (logged as a last `error` event).
async fn read_events(turn: &Turn, response: reqwest::Response) {
    let mut body = pin!(response.bytes_stream());
    let mut framer = Framer::default();
    loop {
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, body.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                let error = HermesError::InvalidResponse(error);
                tracing::warn!(%error, "the Hermes stream failed");
                turn.push(error_event(&error.to_string()));
                return;
            }
            Ok(None) => break,
            Err(_) => {
                tracing::warn!("Hermes sent nothing for 30 minutes, giving up on the turn");
                turn.push(error_event("Hermes sent nothing for 30 minutes"));
                return;
            }
        };
        for event in framer.push(&chunk) {
            let done = event_name(&event).as_deref() == Some("done");
            turn.push(event);
            if done {
                return;
            }
        }
    }
    if let Some(event) = framer.finish() {
        turn.push(event);
    }
}

/// The `error` event the sidecar adds when the Hermes stream fails.
fn error_event(message: &str) -> Bytes {
    let data = serde_json::json!({ "error": { "code": "hermes_unavailable", "message": message } });
    Bytes::from(format!("event: error\ndata: {data}\n\n"))
}

/// Cuts an SSE body into events, each kept byte for byte with the blank line that ends it.
#[derive(Default)]
struct Framer {
    pending: Vec<u8>,
}

impl Framer {
    /// The events completed by `chunk`.
    fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = event_end(&self.pending) {
            let rest = self.pending.split_off(end);
            events.push(Bytes::from(std::mem::replace(&mut self.pending, rest)));
        }
        events
    }

    /// What is left when the body ends: a last event without its blank line, completed with one.
    fn finish(self) -> Option<Bytes> {
        if self.pending.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        let mut event = self.pending;
        event.extend_from_slice(b"\n\n");
        Some(Bytes::from(event))
    }
}

/// Index just past the first blank line (`\n\n` or `\r\n\r\n`) in `bytes`.
fn event_end(bytes: &[u8]) -> Option<usize> {
    (0..bytes.len()).find_map(|i| {
        let rest = &bytes[i..];
        if rest.starts_with(b"\r\n\r\n") {
            Some(i + 4)
        } else if rest.starts_with(b"\n\n") {
            Some(i + 2)
        } else {
            None
        }
    })
}

/// The type of an event: its `event:` field, or else the `type` of its JSON data.
fn event_name(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let mut data = Vec::new();
    for line in text.lines() {
        if let Some(name) = field(line, "event") {
            return Some(name.to_owned());
        }
        if let Some(value) = field(line, "data") {
            data.push(value);
        }
    }
    let data: Value = serde_json::from_str(&data.join("\n")).ok()?;
    data.get("type")?.as_str().map(str::to_owned)
}

/// The value of `line` if it is the field `name` (`name: value` or `name:value`).
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let value = line.strip_prefix(name)?.strip_prefix(':')?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

/// Turns running in Hermes through the sidecar, by session id. A turn is removed as soon as it
/// finishes: Hermes then has the whole exchange, so its events are not kept.
#[derive(Clone, Default)]
pub struct StreamRelay {
    turns: Arc<RwLock<HashMap<String, Arc<Turn>>>>,
}

impl StreamRelay {
    /// True while a turn started through the sidecar runs in this session.
    pub fn is_running(&self, session_id: &str) -> bool {
        self.read().contains_key(session_id)
    }

    /// The events of the turn running in this session, from its first one, until it finishes.
    pub fn subscribe(&self, session_id: &str) -> Option<impl Stream<Item = Bytes> + Send + use<>> {
        let turn = self.read().get(session_id).cloned()?;
        Some(turn.events())
    }

    /// Registers a new turn in this session, unless one is already running.
    fn begin(&self, session_id: &str) -> Result<TurnHandle, AppError> {
        let turn = Arc::new(Turn::new());
        match self.write().entry(session_id.to_owned()) {
            Entry::Occupied(_) => return Err(AppError::TurnAlreadyRunning),
            Entry::Vacant(entry) => {
                entry.insert(Arc::clone(&turn));
            }
        }
        Ok(TurnHandle {
            relay: self.clone(),
            session_id: session_id.to_owned(),
            turn,
        })
    }

    // The map stays consistent even if a holder panicked: every change is a single call.
    fn read(&self) -> RwLockReadGuard<'_, HashMap<String, Arc<Turn>>> {
        self.turns.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<String, Arc<Turn>>> {
        self.turns.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Owns a registered turn: dropping it finishes the turn and frees the session, also when the
/// relay task panics.
struct TurnHandle {
    relay: StreamRelay,
    session_id: String,
    turn: Arc<Turn>,
}

impl Drop for TurnHandle {
    fn drop(&mut self) {
        self.turn.finish();
        let mut turns = self.relay.write();
        if turns
            .get(&self.session_id)
            .is_some_and(|turn| Arc::ptr_eq(turn, &self.turn))
        {
            turns.remove(&self.session_id);
        }
    }
}

/// One turn: the events Hermes sent so far, shared with every client that follows it.
struct Turn {
    log: watch::Sender<Log>,
}

#[derive(Default)]
struct Log {
    /// Each SSE event as Hermes framed it, in order.
    events: Vec<Bytes>,
    /// No event will be added.
    finished: bool,
}

impl Turn {
    fn new() -> Self {
        Self {
            log: watch::Sender::new(Log::default()),
        }
    }

    fn push(&self, event: Bytes) {
        self.log.send_modify(|log| log.events.push(event));
    }

    fn finish(&self) {
        self.log.send_modify(|log| log.finished = true);
    }

    /// Every event from the first one, then each new one as it comes, until the turn finishes.
    fn events(&self) -> impl Stream<Item = Bytes> + Send + use<> {
        replay(self.log.subscribe())
    }
}

fn replay(receiver: watch::Receiver<Log>) -> impl Stream<Item = Bytes> + Send + 'static {
    stream::unfold(
        (receiver, 0, false),
        |(mut receiver, next, mut closed)| async move {
            loop {
                // The borrow must end before any await: it holds a lock.
                let (event, finished) = {
                    let log = receiver.borrow_and_update();
                    (log.events.get(next).cloned(), log.finished)
                };
                if let Some(event) = event {
                    return Some((event, (receiver, next + 1, closed)));
                }
                if finished || closed {
                    return None;
                }
                // An error means the turn is gone: what it left was read above.
                closed = receiver.changed().await.is_err();
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(events: &[Bytes]) -> Vec<&str> {
        events
            .iter()
            .map(|event| std::str::from_utf8(event).unwrap())
            .collect()
    }

    #[test]
    fn frames_events_across_chunks() {
        let mut framer = Framer::default();
        assert!(framer.push(b"event: run.started\ndata: {}\n").is_empty());
        let events =
            framer.push(b"\nevent: assistant.delta\ndata: {\"delta\":\"Hi\"}\n\nevent: do");
        assert_eq!(
            text(&events),
            [
                "event: run.started\ndata: {}\n\n",
                "event: assistant.delta\ndata: {\"delta\":\"Hi\"}\n\n",
            ]
        );
        let events = framer.push(b"ne\ndata: {}\n\n");
        assert_eq!(text(&events), ["event: done\ndata: {}\n\n"]);
        assert!(framer.finish().is_none());
    }

    #[test]
    fn frames_crlf_events() {
        let mut framer = Framer::default();
        let events = framer.push(b"event: run.started\r\ndata: {}\r\n\r\n: ping\r\n\r");
        assert_eq!(text(&events), ["event: run.started\r\ndata: {}\r\n\r\n"]);
        let events = framer.push(b"\n");
        assert_eq!(text(&events), [": ping\r\n\r\n"]);
    }

    #[test]
    fn completes_a_last_event_without_its_blank_line() {
        let mut framer = Framer::default();
        assert!(framer.push(b"event: done\ndata: {}").is_empty());
        assert_eq!(framer.finish().unwrap(), "event: done\ndata: {}\n\n");

        let mut framer = Framer::default();
        assert!(framer.push(b"\n").is_empty());
        assert!(framer.finish().is_none());
    }

    #[test]
    fn names_events() {
        assert_eq!(event_name(b"event: done\ndata: {}\n\n").unwrap(), "done");
        assert_eq!(event_name(b"event:done\n\n").unwrap(), "done");
        assert_eq!(
            event_name(b"data: {\"type\": \"done\"}\r\n\r\n").unwrap(),
            "done"
        );
        assert_eq!(event_name(b"data: {\"delta\": \"x\"}\n\n"), None);
        assert_eq!(event_name(b": ping\n\n"), None);
        // A field must be the whole name before the colon.
        assert_eq!(event_name(b"events: done\n\n"), None);
    }

    #[test]
    fn rejects_dot_segments_as_session_ids() {
        assert!(validate_session_id("20260412_101500_ab12cd").is_ok());
        assert!(validate_session_id("...").is_ok());
        for id in [".", "..", "", "a/b", "a b"] {
            assert!(validate_session_id(id).is_err(), "{id:?}");
        }
    }

    #[test]
    fn requires_a_message() {
        let parse = |body: &str| parse_request(Ok(Bytes::from(body.to_owned())));
        assert!(parse(r#"{"message": "Hi", "model": "x"}"#).is_ok());
        assert!(parse(r#"{"message": [{"type": "text", "text": "Hi"}]}"#).is_ok());
        for body in ["", "not json", "[]", "\"Hi\"", "{}", r#"{"message": null}"#] {
            assert!(
                matches!(parse(body), Err(AppError::InvalidRequest(_))),
                "{body:?}"
            );
        }
    }

    #[test]
    fn allows_one_turn_per_session() {
        let relay = StreamRelay::default();
        let handle = relay.begin("a").unwrap();
        assert!(relay.is_running("a"));
        assert!(matches!(
            relay.begin("a"),
            Err(AppError::TurnAlreadyRunning)
        ));
        let other = relay.begin("b").unwrap();

        drop(handle);
        assert!(!relay.is_running("a"));
        assert!(relay.is_running("b"));
        assert!(relay.begin("a").is_ok());
        drop(other);
    }

    #[tokio::test]
    async fn replays_a_turn_to_late_and_early_subscribers() {
        let relay = StreamRelay::default();
        let handle = relay.begin("a").unwrap();
        let early = relay.subscribe("a").unwrap();
        handle.turn.push(Bytes::from_static(b"one\n\n"));
        let late = relay.subscribe("a").unwrap();
        handle.turn.push(Bytes::from_static(b"two\n\n"));
        drop(handle);
        assert!(relay.subscribe("a").is_none());

        for events in [early.collect::<Vec<_>>().await, late.collect().await] {
            assert_eq!(text(&events), ["one\n\n", "two\n\n"]);
        }
    }
}
