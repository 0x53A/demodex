//! Background handles belong to an app-server connection, never an OS PID or
//! the session's currently selected execution target.
use crate::manager::{Live, Manager};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(crate) async fn list(live: &Live) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut cursor = Value::Null;
    let mut cursors = HashSet::new();
    // A broken or constantly changing upstream pagination must not hang a snapshot.
    for _ in 0..100 {
        let page = live.rpc.call("thread/backgroundTerminals/list", json!({
            "threadId":live.thread,"cursor":cursor,"limit":100
        })).await?;
        for row in page["data"].as_array().context("Invalid background terminal list")? {
            for field in ["processId", "itemId", "command", "cwd"] {
                ensure!(row[field].is_string(), "Invalid background terminal {field}");
            }
            if !rows.iter().any(|old: &Value| old["processId"] == row["processId"]) {
                rows.push(row.clone());
            }
        }
        cursor = page.get("nextCursor").context("Missing background terminal cursor")?.clone();
        if cursor.is_null() { return Ok(rows); }
        let next = cursor.as_str().context("Invalid background terminal cursor")?;
        ensure!(cursors.insert(next.to_owned()), "Repeated background terminal cursor");
    }
    anyhow::bail!("Background terminal list exceeded pagination limit")
}

impl Manager {
    pub async fn background_snapshot(&self, id: &str) -> Value {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let live = self.runtime(id).await?;
            let rows = list(&live).await?;
            ensure!(self.runtime(id).await?.generation == live.generation, "Codex connection changed; refresh background terminals");
            Ok::<_, anyhow::Error>(json!({"generation":live.generation,"data":rows}))
        }).await;
        match result {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => json!({"error":format!("{error:#}")}),
            Err(_) => json!({"error":"Background terminal listing timed out"}),
        }
    }

    pub async fn stop_background(&self, id: &str, generation: &str, processes: &[(String, String)]) -> Result<Value> {
        ensure!(!processes.is_empty() && processes.len() <= 10000, "Select background terminals to stop");
        let _guard = self.connecting.lock().await;
        let live = self.runtime(id).await?;
        ensure!(live.generation == generation, "These process handles belong to an old Codex connection. Refresh before stopping terminals.");
        let rows = list(&live).await?;
        // Validate the entire selection before sending any termination. A reused
        // process identifier must not terminate another command.
        for (process, item) in processes {
            if let Some(row) = rows.iter().find(|row| row["processId"] == *process) {
                ensure!(row["itemId"] == *item, "Background process identity changed; refresh before stopping it");
            }
        }
        let mut results = Vec::new();
        for (process, _) in processes {
            if !rows.iter().any(|row| row["processId"] == *process) {
                results.push(json!({"processId":process,"terminated":false}));
                continue;
            }
            // Route through the captured app-server; never resolve a current
            // executor, selected target, OS PID, or replacement connection here.
            let response = live.rpc.call("thread/backgroundTerminals/terminate", json!({
                "threadId":live.thread,"processId":process
            })).await;
            self.changed();
            let response = response?;
            ensure!(response["terminated"].is_boolean(), "Invalid terminal termination response");
            results.push(json!({"processId":process,"terminated":response["terminated"]}));
        }
        self.changed();
        Ok(json!({"results":results}))
    }
}
