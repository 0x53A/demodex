//! Account quota reads never start a runtime or make an inference request.
use super::{Claude, anthropic_headers, now, read_json};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(super) struct Cached {
    generation: u64,
    checked: Instant,
    value: Value,
}

fn snapshot(body: &Value) -> Result<Value> {
    ensure!(body.is_object(), "Invalid Claude usage response");
    let windows: Vec<_> = [("five_hour", 300), ("seven_day", 10080)]
        .into_iter()
        .filter(|(key, _)| body[*key].is_object())
        .map(|(key, minutes)| {
            let window = &body[key];
            let used = window["utilization"].as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n));
            let reset = window["resets_at"].as_str().filter(|s| s.len() <= 64);
            json!({"duration_minutes":minutes,"used_percent":used,"resets_at":reset})
        }).collect();
    Ok(json!({"connected":true,"windows":windows,"checked_at":now()}))
}

impl Claude {
    pub async fn usage(&self) -> Value {
        // Serialize refreshes across all browsers. Failures are cached too.
        let mut cache = self.usage.lock().await;
        let generation = {
            let state = self.state.lock().await;
            if state.credentials.is_none() {
                *cache = None;
                return json!({"connected":false,"windows":[]});
            }
            state.generation
        };
        if let Some(entry) = cache.as_ref()
            && entry.generation == generation
            && entry.checked.elapsed() < Duration::from_secs(300)
        {
            return entry.value.clone();
        }
        let fetched = tokio::time::timeout(Duration::from_secs(5), async {
            let access = self.access().await?;
            let response = anthropic_headers(
                self.client.get(format!("{}/api/oauth/usage", self.api)), &access, false,
            ).send().await?;
            // Never forward provider bodies or request errors containing URLs/tokens.
            ensure!(response.status().is_success(), "Claude usage request failed (HTTP {})", response.status().as_u16());
            snapshot(&read_json(response, 64 * 1024).await?)
        }).await;
        let state = self.state.lock().await;
        if state.generation != generation {
            *cache = None;
            return json!({"connected":state.credentials.is_some(),"windows":[],
                "error":"Claude account changed; usage will refresh"});
        }
        let value = match fetched {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => { tracing::warn!("Claude usage unavailable: {}", super::clip(&format!("{e:#}"))); json!({"connected":true,"windows":[],"checked_at":now(),
                "error":"Claude usage unavailable; check the account connection or try again later"}) },
            Err(_) => json!({"connected":true,"windows":[],"checked_at":now(),
                "error":"Claude usage request timed out"}),
        };
        *cache = Some(Cached { generation, checked: Instant::now(), value: value.clone() });
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_is_fractional_used_percent_and_missing_is_unknown() {
        let v = snapshot(&json!({"five_hour":{"utilization":12.5,"resets_at":"2026-11-04T18:30:00+00:00"},"seven_day":null,"extra_usage":{"secret":"not forwarded"}})).unwrap();
        assert_eq!(v["windows"][0]["used_percent"], 12.5);
        assert_eq!(v["windows"].as_array().unwrap().len(), 1);
        assert!(!v.to_string().contains("secret"));
        for bad in [json!(-1),json!(101),json!("50")] {
            assert!(snapshot(&json!({"five_hour":{"utilization":bad}})).unwrap()["windows"][0]["used_percent"].is_null());
        }
        assert!(snapshot(&json!([])).is_err());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use super::super::Credentials;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use http_body_util::Full;
    use hyper::{Response, body::Bytes, service::service_fn};
    use hyper_util::rt::TokioIo;

    async fn signed_in(c: &Claude) {
        let mut state = c.state.lock().await;
        state.generation += 1;
        state.credentials = Some(Credentials { access:"usage-fixture-token".into(), refresh:"fixture-refresh".into(), expires:now()+3600, models:vec![] });
    }

    #[tokio::test]
    async fn cache_auth_errors_and_account_changes_are_isolated() -> Result<()> {
        let root = tempfile::tempdir()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let mut c = Claude::new(root.path(), root.path().join("profile"))?;
        Arc::get_mut(&mut c).unwrap().api = format!("http://{}",listener.local_addr()?);
        let hits = Arc::new(AtomicUsize::new(0));
        let mode = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (h,m,s,r) = (hits.clone(),mode.clone(),started.clone(),release.clone());
        let server = tokio::spawn(async move {
            loop {
                let (stream,_) = listener.accept().await.unwrap();
                let (h,m,s,r) = (h.clone(),m.clone(),s.clone(),r.clone());
                tokio::spawn(async move {
                    let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let (h,m,s,r) = (h.clone(),m.clone(),s.clone(),r.clone());
                        async move {
                            assert_eq!(req.method(), "GET");
                            assert_eq!(req.uri().path(), "/api/oauth/usage");
                            assert_eq!(req.headers()["authorization"], "Bearer usage-fixture-token");
                            h.fetch_add(1,Ordering::SeqCst);
                            let mode = m.load(Ordering::SeqCst);
                            if mode == 3 { s.notify_one(); r.notified().await; }
                            let body = if mode == 1 { "private-provider-error usage-fixture-token".into() }
                                else if mode == 2 { "not json".into() }
                                else { json!({"five_hour":{"utilization":12.5},"seven_day":{"utilization":100}}).to_string() };
                            Ok::<_,std::convert::Infallible>(Response::builder().status(if mode==1 {429}else{200}).body(Full::new(Bytes::from(body))).unwrap())
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream),service).await;
                });
            }
        });
        assert_eq!(c.usage().await["connected"],false);
        assert_eq!(hits.load(Ordering::SeqCst),0);
        signed_in(&c).await;
        let (a,b) = tokio::join!(c.usage(),c.usage());
        assert_eq!(a,b);
        assert_eq!(a["windows"][0]["used_percent"],12.5);
        assert_eq!(hits.load(Ordering::SeqCst),1);
        for value in [1,2] {
            mode.store(value,Ordering::SeqCst);
            c.usage.lock().await.as_mut().unwrap().checked = Instant::now()-Duration::from_secs(301);
            let failed = c.usage().await;
            assert!(failed["error"].is_string());
            assert_eq!(failed["windows"],json!([]));
            assert!(!failed.to_string().contains("usage-fixture-token"));
            assert!(!failed.to_string().contains("private-provider-error"));
            let count = hits.load(Ordering::SeqCst);
            assert_eq!(c.usage().await,failed);
            assert_eq!(hits.load(Ordering::SeqCst),count);
        }
        mode.store(3,Ordering::SeqCst);
        *c.usage.lock().await = None;
        let other = c.clone();
        let read = tokio::spawn(async move {other.usage().await});
        started.notified().await;
        c.disconnect().await?;
        release.notify_one();
        assert_eq!(read.await?["connected"],false);
        assert_eq!(c.usage().await["connected"],false);
        signed_in(&c).await;
        mode.store(0,Ordering::SeqCst);
        assert_eq!(c.usage().await["windows"][1]["used_percent"],100.0);
        server.abort();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires built DEMODEX_WEB and local Chrome/uv; disposable cached fixture only"]
    async fn claude_usage_browser() -> Result<()> {
        let root = tempfile::tempdir()?;
        let service = crate::Service::open(crate::Config {stt:None, data_dir:root.path().join("state"),vm_image:None,host_workspace:None,codex_home:None})?;
        let c = service.manager.claude.get().unwrap();
        signed_in(c).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}",listener.local_addr()?);
        let server = crate::http::Server::new(Some(crate::wormhole::Endpoint::new(service.clone(),"usage-browser-fixture".into(),vec![],vec![])),Some(std::env::var("DEMODEX_WEB")?.into()));
        let (stop,stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.serve_tcp(listener,async {let _=stopped.await;}));
        for scenario in ["success","weekly_only","partial","error","signedout"] {
            let mut value = snapshot(&json!({"five_hour":{"utilization":12.5,"resets_at":"2099-11-04T18:30:00+00:00"},"seven_day":{"utilization":100,"resets_at":"2099-11-10T18:30:00Z"}}))?;
            if scenario == "weekly_only" {value["windows"].as_array_mut().unwrap().remove(0);}
            if scenario == "partial" { value["windows"][1]["used_percent"]=Value::Null;value["windows"][0]["resets_at"]=json!("invalid-date"); }
            if scenario == "error" {value=json!({"connected":true,"windows":[],"error":"Claude usage request timed out"});}
            if scenario == "signedout" {c.disconnect().await?;}
            *c.usage.lock().await=Some(Cached {generation:c.state.lock().await.generation,checked:Instant::now(),value});
            let status=tokio::process::Command::new("uv").args(["run","tests/claude_usage.py",&origin,scenario]).status().await?;
            ensure!(status.success(),"Claude usage browser fixture failed: {scenario}");
        }
        let _=stop.send(());
        task.await??;
        Ok(())
    }
}
