//! Native Wormhole client. Calls are never replayed after disconnect or timeout.
use anyhow::{Context, Result, ensure};
pub use demodex_protocol as protocol;
use futures_util::{SinkExt, StreamExt};
use protocol::{Api, Hello, Login, Notice, Operation, Response, VERSION};
use ractor::{ActorCell, ActorRef};
use ractor_wormhole::{
    conduit::{self, ConduitMessage},
    nexus::start_nexus,
    portal::{Portal, PortalActorMessage},
    util::{ActorRef_Ask, FnActor},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Changed,
    Closed,
}

struct Actors(Vec<ActorCell>, Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for Actors {
    fn drop(&mut self) {
        self.1.take();
        for actor in &self.0 {
            actor.stop(None);
        }
    }
}

pub struct Client {
    api: ActorRef<Api>,
    _actors: Actors,
    connected: Arc<AtomicBool>,
    updates: watch::Receiver<Wake>,
}
impl Client {
    /// URL is a WebSocket endpoint, normally ws://127.0.0.1:4780/wormhole.
    /// TLS uses the system trust store. Tokens are sent only after schema negotiation.
    pub async fn connect(url: &str, token: String) -> Result<Self> {
        tokio::time::timeout(Duration::from_secs(30), Self::connect_inner(url, token)).await?
    }
    async fn connect_inner(url: &str, token: String) -> Result<Self> {
        let (mut socket, _) = tokio_tungstenite::connect_async_with_config(
            url,
            // The selected daemon is trusted. Imported history is not bounded
            // by upload limits or tungstenite's default 16 MiB frame limit.
            Some(
                WebSocketConfig::default()
                    .max_message_size(None)
                    .max_frame_size(None),
            ),
            false,
        )
        .await?;
        let message = socket
            .next()
            .await
            .context("closed before compatibility hello")??;
        let Message::Text(hello) = message else {
            anyhow::bail!("expected Demodex compatibility hello");
        };
        ensure!(hello.len() <= 4096, "compatibility hello too large");
        serde_json::from_str::<Hello>(&hello)?
            .check()
            .map_err(anyhow::Error::msg)?;
        socket
            .send(Message::Text(
                serde_json::to_string(&Hello::default())?.into(),
            ))
            .await?;
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
                    Ok(Message::Text(text)) => {
                        Some(Ok(ConduitMessage::Handshake(text.to_string())))
                    }
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
        let portal =
            conduit::from_sink_source(nexus, url.into(), Box::pin(sink), Box::pin(source)).await?;
        actors.0.push(portal.get_cell());
        portal.wait_for_opened(Duration::from_secs(10)).await?;
        let address = portal
            .ask(
                |reply| PortalActorMessage::QueryNamedRemoteActor("login".into(), reply),
                Some(Duration::from_secs(10)),
            )
            .await??;
        let login: ActorRef<Login> = portal.instantiate_proxy_for_remote_actor(address).await?;
        let api = login
            .ask(
                |reply| Login::Authenticate {
                    version: VERSION,
                    token,
                    reply,
                },
                Some(Duration::from_secs(10)),
            )
            .await?
            .map_err(anyhow::Error::msg)?;
        let (tx, updates) = watch::channel(Wake::Changed);
        let changed = tx.clone();
        let (sink, _) = FnActor::<Notice>::start_fn(async move |mut ctx| {
            while let Some(Notice::Changed) = ctx.rx.recv().await {
                changed.send_replace(Wake::Changed);
            }
        })
        .await?;
        actors.0.push(sink.get_cell());
        api.ask(
            |reply| Api::Watch { sink, reply },
            Some(Duration::from_secs(10)),
        )
        .await?;
        let connected = Arc::new(AtomicBool::new(true));
        let alive = connected.clone();
        tokio::spawn(async move {
            let _ = portal.wait(None).await;
            alive.store(false, Ordering::Release);
            tx.send_replace(Wake::Closed);
        });
        Ok(Self {
            api,
            _actors: actors,
            connected,
            updates,
        })
    }
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
    pub fn subscribe(&self) -> watch::Receiver<Wake> {
        self.updates.clone()
    }
    pub async fn call(&self, request_id: String, operation: Operation) -> Result<Response> {
        let mut connection = self.updates.clone();
        ensure!(
            self.is_connected(),
            "Connection closed; this call was not submitted"
        );
        let call = self.api.ask(
            |reply| Api::Call {
                request_id,
                operation,
                reply,
            },
            Some(Duration::from_secs(120)),
        );
        let result = tokio::select! {
            result = call => result,
            _ = connection.wait_for(|wake| *wake == Wake::Closed) => {
                anyhow::bail!("Connection interrupted; outcome may be uncertain. This call was not replayed");
            }
        };
        result
            .context(
                "Connection interrupted; outcome may be uncertain. This call was not replayed",
            )?
            .map_err(anyhow::Error::msg)
    }
    pub async fn read(&self, operation: Operation) -> Result<Response> {
        ensure!(
            !operation.is_mutation(),
            "read requires a non-mutating operation"
        );
        self.call(String::new(), operation).await
    }
}
