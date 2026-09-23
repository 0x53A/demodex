use anyhow::{Context, Result, anyhow, bail};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

type Reply = oneshot::Sender<Result<Value>>;
type Waiters = Arc<Mutex<HashMap<u64, Reply>>>;
// Reads can be cancelled by an outer snapshot timeout. Remove their waiter
// even when the call future never reaches its normal return path.
struct PendingCall<'a> {
    waiters: &'a Waiters,
    id: u64,
}
impl Drop for PendingCall<'_> {
    fn drop(&mut self) {
        self.waiters.lock().unwrap().remove(&self.id);
    }
}
type WsError = tokio_tungstenite::tungstenite::Error;
type WsSink = Pin<Box<dyn Sink<Message,Error=WsError>+Send>>;
type WsStream = Pin<Box<dyn Stream<Item=std::result::Result<Message,WsError>>+Send>>;
struct Outgoing {
    message: Value,
    sent: oneshot::Sender<Result<()>>,
}

/// An explicit JSON-RPC error reply, distinct from unknown-delivery transport errors.
#[derive(Debug)]
pub struct RemoteError(pub Value);
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Codex: {}", self.0) }
}
impl std::error::Error for RemoteError {}

pub fn no_active_turn(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RemoteError>().is_some_and(|reply|
        reply.0["code"] == -32600 && reply.0["message"] == "no active turn to steer")
}

pub struct Rpc {
    out: mpsc::Sender<Outgoing>,
    pending: Waiters,
    next: AtomicU64,
    task: tokio::task::AbortHandle,
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Rpc {
    pub fn close(&self) {
        self.task.abort();
        for (_,sender) in self.pending.lock().unwrap().drain() {
            let _=sender.send(Err(anyhow!("connection closed by manager")));
        }
    }
    pub async fn connect(url: &str) -> Result<(Self, mpsc::Receiver<Result<Value>>)> {
        // Endpoints are host-admin configuration, never supplied by an agent tool.
        let (mut sink,mut stream): (WsSink,WsStream)=tokio::time::timeout(Duration::from_secs(10),async {
            if let Some(path)=url.strip_prefix("unix://") {
                let stream=tokio::net::UnixStream::connect(path).await?;
                let (socket,_)=tokio_tungstenite::client_async("ws://localhost/",stream).await?;
                let (sink,stream)=socket.split();
                Ok::<(WsSink,WsStream),anyhow::Error>((Box::pin(sink),Box::pin(stream)))
            } else {
                anyhow::ensure!(url.starts_with("ws://"),"use a private ws:// or unix:// app-server endpoint");
                let (socket,_)=tokio_tungstenite::connect_async(url).await?;
                let (sink,stream)=socket.split();
                Ok::<(WsSink,WsStream),anyhow::Error>((Box::pin(sink),Box::pin(stream)))
            }
        }).await.context("app-server connection timed out")??;
        let (out, mut commands) = mpsc::channel::<Outgoing>(64);
        let (events, receiver) = mpsc::channel(256);
        let pending: Waiters = Arc::new(Mutex::new(HashMap::new()));
        let waiters = pending.clone();
        let task = tokio::spawn(async move {
            let outcome: Result<()> = async {
                loop {
                    tokio::select! {
                        command = commands.recv() => {
                            let Some(command) = command else { return Ok(()) };
                            match sink.send(Message::Text(command.message.to_string().into())).await {
                                Ok(()) => { let _ = command.sent.send(Ok(())); }
                                Err(error) => {
                                    let _ = command.sent.send(Err(anyhow!(error.to_string())));
                                    return Err(error.into());
                                }
                            }
                        }
                        frame = stream.next() => {
                            match frame.context("app-server closed connection")?? {
                                Message::Text(text) => {
                                    let message: Value = serde_json::from_str(&text)?;
                                    if message.get("method").is_none() {
                                        if let Some(id) = message["id"].as_u64() {
                                            let waiter = waiters.lock().unwrap().remove(&id);
                                            if let Some(waiter) = waiter {
                                                let reply = if let Some(error)=message.get("error") { Err(anyhow::Error::new(RemoteError(error.clone()))) }
                                                    else { Ok(message["result"].clone()) };
                                                let _ = waiter.send(reply);
                                            }
                                        }
                                    } else { events.try_send(Ok(message)).context("event consumer fell behind; disconnecting instead of stalling RPC")?; }
                                }
                                Message::Ping(data) => sink.send(Message::Pong(data)).await?,
                                Message::Close(_) => bail!("app-server disconnected"),
                                _ => {}
                            }
                        }
                    }
                }
            }.await;
            let reason = outcome
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "connection closed".into());
            for (_, sender) in waiters.lock().unwrap().drain() {
                let _ = sender.send(Err(anyhow!(reason.clone())));
            }
            let _ = events.send(Err(anyhow!(reason))).await;
        });
        let rpc = Self {
            out,
            pending,
            next: AtomicU64::new(1),
            task: task.abort_handle(),
        };
        rpc.call("initialize", json!({"clientInfo":{"name":"demodex","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        rpc.send(json!({"method":"initialized","params":{}}))
            .await?;
        Ok((rpc, receiver))
    }

    pub async fn send(&self, message: Value) -> Result<()> {
        let (sent, received) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(15), async {
            self.out
                .send(Outgoing { message, sent })
                .await
                .context("connection unavailable")?;
            received
                .await
                .context("connection closed before send acknowledgement")?
        })
        .await
        .context("send timed out; delivery is unknown, do not automatically retry")?
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let _pending = PendingCall { waiters: &self.pending, id };
        self.send(json!({"id":id,"method":method,"params":params})).await?;
        tokio::time::timeout(Duration::from_secs(45),receiver).await
            .context("Codex response timed out; operation may have executed, inspect state before retrying")?
            .context("connection lost before response")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_requires_the_explicit_no_active_turn_error() {
        let reply=|code,message|anyhow::Error::new(RemoteError(json!({"code":code,"message":message})));
        assert!(no_active_turn(&reply(-32600,"no active turn to steer")));
        assert!(!no_active_turn(&reply(-32600,"cannot steer a review turn")));
        assert!(!no_active_turn(&reply(-32603,"no active turn to steer")));
        assert!(!no_active_turn(&anyhow!("no active turn to steer")));
        assert!(!no_active_turn(&anyhow!("connection lost before response")));
    }

    #[tokio::test]
    async fn cancelling_a_call_releases_its_waiter_and_keeps_the_connection() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let (seen, received) = oneshot::channel();
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut seen = Some(seen);
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: Value = serde_json::from_str(&raw).unwrap();
                if request["method"] == "initialized" { continue; }
                if request["method"] == "hang" {
                    seen.take().unwrap().send(()).unwrap();
                    continue;
                }
                ws.send(Message::Text(json!({"id":request["id"],"result":{}}).to_string().into())).await.unwrap();
            }
        });
        let (rpc, _events) = Rpc::connect(&url).await?;
        let mut call = Box::pin(rpc.call("hang", json!({})));
        tokio::select! {
            result = &mut call => panic!("call completed unexpectedly: {result:?}"),
            result = received => result?,
        }
        assert_eq!(rpc.pending.lock().unwrap().len(), 1);
        drop(call);
        assert!(rpc.pending.lock().unwrap().is_empty());
        assert_eq!(rpc.call("still-connected", json!({})).await?, json!({}));
        rpc.close();
        fake.await?;
        Ok(())
    }

    #[tokio::test]
    async fn routes_interleaved_server_requests_and_out_of_order_responses() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let init: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::Text(
                json!({"id":init["id"],"result":{}}).to_string().into(),
            ))
            .await
            .unwrap();
            ws.next().await.unwrap().unwrap();
            let a: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let b: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            for message in [
                json!({"id":99,"method":"approval","params":{}}),
                json!({"id":b["id"],"result":b["method"]}),
                json!({"id":a["id"],"result":a["method"]}),
            ] {
                ws.send(Message::Text(message.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let (rpc, mut events) = Rpc::connect(&url).await?;
        let (a, b) = tokio::join!(rpc.call("a", json!({})), rpc.call("b", json!({})));
        assert_eq!(a?, json!("a"));
        assert_eq!(b?, json!("b"));
        assert_eq!(events.recv().await.unwrap()?["id"], json!(99));
        fake.await?;
        Ok(())
    }
}
