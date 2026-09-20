use crate::App;
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use demodex_protocol::{Api, Hello, Login, Notice, Operation, VERSION};
use futures_util::{SinkExt, StreamExt};
use ractor_wormhole::{
    conduit::{self, ConduitMessage},
    nexus::{Nexus, start_nexus},
    util::FnActor,
};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;

#[derive(Clone)]
struct Endpoint {
    app: App,
    origins: Arc<Vec<String>>,
}

pub fn router(app: App, origins: Vec<String>) -> Router {
    Router::new()
        .route("/wormhole", get(upgrade))
        .with_state(Endpoint {
            app,
            origins: Arc::new(origins),
        })
}

fn origin_allowed(headers: &HeaderMap, origins: &[String]) -> bool {
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if origins.iter().any(|allowed| allowed == origin) {
        return true;
    }
    let Some(host) = headers.get("host").and_then(|h| h.to_str().ok()) else {
        return false;
    };
    origin == format!("https://{host}") || origin == format!("http://{host}")
}

async fn upgrade(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !origin_allowed(&headers, &endpoint.origins) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(8 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            if let Err(error) = connection(socket, endpoint.app).await {
                tracing::debug!("browser connection ended: {error:#}");
            }
        })
}

// Drop only connection-scoped actors; the session manager and Codex RPCs are not
// supervised by browser portals. In-flight commands finish even after a tab dies.
struct Actors(Vec<ractor::ActorCell>);
impl Drop for Actors {
    fn drop(&mut self) {
        for actor in &self.0 {
            actor.stop(None);
        }
    }
}

async fn connection(mut socket: WebSocket, app: App) -> Result<()> {
    // Negotiate in stable JSON before creating any actors or decoding the
    // transport's binary messages. No authentication token is needed yet.
    socket
        .send(Message::Text(
            serde_json::to_string(&Hello::default())?.into(),
        ))
        .await?;
    let first = tokio::time::timeout(Duration::from_secs(10), socket.recv())
        .await?
        .context("connection closed before protocol negotiation")??;
    let Message::Text(hello) = first else {
        anyhow::bail!("expected Demodex protocol hello")
    };
    ensure!(hello.len() <= 4096, "protocol hello too large");
    serde_json::from_str::<Hello>(&hello)?
        .check()
        .map_err(anyhow::Error::msg)?;
    let (sink, source) = socket.split();
    let sink = sink
        .with(|message: ConduitMessage| async move {
            Ok::<_, anyhow::Error>(match message {
                ConduitMessage::Handshake(text) => Message::Text(text.into()),
                ConduitMessage::Content(bytes) => Message::Binary(bytes.into()),
                ConduitMessage::Close(_) => Message::Close(None),
            })
        })
        .sink_map_err(anyhow::Error::from);
    let source = source.filter_map(|message| async move {
        match message {
            Ok(Message::Text(text)) => Some(Ok(ConduitMessage::Handshake(text.to_string()))),
            Ok(Message::Binary(bytes)) => Some(Ok(ConduitMessage::Content(bytes.to_vec()))),
            Ok(Message::Close(_)) => Some(Ok(ConduitMessage::Close(None))),
            Ok(_) => None,
            Err(error) => Some(Err(error.into())),
        }
    });
    let nexus = start_nexus(None, None)
        .await
        .map_err(anyhow::Error::from_boxed)?;
    let mut actors = Actors(vec![nexus.get_cell()]);
    let authenticated = Arc::new(AtomicBool::new(false));
    let authenticated_copy = authenticated.clone();
    let api_authenticated = authenticated.clone();
    let (notify_tx, mut notify_rx) = tokio::sync::watch::channel(None::<ractor::ActorRef<Notice>>);
    let call_app = app.clone();
    let permits = Arc::new(tokio::sync::Semaphore::new(16));
    let (api, _) = FnActor::<Api>::start_fn(async move |mut ctx| {
        while let Some(message) = ctx.rx.recv().await {
            match message {
                Api::Call {
                    request_id,
                    operation,
                    reply,
                } => {
                    if !api_authenticated.load(Ordering::Acquire) {
                        let _ = reply.send(Err("Authentication required".into()));
                        continue;
                    }
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        let _ = reply.send(Err("Too many pending requests".into()));
                        continue;
                    };
                    let app = call_app.clone();
                    // Detached from the portal deliberately: accepted mutations must
                    // complete and record their outcome even if the browser leaves.
                    tokio::spawn(async move {
                        let _permit = permit;
                        let result = execute(&app, &request_id, operation)
                            .await
                            .map_err(|e| format!("{e:#}"));
                        let _ = reply.send(result);
                    });
                }
                Api::Watch { sink, reply } => {
                    if api_authenticated.load(Ordering::Acquire) {
                        let _ = notify_tx.send(Some(sink));
                        let _ = reply.send(());
                    }
                }
            }
        }
    })
    .await?;
    actors.0.push(api.get_cell());
    let expected_token = app.token.clone();
    let (login, _) = FnActor::<Login>::start_fn(async move |mut ctx| {
        while let Some(Login::Authenticate { version, token, reply }) = ctx.rx.recv().await {
            let result = if version != VERSION {
                Err(format!("Protocol mismatch: daemon {VERSION}, browser {version}. Update the browser or daemon."))
            } else if token != *expected_token {
                Err("Access token rejected".into())
            } else {
                authenticated_copy.store(true, Ordering::Release);
                Ok(api.clone())
            };
            let _ = reply.send(result);
        }
    }).await?;
    actors.0.push(login.get_cell());
    nexus.publish_named_actor("login".into(), login).await?;
    let portal = conduit::from_sink_source(
        nexus,
        "demodex-browser".into(),
        Box::pin(sink),
        Box::pin(source),
    )
    .await?;
    actors.0.push(portal.get_cell());
    let mut changes = app.manager.updates.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    let authentication_deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(authentication_deadline);
    loop {
        tokio::select! {
            _ = portal.wait(None) => break,
            _ = &mut authentication_deadline, if !authenticated.load(Ordering::Acquire) => break,
            result = notify_rx.changed() => { if result.is_err() { break } },
            _ = changes.recv() => {},
            _ = heartbeat.tick() => {},
        }
        if let Some(sink) = notify_rx.borrow().as_ref() {
            let _ = sink.send_message(Notice::Changed);
        }
    }
    Ok(())
}

pub(crate) async fn execute(app: &App, request_id: &str, operation: Operation) -> Result<String> {
    if let Operation::Receipt { id } = &operation {
        return Ok(match app.manager.store.receipt(id)? {
            None => json!({"state":"unknown"}),
            Some((_, None)) => json!({"state":"pending-or-interrupted","message":"Do not automatically retry this command."}),
            Some((_, Some(response))) => json!({"state":"completed","result":serde_json::from_str::<Result<String,String>>(&response)?}),
        }.to_string());
    }
    if !operation.is_mutation() {
        return dispatch(app, operation).await;
    }
    ensure!(
        uuid::Uuid::parse_str(request_id).is_ok(),
        "a UUID request ID is required for commands"
    );
    let encoded = serde_json::to_string(&operation)?;
    let _lock = app.commands.lock().await;
    if let Some((previous, response)) = app.manager.store.receipt(request_id)? {
        ensure!(
            previous == encoded,
            "request ID already belongs to a different command"
        );
        let response = response
            .context("command outcome is uncertain after an interruption; refusing to replay it")?;
        return serde_json::from_str::<Result<String, String>>(&response)?
            .map_err(anyhow::Error::msg);
    }
    app.manager.store.begin_command(request_id, &encoded)?;
    drop(_lock);
    let result = dispatch(app, operation).await.map_err(|e| format!("{e:#}"));
    app.manager
        .store
        .finish_command(request_id, &serde_json::to_string(&result)?)?;
    app.manager.changed();
    result.map_err(anyhow::Error::msg)
}

fn id(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value.len() <= 160
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid identifier"
    );
    Ok(value)
}

async fn dispatch(app: &App, operation: Operation) -> Result<String> {
    use Operation::*;
    if let SavedThreads { cursor, search } = operation {
        return Ok(app
            .orchestrator
            .saved_threads(cursor, search)
            .await?
            .to_string());
    }
    let (path, body) = match operation {
        Sessions => ("/sessions".into(), None),
        Detail { id: value } => (format!("/sessions/{}", id(&value)?), None),
        Events { id: value, after } => (
            format!("/sessions/{}/events?after={after}", id(&value)?),
            None,
        ),
        Runtime => ("/runtime".into(), None),
        StartRuntime => ("/runtime/start".into(), Some("{}".into())),
        Login => ("/runtime/login".into(), Some("{}".into())),
        HostSession { input } => ("/host/sessions".into(), Some(input)),
        ExternalSession { input } => ("/sessions".into(), Some(input)),
        Connect { id: value } => (
            format!("/sessions/{}/connect", id(&value)?),
            Some("{}".into()),
        ),
        Sandbox { id: value, input } => (format!("/sessions/{}/sandbox", id(&value)?), Some(input)),
        Prompt { id: value, text } => (
            format!("/sessions/{}/messages", id(&value)?),
            Some(json!({"text":text}).to_string()),
        ),
        Interrupt { id: value } => (
            format!("/sessions/{}/interrupt", id(&value)?),
            Some("{}".into()),
        ),
        Answer {
            id: value,
            key,
            result,
        } => (
            format!("/sessions/{}/answer", id(&value)?),
            Some(
                json!({"key":key,"result":serde_json::from_str::<serde_json::Value>(&result)?})
                    .to_string(),
            ),
        ),
        Environments => ("/environments".into(), None),
        CreateEnvironment { input } => ("/environments".into(), Some(input)),
        StartEnvironment { id: value } => (
            format!("/environments/{}/start", id(&value)?),
            Some("{}".into()),
        ),
        StopEnvironment { id: value } => (
            format!("/environments/{}/stop", id(&value)?),
            Some("{}".into()),
        ),
        EnvironmentSession { id: value, input } => (
            format!("/environments/{}/sessions", id(&value)?),
            Some(input),
        ),
        SavedThreads { .. } | Receipt { .. } => unreachable!(),
    };
    // Share the existing validation/business handlers. This is an in-process
    // router call, not a second HTTP connection and not a REST browser client.
    let request = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("Authorization", format!("Bearer {}", app.token))
        .header("Content-Type", "application/json")
        .body(Body::from(body.unwrap_or_default()))?;
    let response = crate::api(app.clone()).oneshot(request).await?;
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await?
            .to_vec(),
    )?;
    ensure!(status.is_success(), "{body}");
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Result<App> {
        let manager = crate::manager::Manager::new(crate::store::Store::open(
            std::path::Path::new(":memory:"),
        )?);
        Ok(App {
            orchestrator: crate::orchestrator::Orchestrator::new(
                manager.clone(),
                "/unused".into(),
                None,
                None,
                None,
            ),
            manager,
            token: Arc::new("fixture".into()),
            commands: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    #[tokio::test]
    async fn repeated_commands_do_not_create_another_session() -> Result<()> {
        let app = fixture()?;
        let id = uuid::Uuid::new_v4().to_string();
        let command = Operation::ExternalSession {
            input: json!({"name":"fixture","endpoint":"ws://127.0.0.1:1"}).to_string(),
        };
        let (first, second) = tokio::join!(
            execute(&app, &id, command.clone()),
            execute(&app, &id, command)
        );
        assert_eq!(first?, second?);
        assert_eq!(app.manager.store.list()?.len(), 1);
        assert!(
            execute(
                &app,
                &id,
                Operation::Connect {
                    id: "different".into()
                }
            )
            .await
            .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn interrupted_receipt_is_never_replayed() -> Result<()> {
        let app = fixture()?;
        let id = uuid::Uuid::new_v4().to_string();
        let command = Operation::ExternalSession {
            input: json!({"name":"fixture","endpoint":"ws://127.0.0.1:1"}).to_string(),
        };
        app.manager
            .store
            .begin_command(&id, &serde_json::to_string(&command)?)?;
        assert!(
            execute(&app, &id, command)
                .await
                .unwrap_err()
                .to_string()
                .contains("refusing to replay")
        );
        assert!(app.manager.store.list()?.is_empty());
        let receipt = execute(&app, "", Operation::Receipt { id }).await?;
        assert!(receipt.contains("pending-or-interrupted"));
        Ok(())
    }
    #[test]
    fn browser_origins_are_explicit() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "daemon.example".parse().unwrap());
        headers.insert("origin", "https://other.example".parse().unwrap());
        assert!(!origin_allowed(&headers, &[]));
        assert!(origin_allowed(&headers, &["https://other.example".into()]));
        headers.insert("origin", "https://daemon.example".parse().unwrap());
        assert!(origin_allowed(&headers, &[]));
    }
}
