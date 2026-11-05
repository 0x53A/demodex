//! Session-local Codex configuration; never edits the shared profile.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(crate) fn validate(limit: Option<u32>) -> Result<()> {
    ensure!(limit != Some(0), "Max concurrent subagents must be at least 1; leave blank for the Codex default");
    Ok(())
}

pub(crate) fn apply_config(params: &mut Value, record: &Value) -> Result<()> {
    if record.is_null() { return Ok(()); }
    let limit: Option<u32> = serde_json::from_value(record["limit"].clone())
        .context("Invalid saved subagent limit")?;
    validate(limit)?;
    if let Some(limit) = limit {
        params["config"]["agents.max_concurrent_threads_per_session"] = json!(limit);
    }
    Ok(())
}

impl crate::orchestrator::Orchestrator {
    pub(crate) async fn set_subagent_limit(&self, id: &str, limit: Option<u32>) -> Result<Value> {
        validate(limit)?;
        let _lifecycle = self.jobs.lock().await;
        let settings = self.manager.connecting.lock().await;
        ensure!(self.manager.store.uses_runtime(id)? || self.manager.store.host_sessions()?.iter().any(|s|s==id),
            "Subagent settings require a managed Codex session");
        self.require_prompt_change_idle(id).await?;
        let live = self.manager.runtime(id).await?;
        ensure!(live.subagents.lock().await.active() == 0, "Wait for active subagents before changing their limit");
        let key = format!("subagent-limit/{id}");
        let previous = self.manager.store.prompt_record(&key)?;
        let requested = json!({"limit":limit});
        if previous == requested || (previous.is_null() && limit.is_none()) { return Ok(previous); }
        self.unsubscribe_for_context(id).await?;
        self.manager.store.put_prompt_record(&key, &requested)?;
        self.manager.disconnect(id, "Reconnecting to apply subagent limit").await?;
        self.connect_session_locked(id, &settings, None).await?;
        self.manager.changed();
        Ok(requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn override_and_default_preserve_other_configuration() {
        let mut params = json!({"config":{"model_context_window":32000}});
        apply_config(&mut params, &json!({"limit":7})).unwrap();
        assert_eq!(params["config"]["agents.max_concurrent_threads_per_session"],7);
        assert_eq!(params["config"]["model_context_window"],32000);
        let mut defaults = json!({"config":{"model_context_window":32000}});
        let before=defaults.clone();
        apply_config(&mut defaults, &Value::Null).unwrap();
        apply_config(&mut defaults, &json!({"limit":null})).unwrap();
        assert_eq!(defaults,before);
        assert!(apply_config(&mut defaults,&json!({"limit":0})).is_err());
        assert!(apply_config(&mut defaults,&json!({"limit":-1})).is_err());
    }
}
