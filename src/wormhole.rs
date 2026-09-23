use crate::App;
use anyhow::{Context, Result, ensure};
use demodex_protocol::{Api, Hello, Login, Notice, VERSION};
use futures_util::{SinkExt, StreamExt};
use http::{HeaderMap, StatusCode};
use ractor_wormhole::{
    conduit::{self, ConduitMessage},
    nexus::{Nexus, start_nexus},
    util::FnActor,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

#[derive(Clone)]
pub(crate) struct Endpoint {
    app: App,
    token: Arc<String>,
    origins: Arc<Vec<String>>,
    tailscale_users: Arc<Vec<String>>,
}
impl Endpoint {
    pub(crate) fn new(app: App, token: String, origins: Vec<String>, users: Vec<String>) -> Self {
        Self {
            app,
            token: Arc::new(token),
            origins: Arc::new(origins),
            tailscale_users: Arc::new(users),
        }
    }
    pub(crate) fn matches(&self, path: &str) -> bool {
        path == "/wormhole" || (path == "/" && !self.tailscale_users.is_empty())
    }
    pub(crate) fn authorize(&self, headers: &HeaderMap) -> Result<bool, StatusCode> {
        if !origin_allowed(headers, &self.origins) {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(tailscale_authenticated(
            headers,
            &self.tailscale_users,
            &self.origins,
        ))
    }
    pub(crate) async fn connect<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        &self,
        socket: WebSocketStream<S>,
        identity: bool,
    ) -> Result<()> {
        connection(socket, self.app.clone(), self.token.clone(), identity).await
    }
}

fn tailscale_authenticated(headers: &HeaderMap, users: &[String], origins: &[String]) -> bool {
    let mut identities = headers.get_all("tailscale-user-login").iter();
    let Some(login) = identities.next().and_then(|v| v.to_str().ok()) else {
        return false;
    };
    identities.next().is_none()
        && users.iter().any(|allowed| allowed == login)
        && headers
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|origin| origins.iter().any(|allowed| allowed == origin))
}

fn login_allowed(token: &str, expected: &str, identity: bool) -> bool {
    token == expected || (token.is_empty() && identity)
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

// Drop only connection-scoped actors; the session manager and Codex RPCs are not
// supervised by browser portals. In-flight commands finish even after a tab dies.
struct Actors(
    Vec<ractor::ActorCell>,
    Option<tokio::sync::oneshot::Sender<()>>,
);
impl Drop for Actors {
    fn drop(&mut self) {
        self.1.take();
        for actor in &self.0 {
            actor.stop(None);
        }
    }
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut socket: WebSocketStream<S>,
    app: App,
    token: Arc<String>,
    identity: bool,
) -> Result<()> {
    // Negotiate in stable JSON before creating any actors or decoding the
    // transport's binary messages. No authentication token is needed yet.
    socket
        .send(Message::Text(
            serde_json::to_string(&Hello::default())?.into(),
        ))
        .await?;
    let first = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await?
        .context("connection closed before protocol negotiation")??;
    let Message::Text(hello) = first else {
        anyhow::bail!("expected Demodex protocol hello")
    };
    ensure!(hello.len() <= 4096, "protocol hello too large");
    serde_json::from_str::<Hello>(&hello)?
        .check()
        .map_err(anyhow::Error::msg)?;
    let (stop_read, stopped_read) = tokio::sync::oneshot::channel::<()>();
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
    let source = source
        .take_until(async {
            let _ = stopped_read.await;
        })
        .filter_map(|message| async move {
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
    let mut actors = Actors(vec![nexus.get_cell()], Some(stop_read));
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
                        let result = app
                            .call(&request_id, operation)
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
    let expected_token = token;
    let (login, _) = FnActor::<Login>::start_fn(async move |mut ctx| {
        while let Some(Login::Authenticate { version, token, reply }) = ctx.rx.recv().await {
            let result = if version != VERSION {
                Err(format!("Protocol mismatch: daemon {VERSION}, browser {version}. Update the browser or daemon."))
            } else if !login_allowed(&token, &expected_token, identity) {
                Err(if token.is_empty() { "Access token required: this connection has no allowed Tailscale identity".into() } else { "Access token rejected".into() })
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

#[cfg(test)]
mod tests {
    use super::*;
    use demodex_protocol::Operation;
    use serde_json::json;
    async fn execute(app: &App, id: &str, operation: Operation) -> Result<String> {
        Ok(crate::service::execute(app, id, operation)
            .await?
            .into_value()
            .to_string())
    }
    async fn dispatch(app: &App, operation: Operation) -> Result<String> {
        Ok(crate::service::dispatch(app, operation)
            .await?
            .into_value()
            .to_string())
    }
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
            _lock: None,
            commands: Arc::new(tokio::sync::Mutex::new(())),
            lifecycle: Arc::new(crate::Lifecycle::default()),
        })
    }
    #[tokio::test]
    async fn imported_history_larger_than_eight_mib_is_not_truncated() -> Result<()> {
        let app = fixture()?;
        let session = app
            .manager
            .store
            .create("large history", "ws://127.0.0.1:1", &[], None)?;
        let message = json!({"method":"demodex/threadSnapshot", "params":{
            "thread":{"turns":[{"id":"old-turn", "items":[{
                "type":"agentMessage", "text":"x".repeat(16 * 1024 * 1024)
            }]}]}
        }});
        let seq = app.manager.store.event(&session.id, &message)?;
        let response = dispatch(
            &app,
            Operation::Events {
                id: session.id.clone(),
                after: 0,
            },
        )
        .await?;
        let events: serde_json::Value = serde_json::from_str(&response)?;
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["seq"], seq);
        assert_eq!(events[0]["message"], message);
        let next = dispatch(
            &app,
            Operation::Events {
                id: session.id,
                after: seq,
            },
        )
        .await?;
        assert_eq!(next, "[]");
        Ok(())
    }

    #[test]
    fn token_endpoint_preserves_embedded_frontend_root() -> Result<()> {
        let endpoint = Endpoint::new(fixture()?, "secret".into(), vec![], vec![]);
        assert!(!endpoint.matches("/"));
        assert!(endpoint.matches("/wormhole"));
        assert!(!endpoint.matches("/api/sessions"));
        Ok(())
    }

    #[tokio::test]
    async fn archive_receipt_does_not_rearchive_a_restored_session() -> Result<()> {
        let app = fixture()?;
        let session = app
            .manager
            .store
            .create("archive", "ws://127.0.0.1:1", &[], None)?;
        let request = uuid::Uuid::new_v4().to_string();
        let command = Operation::Archive {
            id: session.id.clone(),
            archived: true,
        };
        execute(&app, &request, command.clone()).await?;
        assert!(app.manager.store.get(&session.id)?.archived);
        execute(
            &app,
            &uuid::Uuid::new_v4().to_string(),
            Operation::Archive {
                id: session.id.clone(),
                archived: false,
            },
        )
        .await?;
        execute(&app, &request, command).await?;
        assert!(!app.manager.store.get(&session.id)?.archived);
        Ok(())
    }

    #[tokio::test]
    async fn repeated_commands_do_not_create_another_session() -> Result<()> {
        let app = fixture()?;
        let id = uuid::Uuid::new_v4().to_string();
        let command = Operation::ExternalSession {
            input: serde_json::from_value(json!({"name":"fixture","endpoint":"ws://127.0.0.1:1"}))?,
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
            input: serde_json::from_value(json!({"name":"fixture","endpoint":"ws://127.0.0.1:1"}))?,
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
    #[tokio::test]
    async fn image_receipts_deduplicate_without_storing_image_contents() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = fixture()?;
        app.orchestrator = crate::orchestrator::Orchestrator::new(
            app.manager.clone(),
            directory.path().into(),
            None,
            Some(directory.path().into()),
            None,
        );
        let session = app
            .manager
            .store
            .create("upload", "ws://127.0.0.1:1", &[], None)?;
        let bytes = b"\x89PNG\r\n\x1a\nfixture".to_vec();
        let command = Operation::UploadImage {
            id: session.id.clone(),
            bytes: bytes.clone(),
        };
        assert!(
            execute(&app, &uuid::Uuid::new_v4().to_string(), command.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("Select an execution target")
        );
        app.manager.store.bind_host(&session.id)?;
        app.manager.store.save_target_selection(
            &session.id,
            &[crate::targets::Selection {
                id: "host".into(),
                cwd: directory.path().to_string_lossy().into_owned(),
            }],
            &[],
        )?;
        let request = uuid::Uuid::new_v4().to_string();
        let first = execute(&app, &request, command.clone()).await?;
        assert_eq!(execute(&app, &request, command).await?, first);
        let result: serde_json::Value = serde_json::from_str(&first)?;
        assert_eq!(std::fs::read(result["path"].as_str().unwrap())?, bytes);
        assert_eq!(
            std::fs::read_dir(directory.path().join("uploads"))?.count(),
            1
        );
        assert!(
            app.manager
                .store
                .receipt(&request)?
                .unwrap()
                .0
                .contains("sha256")
        );
        let different = Operation::UploadImage {
            id: session.id.clone(),
            bytes: b"GIF89aother".to_vec(),
        };
        assert!(execute(&app, &request, different).await.is_err());
        let invalid = Operation::UploadImage {
            id: session.id,
            bytes: b"not an image".to_vec(),
        };
        assert!(
            execute(&app, &uuid::Uuid::new_v4().to_string(), invalid)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(directory.path().join("uploads"))?.count(),
            1
        );
        Ok(())
    }
    #[test]
    fn identity_requires_private_endpoint_exact_user_and_explicit_origin() {
        let users = vec!["owner@example.com".into()];
        let origins = vec!["https://app.example".into()];
        let mut headers = HeaderMap::new();
        headers.insert("tailscale-user-login", "owner@example.com".parse().unwrap());
        assert!(!tailscale_authenticated(&headers, &users, &origins));
        headers.insert("origin", "https://app.example".parse().unwrap());
        assert!(tailscale_authenticated(&headers, &users, &origins));
        // TCP's router has no trusted identity allowlist.
        assert!(!tailscale_authenticated(&headers, &[], &origins));
        headers.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!tailscale_authenticated(&headers, &users, &origins));
        headers.insert("origin", "https://app.example".parse().unwrap());
        headers.insert("tailscale-user-login", "other@example.com".parse().unwrap());
        assert!(!tailscale_authenticated(&headers, &users, &origins));
        headers.insert("tailscale-user-login", "owner@example.com".parse().unwrap());
        headers.append("tailscale-user-login", "owner@example.com".parse().unwrap());
        assert!(!tailscale_authenticated(&headers, &users, &origins));
        assert!(login_allowed("", "secret", true));
        assert!(!login_allowed("", "secret", false));
        assert!(login_allowed("secret", "secret", false));
        assert!(!login_allowed("wrong", "secret", true));
    }

    #[tokio::test]
    async fn background_handles_are_paginated_generation_scoped_and_receipted() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut stops = 0;
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/backgroundTerminals/list" => {
                        assert_eq!(request["params"]["threadId"], "thread");
                        if request["params"]["cursor"].is_null() {
                            json!({"data":[{"processId":"101","itemId":"one","command":"sleep 1000","cwd":"/remote"}],"nextCursor":"next"})
                        } else {
                            assert_eq!(request["params"]["cursor"], "next");
                            json!({"data":[{"processId":"102","itemId":"two","command":"watch build","cwd":"/elsewhere"}],"nextCursor":null})
                        }
                    }
                    "thread/backgroundTerminals/terminate" => {
                        assert_eq!(request["params"]["threadId"], "thread");
                        assert_eq!(request["params"]["processId"], "102");
                        stops += 1;
                        json!({"terminated":true})
                    }
                    method => panic!("unexpected RPC: {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            assert_eq!(stops, 1);
        });
        let app = fixture()?;
        let session = app
            .manager
            .store
            .create("background", &url, &[], Some("thread"))?;
        let (rpc, _events) = crate::rpc::Rpc::connect(&url).await?;
        app.manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(crate::manager::Live {
                rpc,
                generation: "original".into(),
                thread: "thread".into(),
                turn: tokio::sync::Mutex::new(None),
            }),
        );
        let snapshot = app.manager.background_snapshot(&session.id).await;
        assert_eq!(snapshot["generation"], "original");
        assert_eq!(snapshot["data"].as_array().unwrap().len(), 2);
        // No target may be invented from the selected environments or cwd.
        assert!(snapshot["data"][0].get("environmentId").is_none());
        assert!(
            app.manager
                .stop_background(&session.id, "old", &[("102".into(), "two".into())])
                .await
                .unwrap_err()
                .to_string()
                .contains("old Codex")
        );
        // Entire batch is validated before its first stop.
        assert!(
            app.manager
                .stop_background(
                    &session.id,
                    "original",
                    &[
                        ("102".into(), "two".into()),
                        ("101".into(), "reused".into())
                    ]
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
        let command = Operation::StopBackground {
            id: session.id.clone(),
            generation: "original".into(),
            processes: vec![
                ("102".into(), "two".into()),
                ("gone".into(), "finished".into()),
            ],
        };
        let receipt = uuid::Uuid::new_v4().to_string();
        let first = execute(&app, &receipt, command.clone()).await?;
        assert_eq!(execute(&app, &receipt, command).await?, first);
        let result: serde_json::Value = serde_json::from_str(&first)?;
        assert_eq!(result["results"][1]["terminated"], false);
        app.manager.disconnect(&session.id, "done").await?;
        assert!(
            app.manager.background_snapshot(&session.id).await["error"]
                .as_str()
                .unwrap()
                .contains("disconnected")
        );
        fake.await?;
        Ok(())
    }

    #[tokio::test]
    async fn explicit_queue_is_queued_once_and_never_steers() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut queued = 0;
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/queue/add" => {
                        queued += 1;
                        assert_eq!(request["params"]["input"][0]["text"], "follow up");
                        assert!(
                            uuid::Uuid::parse_str(
                                request["params"]["clientUserMessageId"].as_str().unwrap()
                            )
                            .is_ok()
                        );
                        json!({"queuedSubmission":{"id":"queued-one","input":request["params"]["input"]}})
                    }
                    method => panic!("unexpected RPC: {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            assert_eq!(queued, 1);
        });
        let app = fixture()?;
        let session = app
            .manager
            .store
            .create("busy", &url, &[], Some("thread"))?;
        let (rpc, _events) = crate::rpc::Rpc::connect(&url).await?;
        app.manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(crate::manager::Live {
                rpc,
                generation: "fixture".into(),
                thread: "thread".into(),
                turn: tokio::sync::Mutex::new(Some("active".into())),
            }),
        );
        let request = uuid::Uuid::new_v4().to_string();
        let command = Operation::QueuePrompt {
            id: session.id.clone(),
            text: "follow up".into(),
        };
        let first = execute(&app, &request, command.clone()).await?;
        assert!(first.contains("queued-one"));
        assert_eq!(execute(&app, &request, command).await?, first);
        app.manager.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
    }
    #[tokio::test]
    async fn busy_send_steers_once_and_rejected_steering_never_starts_or_queues_a_turn()
    -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut steers = 0;
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
                let reply = match request["method"].as_str().unwrap() {
                    "initialize" => json!({"id":request["id"],"result":{}}),
                    "initialized" => continue,
                    "turn/steer" => {
                        steers += 1;
                        assert_eq!(request["params"]["expectedTurnId"], "active");
                        assert!(
                            uuid::Uuid::parse_str(
                                request["params"]["clientUserMessageId"].as_str().unwrap()
                            )
                            .is_ok()
                        );
                        if request["params"]["input"][0]["text"] == "race" {
                            json!({"id":request["id"],"error":{"code":-32600,"message":"active turn ended"}})
                        } else {
                            json!({"id":request["id"],"result":{"turnId":"active"}})
                        }
                    }
                    method => panic!("unexpected fallback or approval response: {method}"),
                };
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
            assert_eq!(steers, 2);
        });
        let app = fixture()?;
        let session = app
            .manager
            .store
            .create("busy", &url, &[], Some("thread"))?;
        let (rpc, _events) = crate::rpc::Rpc::connect(&url).await?;
        app.manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(crate::manager::Live {
                rpc,
                generation: "fixture".into(),
                thread: "thread".into(),
                turn: tokio::sync::Mutex::new(Some("active".into())),
            }),
        );
        let request = uuid::Uuid::new_v4().to_string();
        let command = Operation::Prompt {
            id: session.id.clone(),
            text: "steer".into(),
        };
        let first = execute(&app, &request, command.clone()).await?;
        assert!(first.contains("active"));
        assert_eq!(execute(&app, &request, command).await?, first);
        let request = uuid::Uuid::new_v4().to_string();
        let rejected = Operation::Prompt {
            id: session.id.clone(),
            text: "race".into(),
        };
        assert!(execute(&app, &request, rejected.clone()).await.is_err());
        assert!(execute(&app, &request, rejected).await.is_err());
        app.manager.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
    }

    #[tokio::test]
    async fn ended_turn_falls_back_once_under_the_same_receipt() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::Value;
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut calls = Vec::new();
            let mut client_id = Value::Null;
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: Value = serde_json::from_str(&raw).unwrap();
                let method = request["method"].as_str().unwrap();
                let reply = match method {
                    "initialize" => json!({"id":request["id"],"result":{}}),
                    "initialized" => continue,
                    "turn/steer" => {
                        calls.push(method.to_string());
                        assert_eq!(request["params"]["expectedTurnId"], "old");
                        client_id = request["params"]["clientUserMessageId"].clone();
                        json!({"id":request["id"],"error":{"code":-32600,"message":"no active turn to steer"}})
                    }
                    "thread/read" => {
                        calls.push(method.to_string());
                        json!({"id":request["id"],"result":{"thread":{"status":{"type":"idle"}}}})
                    }
                    "turn/start" => {
                        calls.push(method.to_string());
                        assert_eq!(request["params"]["clientUserMessageId"], client_id);
                        assert_eq!(request["params"]["input"][0]["text"], "deliver soon");
                        json!({"id":request["id"],"result":{"turn":{"id":"new"}}})
                    }
                    other => panic!("unexpected request {other}"),
                };
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
            assert_eq!(calls, vec!["turn/steer", "thread/read", "turn/start"]);
        });
        let app = fixture()?;
        let session = app
            .manager
            .store
            .create("race", &url, &[], Some("thread"))?;
        let (rpc, _events) = crate::rpc::Rpc::connect(&url).await?;
        app.manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(crate::manager::Live {
                rpc,
                generation: "fixture".into(),
                thread: "thread".into(),
                turn: tokio::sync::Mutex::new(Some("old".into())),
            }),
        );
        let receipt = uuid::Uuid::new_v4().to_string();
        let command = Operation::Prompt {
            id: session.id.clone(),
            text: "deliver soon".into(),
        };
        let first = execute(&app, &receipt, command.clone()).await?;
        assert!(first.contains("new"));
        assert_eq!(execute(&app, &receipt, command).await?, first);
        assert_eq!(
            app.manager
                .runtime(&session.id)
                .await?
                .turn
                .lock()
                .await
                .as_deref(),
            Some("new")
        );
        app.manager.disconnect(&session.id, "done").await?;
        fake.await?;
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
