//! HTTP is limited to static files and WebSocket negotiation. No command routes.
use crate::wormhole::Endpoint;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, future::Future, path::PathBuf};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        handshake::server::create_response,
        protocol::{Role, WebSocketConfig},
    },
};

type Body = UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
pub(crate) struct Server {
    endpoint: Option<Endpoint>,
    directory: Option<PathBuf>,
    stopped: tokio_util::sync::CancellationToken,
}
impl Server {
    pub(crate) fn new(endpoint: Option<Endpoint>, directory: Option<PathBuf>) -> Self {
        Self {
            endpoint,
            directory,
            stopped: tokio_util::sync::CancellationToken::new(),
        }
    }
    pub(crate) fn static_only(directory: PathBuf) -> Self {
        Self::new(None, Some(directory))
    }

    async fn request(self, mut request: Request<Incoming>) -> Result<Response<Body>, Infallible> {
        if let Some(endpoint) = self.endpoint.filter(|e| e.matches(request.uri().path())) {
            let identity = match endpoint.authorize(request.headers()) {
                Ok(identity) => identity,
                Err(status) => return Ok(empty(status)),
            };
            let mut handshake = Request::builder()
                .method(request.method())
                .uri(request.uri())
                .version(request.version())
                .body(())
                .unwrap();
            *handshake.headers_mut() = request.headers().clone();
            let response = match create_response(&handshake) {
                Ok(response) => response,
                Err(_) => return Ok(empty(StatusCode::BAD_REQUEST)),
            };
            let upgraded = hyper::upgrade::on(&mut request);
            tokio::spawn(async move {
                tokio::select! {
                _ = self.stopped.cancelled() => {},
                _ = async {
                if let Ok(stream) = upgraded.await {
                    let socket = WebSocketStream::from_raw_socket(TokioIo::new(stream), Role::Server,
                        Some(WebSocketConfig::default().max_message_size(Some(8 * 1024 * 1024)))).await;
                    if let Err(error) = endpoint.connect(socket, identity).await {
                        tracing::debug!("Wormhole connection ended: {error:#}");
                    }
                }
                } => {},
                }
            });
            return Ok(response.map(|_| body(Bytes::new())));
        }
        // Retired API paths must never fall through to the SPA's index document.
        if request.uri().path() == "/api" || request.uri().path().starts_with("/api/") {
            return Ok(empty(StatusCode::NOT_FOUND));
        }
        if let Some(directory) = self.directory {
            let mut files = tower_http::services::ServeDir::new(&directory).not_found_service(
                tower_http::services::ServeFile::new(directory.join("index.html")),
            );
            match files.try_call(request).await {
                Ok(response) => {
                    return Ok(
                        response.map(|body| body.map_err(|error| error.into()).boxed_unsync())
                    );
                }
                Err(error) => tracing::warn!("static file: {error}"),
            }
        }
        Ok(empty(StatusCode::NOT_FOUND))
    }

    async fn connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(self, stream: S) {
        let connection = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |request| self.clone().request(request)),
            )
            .with_upgrades();
        if let Err(error) = connection.await {
            tracing::debug!("HTTP connection ended: {error}");
        }
    }

    pub(crate) async fn serve_tcp(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()>,
    ) -> std::io::Result<()> {
        let _cancel_on_exit = self.stopped.clone().drop_guard();
        tokio::pin!(shutdown);
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut shutdown => { self.stopped.cancel(); return Ok(()); },
                accepted = listener.accept() => { let (stream, _) = accepted?; connections.spawn(self.clone().connection(stream)); },
                _ = connections.join_next(), if !connections.is_empty() => {},
            }
        }
    }
    pub(crate) async fn serve_unix(
        self,
        listener: tokio::net::UnixListener,
        shutdown: impl Future<Output = ()>,
    ) -> std::io::Result<()> {
        let _cancel_on_exit = self.stopped.clone().drop_guard();
        tokio::pin!(shutdown);
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut shutdown => { self.stopped.cancel(); return Ok(()); },
                accepted = listener.accept() => { let (stream, _) = accepted?; connections.spawn(self.clone().connection(stream)); },
                _ = connections.join_next(), if !connections.is_empty() => {},
            }
        }
    }
}
fn body(bytes: Bytes) -> Body {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}
fn empty(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(body(Bytes::new()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Service};
    use anyhow::Context;
    use demodex_client::{
        Client,
        protocol::{NewSession, Operation, Response as Reply},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn native_transport_auth_receipts_reconnect_and_static_routes() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let service = Service::open(Config {
            data_dir: root.path().join("state"),
            vm_image: None,
            host_workspace: None,
            codex_home: None,
        })?;
        let web = root.path().join("web");
        std::fs::create_dir(&web)?;
        std::fs::write(web.join("index.html"), "static fixture")?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let url = format!("ws://{addr}/wormhole");
        let server = Server::new(
            Some(Endpoint::new(
                service.clone(),
                "test-secret".into(),
                vec![],
                vec![],
            )),
            Some(web),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.serve_tcp(listener, async {
            let _ = stopped.await;
        }));
        for (path, status) in [("/", "200"), ("/api/sessions", "404")] {
            let mut stream = tokio::net::TcpStream::connect(addr).await?;
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await?;
            let mut response = String::new();
            stream.read_to_string(&mut response).await?;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
            if path == "/" {
                assert!(response.ends_with("static fixture"));
            }
        }
        assert!(Client::connect(&url, "wrong".into()).await.is_err());
        assert!(Client::connect(&url, String::new()).await.is_err());
        let client = Client::connect(&url, "test-secret".into()).await?;
        let mut updates = client.subscribe();
        updates.borrow_and_update();
        let receipt = uuid::Uuid::new_v4().to_string();
        let command = Operation::ExternalSession {
            input: NewSession {
                name: "Fixture".into(),
                endpoint: "ws://127.0.0.1:1".into(),
                ..Default::default()
            },
        };
        let first = client.call(receipt.clone(), command.clone()).await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), updates.changed())
            .await
            .context("change notice")??;
        assert_eq!(first, client.call(receipt.clone(), command.clone()).await?);
        let Reply::Session(created) = first else {
            panic!("typed session reply required");
        };
        assert!(
            client
                .call(
                    receipt.clone(),
                    Operation::Archive {
                        id: created.id.clone(),
                        archived: true
                    }
                )
                .await
                .is_err()
        );
        drop(client);
        let client = Client::connect(&url, "test-secret".into()).await?;
        let Reply::Sessions(sessions) = client.read(Operation::Sessions).await? else {
            panic!("typed session list required");
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, created.id);
        // A single event can exceed the old REST response limit. Test the wire too.
        service.manager.store.event(
            &created.id,
            &serde_json::json!({"text":"x".repeat(16 * 1024 * 1024)}),
        )?;
        let Reply::Events(events) = client
            .read(Operation::Events {
                id: created.id,
                after: 0,
            })
            .await?
        else {
            panic!("typed event list required");
        };
        assert_eq!(
            events[0].message["text"].as_str().unwrap().len(),
            16 * 1024 * 1024
        );
        let mut closed = client.subscribe();
        // Stall a receipted call and close its portal. The client must report
        // uncertainty promptly rather than keeping its caller busy for 120s.
        let blocked = service.commands.lock().await;
        let pending = client.call(receipt, command);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("call unexpectedly finished: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
        }
        stop.send(()).unwrap();
        task.await??;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut pending)
                .await?
                .is_err()
        );
        drop(blocked);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while *closed.borrow_and_update() != demodex_client::Wake::Closed {
                closed.changed().await.unwrap();
            }
        })
        .await
        .context("socket shutdown")?;
        assert!(!client.is_connected());
        Ok(())
    }
}
