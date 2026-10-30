//! Per-session context overrides. Catalogue limits are capabilities, not prices.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

pub(crate) const MIN_WINDOW: i64 = 16_000;

fn limits(record: &Value, configured: Option<i64>) -> Result<Value> {
    let maximum = record["max_context_window"].as_i64()
        .or_else(|| record["context_window"].as_i64())
        .filter(|n| *n >= MIN_WINDOW).context("Context window limits unavailable")?;
    let default = configured.or_else(|| record["context_window"].as_i64())
        .unwrap_or(maximum).min(maximum);
    ensure!(default >= MIN_WINDOW, "Invalid default context window");
    Ok(json!({"default":default,"min":MIN_WINDOW,"max":maximum}))
}

pub(crate) fn validate(catalog: &Value, model: &str, window: Option<i64>) -> Result<i64> {
    let row = catalog["data"].as_array().context("Missing model catalogue")?
        .iter().find(|m| m["model"] == model).context("Model unavailable")?;
    let bounds = &row["context_window"];
    let min = bounds["min"].as_i64().context("Context window limits unavailable")?;
    let max = bounds["max"].as_i64().context("Context window limits unavailable")?;
    let window = window.or_else(|| bounds["default"].as_i64()).context("Default context window unavailable")?;
    ensure!((min..=max).contains(&window), "Context window must be between {min} and {max} tokens for {model}");
    Ok(window)
}

async fn enrich(catalog: &mut Value, profile: &Path, config: &Value) -> Result<()> {
    let custom = config["model_catalog_json"].as_str();
    let path = custom.map(std::path::PathBuf::from).unwrap_or_else(|| profile.join("models_cache.json"));
    ensure!(path.is_absolute(), "Model catalogue path must be absolute");
    let metadata = tokio::fs::metadata(&path).await.context("Context window catalogue unavailable")?;
    ensure!(metadata.is_file() && metadata.len() <= 32 * 1024 * 1024, "Invalid context window catalogue");
    if custom.is_none() {
        ensure!(metadata.modified()?.elapsed().unwrap_or_default() <= Duration::from_secs(600), "Context window catalogue is stale; refresh models");
    }
    let source: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    let records = source["models"].as_array().context("Invalid model catalogue")?;
    for row in catalog["data"].as_array_mut().context("Missing model list")? {
        if let Some(record) = records.iter().find(|r| r["slug"] == row["model"]) {
            match limits(record, config["model_context_window"].as_i64()) {
                Ok(bounds) => row["context_window"] = bounds,
                Err(error) => row["context_window_error"] = json!(error.to_string()),
            }
        }
    }
    Ok(())
}

impl crate::orchestrator::Orchestrator {
    pub(crate) async fn context_model_catalog(&self, id: Option<&str>) -> Result<Value> {
        let cwd = if let Some(id) = id {
            let session = self.manager.store.get(id)?;
            let managed = self.manager.store.uses_runtime(id)?
                || self.manager.store.host_sessions()?.iter().any(|s| s == id);
            if !managed { return self.manager.model_catalog(id).await; }
            if session.targets.len() == 1 { Some(session.targets[0].cwd.clone()) } else { None }
        } else {
            self.host_workspace.as_ref().map(|p|p.to_string_lossy().into_owned())
        };
        self.context_catalog_for_cwd(cwd).await
    }

    pub(crate) async fn context_catalog_for_cwd(&self, cwd: Option<String>) -> Result<Value> {
        let rpc = self.runtime_rpc().await?.0;
        let mut catalog = crate::controls::runtime_model_catalog(&rpc).await?;
        let result = async {
                let config = rpc.call("config/read", json!({"cwd":cwd,"includeLayers":false})).await?;
                catalog["defaults"] = crate::controls::runtime_defaults(&config["config"], catalog["data"].as_array().context("Missing model catalogue")?);
                enrich(&mut catalog, &self.prompt_profile(), &config["config"]).await
        }.await;
        if let Err(error) = result { catalog["context_window_error"] = json!(format!("{error:#}")); }
        Ok(catalog)
    }

    // Re-resolve on reconnect so Default follows the profile and current model.
    pub(crate) async fn prepare_context_window(&self, id: &str) -> Result<()> {
        let key = format!("context/{id}");
        let mut setting = self.manager.store.prompt_record(&key)?;
        if setting.is_null() { return Ok(()); }
        let catalog = self.context_model_catalog(Some(id)).await?;
        let saved = self.manager.store.model_settings(id)?;
        let model = saved["selection"]["model"].as_str().or_else(|| saved["effective"]["model"].as_str())
            .or_else(|| catalog["defaults"]["model"].as_str()).context("Model unavailable")?;
        setting["resolved"] = json!(validate(&catalog, model, setting["window"].as_i64())?);
        self.manager.store.put_prompt_record(&key, &setting)
    }

    pub(crate) async fn set_context_window(&self, id: &str, window: Option<i64>) -> Result<Value> {
        let _lifecycle = self.jobs.lock().await;
        let settings = self.manager.connecting.lock().await;
        ensure!(self.manager.store.uses_runtime(id)? || self.manager.store.host_sessions()?.iter().any(|s|s==id),
            "Context settings require a managed Codex session");
        self.require_prompt_change_idle(id).await?;
        let catalog = self.context_model_catalog(Some(id)).await?;
        let saved = self.manager.store.model_settings(id)?;
        let model = saved["effective"]["model"].as_str().context("Current model unavailable")?;
        let resolved = validate(&catalog, model, window)?;
        let key = format!("context/{id}");
        let previous = self.manager.store.prompt_record(&key)?;
        let requested = json!({"window":window,"resolved":resolved});
        if previous == requested || (previous.is_null() && window.is_none()) { return Ok(previous); }
        self.unsubscribe_for_context(id).await?;
        self.manager.store.put_prompt_record(&key, &requested)?;
        self.manager.disconnect(id, "Reconnecting to apply context window").await?;
        self.connect_session_locked(id, &settings, None).await?;
        self.manager.changed();
        Ok(requested)
    }

    pub(crate) async fn require_context_history(&self, id: &str) -> Result<()> {
        let live = self.manager.runtime(id).await?;
        let thread = live.rpc.call("thread/read", json!({"threadId":live.thread,"includeTurns":false})).await?;
        let path = thread["thread"]["path"].as_str().context("Session has no saved history; send a message before changing its context window")?;
        ensure!(tokio::fs::metadata(path).await.is_ok_and(|m|m.is_file()), "Session has no saved history; send a message before changing its context window");
        Ok(())
    }

    pub(crate) async fn unsubscribe_for_context(&self, id: &str) -> Result<()> {
        self.require_context_history(id).await?;
        let live = self.manager.runtime(id).await?;
        let reply = live.rpc.call("thread/unsubscribe", json!({"threadId":live.thread})).await?;
        ensure!(matches!(reply["status"].as_str(), Some("unsubscribed"|"notSubscribed"|"notLoaded")), "Codex did not confirm unsubscribing; context change is unconfirmed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogue_bounds_and_profile_default_are_distinct() {
        let record = json!({"context_window":272000,"max_context_window":872000});
        assert_eq!(limits(&record, None).unwrap()["default"], 272000);
        assert_eq!(limits(&record, Some(1000000)).unwrap()["default"], 872000);
        let catalog = json!({"data":[{"model":"fixture","context_window":limits(&record,None).unwrap()}]});
        assert_eq!(validate(&catalog,"fixture",None).unwrap(),272000);
        assert_eq!(validate(&catalog,"fixture",Some(872000)).unwrap(),872000);
        assert!(validate(&catalog,"fixture",Some(1000000)).is_err());
        assert!(validate(&catalog,"fixture",Some(0)).is_err());
        assert!(validate(&catalog,"missing",Some(500000)).is_err());
    }
}
