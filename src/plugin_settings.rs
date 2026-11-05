//! Per-thread plugin exclusions. Server defaults are snapshots, never live inheritance.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn disabled(record: &Value) -> Result<Vec<String>> {
    let list: Vec<String> = serde_json::from_value(record["disabled"].clone())
        .context("Plugin selection unavailable; connect the session and refresh")?;
    ensure!(list.len() <= 1024 && list.iter().all(|id| !id.is_empty() && id.len() <= 512), "Invalid plugin selection");
    Ok(list.into_iter().collect::<BTreeSet<_>>().into_iter().collect())
}

fn catalog(reply: &Value) -> Result<Vec<Value>> {
    let mut rows = BTreeMap::new();
    for marketplace in reply["marketplaces"].as_array().context("Plugin catalogue unavailable")? {
        for plugin in marketplace["plugins"].as_array().context("Invalid plugin catalogue")? {
            if plugin["installed"] != true { continue; }
            let id = plugin["id"].as_str().context("Plugin ID missing")?;
            let name = plugin["interface"]["displayName"].as_str()
                .or_else(||plugin["name"].as_str()).unwrap_or(id);
            rows.insert(id.to_owned(),json!({"id":id,"name":name,"enabled":plugin["enabled"],"marketplace":marketplace["name"]}));
        }
    }
    Ok(rows.into_values().collect())
}

impl crate::manager::Manager {
    pub(crate) fn observe_plugins(&self, id: &str, settings: &Value) -> Result<()> {
        if let Some(value) = settings.get("disabledPluginIds") {
            let record = json!({"disabled":value});
            disabled(&record)?;
            self.store.put_prompt_record(&format!("plugins-effective/{id}"), &record)?;
        }
        Ok(())
    }

    pub(crate) async fn apply_plugins(&self, id: &str, requested: &[String]) -> Result<()> {
        let live = self.runtime(id).await?;
        Self::require_idle(&live).await?;
        let mut updates = self.plugin_updates.subscribe();
        live.rpc.call("thread/settings/update",json!({"threadId":live.thread,"disabledPluginIds":requested})).await?;
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let (session,generation,value) = updates.recv().await?;
                if session == id && generation == live.generation { return Ok::<_,anyhow::Error>(value); }
            }
        }).await.context("Plugin change was acknowledged but not confirmed; refresh before another change")??;
        ensure!(self.runtime(id).await?.generation == live.generation, "Connection changed while applying plugins");
        ensure!(disabled(&accepted)? == requested, "Codex confirmed a different plugin selection; refresh");
        Ok(())
    }
}

impl crate::orchestrator::Orchestrator {
    pub(crate) async fn plugin_settings(&self, id: Option<&str>) -> Result<Value> {
        let reply = if let Some(id) = id {
            let session = self.manager.store.get(id)?;
            let cwds=session.targets.iter().map(|t|t.cwd.clone()).collect::<Vec<_>>();
            let params=json!({"cwds":cwds});
            if let Ok(live)=self.manager.runtime(id).await {live.rpc.call("plugin/installed",params).await?}
            else {self.runtime_rpc().await?.0.call("plugin/installed",params).await?}
        } else {self.runtime_rpc().await?.0.call("plugin/installed",json!({})).await?};
        let defaults = self.manager.store.prompt_record("plugins-defaults")?;
        let defaults = if defaults.is_null() {json!({"revision":0,"disabled":[]})} else {defaults};
        let (saved,current) = if let Some(id) = id {
            let saved = self.manager.store.prompt_record(&format!("plugins/{id}"))?;
            let observed = self.manager.store.prompt_record(&format!("plugins-effective/{id}"))?;
            let current = if self.manager.runtime(id).await.is_ok() || saved.is_null() {observed} else {saved.clone()};
            (saved,current)
        } else {(defaults.clone(),defaults.clone())};
        Ok(json!({"plugins":catalog(&reply)?,"errors":reply["marketplaceLoadErrors"],
            "revision":saved["revision"].as_u64().unwrap_or(0),"disabled":current["disabled"],
            "defaults":defaults["disabled"],"customized":!saved.is_null(),
            "connected":if let Some(id)=id {self.manager.runtime(id).await.is_ok()}else{true}}))
    }

    pub(crate) async fn save_plugin_settings(&self, id: Option<&str>, expected_revision: u64,
        expected_disabled: &[String], requested: &[String]) -> Result<Value> {
        let _lifecycle = self.jobs.lock().await;
        let _settings = self.manager.connecting.lock().await;
        if let Some(id) = id {
            ensure!(!self.manager.store.get(id)?.archived, "Restore the session before changing plugins");
            self.require_prompt_change_idle(id).await?;
        }
        let expected_disabled = disabled(&json!({"disabled":expected_disabled}))?;
        let view = self.plugin_settings(id).await?;
        ensure!(view["revision"] == expected_revision && disabled(&view)? == expected_disabled,
            "Plugin settings changed; refresh before saving");
        let requested = disabled(&json!({"disabled":requested}))?;
        let known: BTreeSet<_> = view["plugins"].as_array().context("Plugin catalogue unavailable")?
            .iter().filter_map(|p|p["id"].as_str()).collect();
        ensure!(requested.iter().all(|p|known.contains(p.as_str()) || expected_disabled.contains(p)),
            "Plugin catalogue changed; refresh before saving");
        if let Some(id) = id { if requested != expected_disabled { self.manager.apply_plugins(id,&requested).await?; } }
        let revision = expected_revision.checked_add(1).context("Plugin revision exhausted")?;
        let key = id.map(|id|format!("plugins/{id}")).unwrap_or_else(||"plugins-defaults".into());
        self.manager.store.put_prompt_record(&key,&json!({"revision":revision,"disabled":requested}))?;
        self.manager.changed();
        Ok(json!({"revision":revision}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_installed_plugins_without_a_name_allowlist() {
        let rows=catalog(&json!({"marketplaces":[{"name":"new-market","plugins":[
            {"id":"future@new-market","name":"Future","enabled":true,"installed":true},
            {"id":"uninstalled@new-market","installed":false}]}]})).unwrap();
        assert_eq!(rows.len(),1); assert_eq!(rows[0]["id"],"future@new-market");
        assert!(catalog(&json!({})).is_err());
    }
}
