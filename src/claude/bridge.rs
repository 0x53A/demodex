use super::{Claude, translate};
use anyhow::{Context, Result, ensure};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
type Body = UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
pub(super) struct Bridge {
    pub url: String,
    pub key: String,
    pub stop: CancellationToken,
    pub task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
fn full(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(
            Full::new(Bytes::from(value.to_string()))
                .map_err(|e| match e {})
                .boxed_unsync(),
        )
        .unwrap()
}
pub(super) async fn start(claude: Arc<Claude>) -> Result<Bridge> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let url = format!("http://{}/v1", listener.local_addr()?);
    let key = super::random();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let secret = key.clone();
    let weak = Arc::downgrade(&claude);
    let task = tokio::spawn(async move {
        let jobs = tokio_util::task::TaskTracker::new();
        loop {
            tokio::select! {biased;_=stopped.cancelled()=>break,
                incoming=listener.accept()=>{
                    let Ok((stream,_))=incoming else{break;};let Some(account)=weak.upgrade() else{break;};
                    let key=secret.clone();let cancel=stopped.clone();let streams=jobs.clone();
                    jobs.spawn(async move{let service=service_fn(move |r|{let a=account.clone();let k=key.clone();let streams=streams.clone();async move{
                        let response=match request(a,&k,r,&streams).await {Ok(r)=>r,Err(e)=>{ #[cfg(test)] eprintln!("BRIDGE ERROR: {e}"); tracing::warn!("Claude bridge request failed: {}", super::clip(&format!("{e:#}"))); full(StatusCode::BAD_GATEWAY,json!({"error":{"message":e.to_string(),"type":"provider_error"}})) }};
                        Ok::<_,Infallible>(response)
                    }});
                    tokio::select!{_=cancel.cancelled()=>{},_=hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream),service)=>{}}
                    });
                }
            }
        }
        jobs.close();
        jobs.wait().await;
    });
    Ok(Bridge {
        url,
        key,
        stop,
        task: tokio::sync::Mutex::new(Some(task)),
    })
}
async fn request(
    claude: Arc<Claude>,
    key: &str,
    r: Request<Incoming>,
    jobs: &tokio_util::task::TaskTracker,
) -> Result<Response<Body>> {
    if r.headers().contains_key("origin")
        || r.headers()
            .get("x-demodex-inference")
            .and_then(|v| v.to_str().ok())
            != Some(key)
    {
        return Ok(full(
            StatusCode::FORBIDDEN,
            json!({"error":{"message":"Unauthorized inference client"}}),
        ));
    }
    if r.method() != hyper::Method::POST
        || !matches!(r.uri().path(), "/v1/responses" | "/v1/responses/compact")
    {
        return Ok(full(
            StatusCode::NOT_FOUND,
            json!({"error":{"message":"Unsupported inference route"}}),
        ));
    }
    let compact = r.uri().path().ends_with("/compact");
    let data = tokio::time::timeout(
        Duration::from_secs(30),
        http_body_util::Limited::new(r.into_body(), 64 * 1024 * 1024).collect(),
    )
    .await
    .context("Inference request timed out")?
    .map_err(|e| anyhow::anyhow!("Invalid inference body: {e}"))?
    .to_bytes();
    let input: Value = serde_json::from_slice(&data).context("Invalid Responses request")?;
    let model = input["model"].as_str().context("Missing inference model")?;
    let is_claude = model.starts_with("claude-");
    ensure!(
        is_claude,
        "GPT requests cannot use the Claude adapter; use the native OpenAI runtime"
    );
    ensure!(
        !compact,
        "Claude uses local Codex compaction; remote compaction is unsupported"
    );
    let capabilities = claude.model(model).await?;
    let translated = translate::request(&input, &capabilities)?;
    let access = claude.access().await?;
    let upstream = super::anthropic_headers(
        claude.client.post(format!("{}/v1/messages", claude.api)),
        &access,
        translated.body["thinking"]["type"] == "enabled",
    )
    .header("content-type", "application/json")
    .body(translated.body.to_string());
    let response = tokio::time::timeout(Duration::from_secs(90), upstream.send())
        .await
        .context("Inference connection timed out")?
        .context("Inference connection failed")?;
    let status = response.status();
    if !status.is_success() {
        // Log only Anthropic's error type/message (bounded) locally for diagnosis. The full
        // body is never logged or returned, since it can echo prompts or credentials.
        let detail = match tokio::time::timeout(Duration::from_secs(5), response.bytes()).await {
            Ok(Ok(body)) if body.len() <= 64 * 1024 => super::error_summary(&body),
            _ => "error body unavailable".into(),
        };
        tracing::warn!(
            "Claude upstream HTTP {} for model {model}: {detail}",
            status.as_u16()
        );
        // Do not echo provider response bodies (which can contain prompts or credentials).
        return Ok(full(
            status,
            json!({"error":{"message":format!("Claude inference failed (HTTP {})",status.as_u16()),"type":"provider_error"}}),
        ));
    }
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<Bytes, Box<dyn std::error::Error + Send + Sync>>>(16);
    jobs.spawn(async move {
        let mut response = response;
        let mut converter = translate::Stream::new(translated.tools);
        let work = async {
            let mut pending = Vec::new();
            loop {
                let chunk = tokio::select! { _=sender.closed()=>return Ok::<_,anyhow::Error>(()), v=tokio::time::timeout(Duration::from_secs(120),response.chunk())=>v.context("Inference stream timed out")?? };
                let Some(chunk) = chunk else { break; };
                pending.extend_from_slice(&chunk);
                ensure!(pending.len() <= 8*1024*1024,"Upstream SSE event too large");
                while let Some(end) = pending.iter().position(|c| *c == b'\n') {
                    let line: Vec<_> = pending.drain(..=end).collect();
                    let line = std::str::from_utf8(&line)?.trim();
                    if let Some(data) = line.strip_prefix("data:") {
                        for event in converter.accept(serde_json::from_str(data.trim())?)? {
                            if sender.send(Ok(Bytes::from(event))).await.is_err() { return Ok(()); }
                        }
                    }
                }
            }
            ensure!(pending.is_empty(),"Truncated inference event");
            ensure!(converter.finished,"Claude stream ended before completion");
            Ok(())
        }.await;
        if let Err(error) = work { let _ = sender.send(Ok(Bytes::from(converter.failure(&error.to_string())))).await; }
    });
    let stream = futures_util::stream::unfold(receiver, |mut rx| async move {
        rx.recv().await.map(|v| (v.map(Frame::data), rx))
    });
    Ok(Response::builder()
        .status(status)
        .header("cache-control", "no-store")
        .header("content-type", "text/event-stream")
        .body(StreamBody::new(stream).boxed_unsync())?)
}
