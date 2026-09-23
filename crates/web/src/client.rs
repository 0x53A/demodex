use anyhow::{Context, Result};
use demodex_protocol::{Api, Hello, Login, Notice, Operation, VERSION};
use ractor::{ActorCell, ActorRef};
use ractor_wormhole::{
    conduit::websocket::client::ewebsock,
    nexus::start_nexus,
    portal::{Portal, PortalActorMessage},
    util::{ActorRef_Ask, FnActor},
};
use serde_json::Value;
use std::{rc::Rc, time::Duration};
use tokio::sync::mpsc;

pub enum Wake {
    Changed,
    Closed,
}
pub struct Client {
    api: ActorRef<Api>,
    actors: Vec<ActorCell>,
}
impl Drop for Client {
    fn drop(&mut self) {
        for actor in &self.actors {
            actor.stop(None);
        }
    }
}

struct Connecting(Vec<ActorCell>);
impl Drop for Connecting {
    fn drop(&mut self) {
        for actor in &self.0 {
            actor.stop(None);
        }
    }
}

impl Client {
    pub async fn connect(base: &str, token: String) -> Result<(Rc<Self>, mpsc::Receiver<Wake>)> {
        Self::connect_inner(base, token).await.map_err(|error| {
            let message = format!("{error:#}");
            if message.contains("WebSocket") {
                anyhow::anyhow!("{message}\nCheck that Tailscale is connected and the host is running. If you denied Firefox's local-network or Android Nearby devices prompt, allow access in Firefox's site settings or Android Settings → Apps → Firefox → Permissions, then retry. The browser does not reveal the exact network failure cause.")
            } else {
                error
            }
        })
    }

    async fn connect_inner(base: &str, token: String) -> Result<(Rc<Self>, mpsc::Receiver<Wake>)> {
        let url = web_sys::Url::new(base).map_err(|_| anyhow::anyhow!("Enter a valid host URL"))?;
        let local = matches!(url.hostname().as_str(), "localhost" | "127.0.0.1" | "[::1]");
        anyhow::ensure!(
            url.protocol() == "https:" || (url.protocol() == "http:" && local),
            "Use the host's Tailscale HTTPS URL (HTTP is only allowed on localhost)"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_empty()
                && url.search().is_empty()
                && url.hash().is_empty()
                && matches!(url.pathname().as_str(), "" | "/"),
            "Use a host origin without credentials, query, or path"
        );
        let websocket = format!(
            "{}//{}/wormhole",
            if url.protocol() == "https:" {
                "wss:"
            } else {
                "ws:"
            },
            url.host()
        );
        let nexus = start_nexus(None, None)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        let mut guard = Connecting(vec![nexus.get_cell()]);
        // The pinned library's convenience client doesn't forward socket-close
        // events. Adapt them explicitly so the UI can reconnect after a dropout.
        let (opened_tx, opened_rx) = tokio::sync::oneshot::channel();
        let opened = std::sync::Arc::new(std::sync::Mutex::new(Some(opened_tx)));
        let (tx_socket, mut rx_socket) = mpsc::unbounded_channel();
        let mut sender = ::ewebsock::ws_connect(
            websocket.clone(),
            ::ewebsock::Options::default(),
            Box::new(move |event| {
                use ::ewebsock::{WsEvent, WsMessage};
                use ractor_wormhole::conduit::ConduitMessage;
                use std::ops::ControlFlow;
                match event {
                    WsEvent::Opened => {
                        if let Some(tx) = opened.lock().unwrap().take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    WsEvent::Message(WsMessage::Text(text)) => {
                        let _ = tx_socket.send(ConduitMessage::Handshake(text));
                    }
                    WsEvent::Message(WsMessage::Binary(bytes)) => {
                        let _ = tx_socket.send(ConduitMessage::Content(bytes));
                    }
                    WsEvent::Closed | WsEvent::Error(_) => {
                        if let Some(tx) = opened.lock().unwrap().take() {
                            let _ = tx.send(Err("WebSocket connection failed"));
                        }
                        let _ = tx_socket.send(ConduitMessage::Close(None));
                        return ControlFlow::Break(());
                    }
                    _ => {}
                }
                ControlFlow::Continue(())
            }),
        )
        .map_err(anyhow::Error::msg)
        .context("WebSocket connection could not be started")?;
        let timeout = gloo::timers::future::TimeoutFuture::new(10_000);
        match futures_util::future::select(Box::pin(opened_rx), Box::pin(timeout)).await {
            futures_util::future::Either::Left((result, _)) => {
                result?.map_err(anyhow::Error::msg)?
            }
            futures_util::future::Either::Right(_) => {
                anyhow::bail!("WebSocket connection timed out")
            }
        }
        let hello_timeout = gloo::timers::future::TimeoutFuture::new(10_000);
        let hello =
            match futures_util::future::select(Box::pin(rx_socket.recv()), Box::pin(hello_timeout))
                .await
            {
                futures_util::future::Either::Left((
                    Some(ractor_wormhole::conduit::ConduitMessage::Handshake(text)),
                    _,
                )) => text,
                futures_util::future::Either::Left((None | Some(ractor_wormhole::conduit::ConduitMessage::Close(_)), _)) => anyhow::bail!("WebSocket closed before the host replied"),
                _ => anyhow::bail!(
                    "Protocol mismatch: host did not send a Demodex compatibility hello"
                ),
            };
        anyhow::ensure!(
            hello.len() <= 4096,
            "Protocol mismatch: invalid compatibility hello"
        );
        serde_json::from_str::<Hello>(&hello)
            .context("Protocol mismatch: host uses an older or different handshake")?
            .check()
            .map_err(anyhow::Error::msg)?;
        sender.send(::ewebsock::WsMessage::Text(serde_json::to_string(
            &Hello::default(),
        )?));
        let sink = ewebsock::adapt_WsSender_to_Conduit(sender).await?;
        let source = ewebsock::adapt_tokio_receiver_to_Conduit(rx_socket);
        let portal =
            ractor_wormhole::conduit::from_sink_source(nexus, websocket, sink, source).await?;
        guard.0.push(portal.get_cell());
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
        let (tx, rx) = mpsc::channel(1);
        let tx_changed = tx.clone();
        let (sink, _) = FnActor::<Notice>::start_fn(async move |mut ctx| {
            while let Some(Notice::Changed) = ctx.rx.recv().await {
                let _ = tx_changed.try_send(Wake::Changed);
            }
        })
        .await?;
        guard.0.push(sink.get_cell());
        api.ask(
            |reply| Api::Watch { sink, reply },
            Some(Duration::from_secs(10)),
        )
        .await?;
        wasm_bindgen_futures::spawn_local(async move {
            let _ = portal.wait(None).await;
            let _ = tx.send(Wake::Closed).await;
        });
        Ok((
            Rc::new(Self {
                api,
                actors: std::mem::take(&mut guard.0),
            }),
            rx,
        ))
    }

    pub async fn call(&self, operation: Operation, request_id: String) -> Result<Value> {
        let response = self.api.ask(|reply| Api::Call { request_id, operation, reply }, Some(Duration::from_secs(60))).await
            .context("Connection interrupted; the command may have been accepted. It will not be replayed")?
            .map_err(anyhow::Error::msg)?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn read(&self, operation: Operation) -> Result<Value> {
        self.call(operation, String::new()).await
    }

    pub async fn snapshot(&self, selected: String, after: i64) -> Result<Snapshot> {
        let (sessions, runtime, environments, targets) = futures_util::try_join!(
            self.read(Operation::Sessions),
            self.read(Operation::Runtime),
            self.read(Operation::Environments),
            self.read(Operation::Targets)
        )?;
        let mut snapshot = Snapshot {
            sessions,
            runtime,
            environments,
            targets,
            selected: selected.clone(),
            detail: Value::Null,
            events: vec![],
        };
        if !selected.is_empty()
            && snapshot
                .sessions
                .as_array()
                .is_some_and(|sessions| sessions.iter().any(|s| s["id"] == selected))
        {
            snapshot.detail = self
                .read(Operation::Detail {
                    id: selected.clone(),
                })
                .await?;
            let mut cursor = after;
            loop {
                let batch = self
                    .read(Operation::Events {
                        id: selected.clone(),
                        after: cursor,
                    })
                    .await?;
                let batch = batch.as_array().context("Invalid event response")?;
                if let Some(event) = batch.last() {
                    cursor = event["seq"].as_i64().context("Invalid event cursor")?;
                }
                snapshot.events.extend(batch.iter().cloned());
                if batch.len() < 500 {
                    break;
                }
            }
        }
        Ok(snapshot)
    }
}

pub struct Snapshot {
    pub sessions: Value,
    pub runtime: Value,
    pub environments: Value,
    pub targets: Value,
    pub selected: String,
    pub detail: Value,
    pub events: Vec<Value>,
}
