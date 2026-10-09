use crate::error::AgentError;
use crate::ipc::tasks::{TaskEvent, TaskState};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::Stream;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast;

pub async fn list(State(state): State<AppState>) -> Json<Vec<TaskState>> {
    Json(state.tasks.list())
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TaskState>, AgentError> {
    state.tasks.get(&id).map(Json).ok_or(AgentError::NotFound)
}

pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AgentError> {
    // Live task: snapshot + follow the broadcast channel. Finished task: the
    // handle was evicted from the live map by finish(), so fall back to the
    // persisted DB row and replay it. The panel's EventSource always connects
    // late (it only mounts after the spawning action returns, which itself
    // waits on a revalidate), so a fast task is routinely already finished by
    // the time the stream request arrives - without this fallback that's a
    // 404 and the UI sits on "pending" forever.
    let (snapshot, rx) = match state.tasks.subscribe(&id) {
        Some((snapshot, rx)) => (snapshot, Some(rx)),
        None => (state.tasks.get(&id).ok_or(AgentError::NotFound)?, None),
    };
    let stream = make_stream(snapshot, rx);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn make_stream(
    snapshot: TaskState,
    rx: Option<broadcast::Receiver<TaskEvent>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        for line in &snapshot.log {
            yield Ok(Event::default().event("line").data(line.clone()));
        }

        let Some(mut rx) = rx else {
            // Replay of a task with no live producer: nothing more will ever
            // arrive, so emit the terminal event and end the stream. The DB
            // status is always terminal here - finish()/Drop persist the
            // final status before evicting from the live map, and interrupted
            // rows from a previous agent boot are swept to failed at startup.
            yield Ok(Event::default()
                .event("done")
                .data(serde_json::to_string(&snapshot.status).unwrap_or_default()));
            return;
        };

        yield Ok(Event::default()
            .event("status")
            .data(serde_json::to_string(&snapshot.status).unwrap_or_default()));

        loop {
            match rx.recv().await {
                Ok(TaskEvent::Line { text }) => {
                    yield Ok(Event::default().event("line").data(text));
                }
                Ok(TaskEvent::Status { status }) => {
                    yield Ok(Event::default()
                        .event("status")
                        .data(serde_json::to_string(&status).unwrap_or_default()));
                }
                Ok(TaskEvent::Done { status }) => {
                    yield Ok(Event::default()
                        .event("done")
                        .data(serde_json::to_string(&status).unwrap_or_default()));
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}
