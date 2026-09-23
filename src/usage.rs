//! Read-only account quota and last-reported thread context snapshots.
use crate::rpc::Rpc;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn weekly(response: &Value) -> Value {
    let mut windows = Vec::new();
    let mut add = |id: &str, bucket: &Value| {
        for name in ["primary", "secondary"] {
            let window = &bucket[name];
            if window["windowDurationMins"].as_i64() != Some(7 * 24 * 60) {
                continue;
            }
            let Some(used) = window["usedPercent"]
                .as_i64()
                .filter(|v| (0..=100).contains(v))
            else {
                continue;
            };
            windows.push(
                json!({"id":id,"name":bucket["limitName"].as_str().unwrap_or(id),
                "used_percent":used,"resets_at":window["resetsAt"]}),
            );
        }
    };
    if let Some(buckets) = response["rateLimitsByLimitId"]
        .as_object()
        .filter(|m| !m.is_empty())
    {
        if let Some(bucket) = buckets.get("codex") {
            add("codex", bucket);
        }
        for (id, bucket) in buckets {
            if id != "codex" {
                add(id, bucket);
            }
        }
    } else {
        add(
            response["rateLimits"]["limitId"]
                .as_str()
                .unwrap_or("codex"),
            &response["rateLimits"],
        );
    }
    json!({"windows":windows,"checked_at":now(),"error":if windows.is_empty(){Some("No weekly usage window reported by Codex")}else{None}})
}

struct Cached {
    rpc: Weak<Rpc>,
    account: Value,
    checked: Instant,
    value: Value,
}
#[derive(Default)]
pub struct RateLimitsCache(Mutex<Option<Cached>>);
impl RateLimitsCache {
    pub async fn read(&self, rpc: Arc<Rpc>, account: &Value) -> Value {
        let mut cache = self.0.lock().await;
        if account.is_null() {
            *cache = None;
            return json!({"windows":[],"error":"Sign in to see weekly usage"});
        }
        if let Some(entry) = cache.as_ref()
            && entry.account == *account
            && entry.checked.elapsed() < Duration::from_secs(60)
            && entry
                .rpc
                .upgrade()
                .is_some_and(|old| Arc::ptr_eq(&old, &rpc))
        {
            return entry.value.clone();
        }
        let value = match tokio::time::timeout(
            Duration::from_secs(5),
            rpc.call("account/rateLimits/read", json!({})),
        )
        .await
        {
            Ok(Ok(response)) => weekly(&response),
            Ok(Err(error)) => {
                json!({"windows":[],"checked_at":now(),"error":format!("Weekly usage unavailable: {error:#}")})
            }
            Err(_) => {
                json!({"windows":[],"checked_at":now(),"error":"Weekly usage request timed out"})
            }
        };
        *cache = Some(Cached {
            rpc: Arc::downgrade(&rpc),
            account: account.clone(),
            checked: Instant::now(),
            value: value.clone(),
        });
        value
    }
}

pub fn context(usage: &Value) -> Value {
    let used = usage["last"]["totalTokens"].as_i64().filter(|n| *n >= 0);
    let window = usage["modelContextWindow"].as_i64().filter(|n| *n > 0);
    json!({"used_tokens":used,"window_tokens":window,"reported_at":now()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn account_cache_deduplicates_reads_and_does_not_leak_across_accounts()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let reply = match request["method"].as_str().unwrap() {
                    "initialized" => continue,
                    "initialize" => json!({"id":request["id"],"result":{}}),
                    "account/rateLimits/read" => {
                        let count = observed.fetch_add(1, Ordering::SeqCst);
                        if count == 2 {
                            json!({"id":request["id"],"error":{"message":"unavailable"}})
                        } else {
                            json!({"id":request["id"],"result":{"rateLimits":{"secondary":{"usedPercent":20,"windowDurationMins":10080}}}})
                        }
                    }
                    method => panic!("unexpected request: {method}"),
                };
                socket
                    .send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let (rpc, _events) = Rpc::connect(&endpoint).await?;
        let rpc = Arc::new(rpc);
        let cache = RateLimitsCache::default();
        let first = json!({"id":"one"});
        let (a, b) = tokio::join!(
            cache.read(rpc.clone(), &first),
            cache.read(rpc.clone(), &first)
        );
        assert_eq!(a, b);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cache.read(rpc.clone(), &json!({"id":"two"})).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let failed = cache.read(rpc.clone(), &json!({"id":"three"})).await;
        assert!(failed["windows"].as_array().unwrap().is_empty());
        assert!(failed["error"].is_string());
        let signed_out = cache.read(rpc.clone(), &Value::Null).await;
        assert!(signed_out["windows"].as_array().unwrap().is_empty());
        rpc.close();
        fake.abort();
        Ok(())
    }

    #[test]
    fn context_history_migrates_and_latest_snapshot_survives_restart() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("usage.db");
        let id;
        {
            let store = crate::store::Store::open(&path)?;
            id = store
                .create("test", "ws://localhost:1", &[], Some("thread"))?
                .id;
            for tokens in [50000, 10000] {
                store.event(&id,&json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thread","tokenUsage":{"last":{"totalTokens":tokens},"total":{"totalTokens":800000},"modelContextWindow":200000}}}))?;
            }
        }
        {
            let store = crate::store::Store::open(&path)?;
            assert_eq!(store.get(&id)?.context_usage["used_tokens"], 10000);
            store.context_usage(
                &id,
                &json!({"last":{"totalTokens":0},"modelContextWindow":200000}),
            )?;
        }
        let store = crate::store::Store::open(&path)?;
        assert_eq!(store.get(&id)?.context_usage["used_tokens"], 0);
        assert_eq!(store.events(&id, 0)?.len(), 2);
        Ok(())
    }

    #[test]
    fn weekly_selects_duration_not_primary_position_and_avoids_duplicate_legacy_bucket() {
        let data = json!({"rateLimits":{"secondary":{"usedPercent":99,"windowDurationMins":10080}},
            "rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":17,"windowDurationMins":10080,"resetsAt":1900000000},"secondary":{"usedPercent":88,"windowDurationMins":300}}}});
        let result = weekly(&data);
        assert_eq!(result["windows"].as_array().unwrap().len(), 1);
        assert_eq!(result["windows"][0]["used_percent"], 17);
        assert_eq!(result["windows"][0]["resets_at"], 1900000000);
        let multiple = weekly(&json!({"rateLimitsByLimitId":{
            "base_model_inference":{"primary":{"usedPercent":99,"windowDurationMins":10080}},
            "codex":{"primary":{"usedPercent":17,"windowDurationMins":10080}}}}));
        assert_eq!(multiple["windows"][0]["id"], "codex");
        assert_eq!(multiple["windows"].as_array().unwrap().len(), 2);
        assert!(
            weekly(&json!({"rateLimits":{"secondary":{"usedPercent":0}}}))["windows"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            weekly(
                &json!({"rateLimits":{"secondary":{"usedPercent":0,"windowDurationMins":10080}}})
            )["windows"][0]["used_percent"],
            0
        );
    }
    #[test]
    fn context_uses_last_not_cumulative_tokens_and_can_decrease_after_compaction() {
        let first = context(
            &json!({"last":{"totalTokens":60000},"total":{"totalTokens":900000},"modelContextWindow":200000}),
        );
        assert_eq!(first["used_tokens"], 60000);
        assert_eq!(
            context(&json!({"last":{"totalTokens":10000},"modelContextWindow":200000}))["used_tokens"],
            10000
        );
        assert!(context(&json!({}))["used_tokens"].is_null());
        assert!(context(&json!({"modelContextWindow":0}))["window_tokens"].is_null());
    }
}
