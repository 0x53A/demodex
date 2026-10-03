//! Operator-owned prompt layers. Codex profiles and instruction files are read-only.
use anyhow::{Context, Result, ensure};
use demodex_protocol::PromptSettings;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};
use tokio::io::AsyncReadExt;

pub fn integration_default() -> String {
    format!(
        "{}\n\n{}",
        crate::session_context::INSTRUCTIONS,
        crate::ssh::AGENT_INSTRUCTIONS
    )
}
pub fn fingerprint(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn validate(settings: &PromptSettings) -> Result<()> {
    ensure!(settings.models.len() <= 100, "At most 100 model overrides");
    ensure!(
        serde_json::to_vec(settings)?.len() <= 4 * 1024 * 1024,
        "Prompt settings exceed 4 MiB"
    );
    for (name, entry) in &settings.models {
        ensure!(
            !name.is_empty() && name.len() <= 200 && !name.contains('\0'),
            "Invalid model identifier"
        );
        crate::orchestrator::validate_prompt(Some(&entry.text))?;
        ensure!(
            entry.reviewed_default.len() <= 128,
            "Invalid baseline fingerprint"
        );
    }
    crate::orchestrator::validate_prompt(Some(&settings.append))?;
    if let Some(entry) = &settings.integration {
        crate::orchestrator::validate_prompt(Some(&entry.text))?;
        ensure!(
            entry.reviewed_default.len() <= 128,
            "Invalid baseline fingerprint"
        );
    }
    Ok(())
}

pub async fn read_text(path: &Path) -> Result<Option<String>> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => ensure!(
            meta.is_file() && meta.len() <= 262144,
            "Instruction source must be a regular file of at most 256 KiB"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata().await?.is_file(),
        "Instruction source is not a regular file"
    );
    let mut bytes = Vec::new();
    (&mut file).take(262145).read_to_end(&mut bytes).await?;
    ensure!(bytes.len() <= 262144, "Instruction file exceeds 256 KiB");
    Ok(Some(
        String::from_utf8(bytes).context("Instruction file is not UTF-8")?,
    ))
}

pub async fn catalog(rpc: &crate::rpc::Rpc, profile: &Path, cwd: Option<&str>) -> Result<Value> {
    let config = rpc
        .call("config/read", json!({"cwd":cwd,"includeLayers":false}))
        .await?["config"]
        .clone();
    let mut models = Vec::new();
    let mut cursor = Value::Null;
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = rpc
            .call(
                "model/list",
                json!({"cursor":cursor,"limit":100,"includeHidden":false}),
            )
            .await?;
        models.extend(
            page["data"]
                .as_array()
                .context("Missing model list")?
                .iter()
                .cloned(),
        );
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
        ensure!(
            seen.len() < 100 && seen.insert(cursor.to_string()),
            "Model pagination did not finish"
        );
    }
    let cache = profile.join("models_cache.json");
    let (metadata, source) = if cache.is_file() {
        (
            serde_json::from_slice::<Value>(&tokio::fs::read(&cache).await?)?,
            cache.display().to_string(),
        )
    } else {
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new("codex")
                .args(["debug", "models", "--bundled"])
                .env("CODEX_HOME", profile)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        ensure!(output.status.success(), "Codex model catalogue unavailable");
        (
            serde_json::from_slice(&output.stdout)?,
            "Codex bundled catalogue".into(),
        )
    };
    let records = metadata["models"]
        .as_array()
        .context("Invalid model catalogue")?;
    let configured = if let Some(path) = config["model_instructions_file"].as_str() {
        ensure!(
            Path::new(path).is_absolute(),
            "Codex returned an unresolved instruction path"
        );
        Some((
            read_text(Path::new(path))
                .await?
                .context("Configured prompt file is missing")?,
            path.to_string(),
        ))
    } else {
        config["instructions"]
            .as_str()
            .map(|s| (s.to_owned(), "Codex profile instructions".into()))
    };
    let mut defaults = BTreeMap::new();
    for model in &models {
        let slug = model["model"].as_str().context("Model has no identifier")?;
        let entry = records.iter().find(|m| m["slug"] == slug);
        let text = entry.and_then(|m| {
            m["model_messages"]["instructions_template"]
                .as_str()
                .or_else(|| m["base_instructions"].as_str())
        });
        let mut value = if let Some(text) = text {
            json!({"text":text,"fingerprint":fingerprint(text),"source":source,"effective_text":configured.as_ref().map(|v|v.0.as_str()).unwrap_or(text),"effective_source":configured.as_ref().map(|v|v.1.as_str()).unwrap_or(&source)})
        } else {
            json!({"error":"This model does not expose a default prompt in the runtime catalogue"})
        };
        // A profile prompt is usable even when this model has no catalogue default.
        if let Some((text, source)) = &configured {
            value["effective_text"] = json!(text);
            value["effective_source"] = json!(source);
        }
        defaults.insert(slug.to_owned(), value);
    }
    let default_model = config["model"]
        .as_str()
        .or_else(|| {
            models
                .iter()
                .find(|m| m["isDefault"] == true)
                .and_then(|m| m["model"].as_str())
        })
        .context("Codex did not identify a default model")?;
    Ok(json!({"models":models,"defaults":defaults,"default_model":default_model,"config":config}))
}

pub fn compose(
    settings: &PromptSettings,
    model: &str,
    default: &Value,
    developer: &str,
) -> Result<(Option<String>, String)> {
    let base = if let Some(replacement) = settings.models.get(model) {
        Some(replacement.text.clone())
    } else if !settings.append.is_empty() {
        Some(
            default["effective_text"]
                .as_str()
                .context("Cannot append: current model prompt is unavailable")?
                .to_owned(),
        )
    } else {
        None
    };
    let base = base.map(|mut base| {
        if !settings.append.is_empty() {
            base.push_str("\n\n");
            base.push_str(&settings.append);
        }
        base
    });
    crate::orchestrator::validate_prompt(base.as_deref())?;
    let integration = settings
        .integration
        .as_ref()
        .map(|v| v.text.clone())
        .unwrap_or_else(integration_default);
    Ok((base, format!("{developer}\n\n{integration}")))
}

impl crate::orchestrator::Orchestrator {
    fn prompt_profile(&self) -> std::path::PathBuf {
        self.codex_home
            .clone()
            .unwrap_or_else(|| self.root.join("runtime/home"))
    }
    pub(crate) async fn prompt_settings_view(&self) -> Result<Value> {
        let (revision, settings) = self.manager.store.prompt_settings()?;
        let (rpc, _) = self.runtime_rpc().await?;
        let mut value = catalog(
            &rpc,
            &self.prompt_profile(),
            self.host_workspace.as_deref().and_then(Path::to_str),
        )
        .await?;
        value["revision"] = json!(revision);
        value["settings"] = json!(settings);
        let default = integration_default();
        value["integration_default"] = json!({"text":default,"fingerprint":fingerprint(&default)});
        // This is a comparison source, not a claim that the session has loaded it.
        value["global_switch_supported"] = json!(false);
        value.as_object_mut().unwrap().remove("config");
        Ok(value)
    }
    pub(crate) fn session_prompt_view(&self, id: &str) -> Result<Value> {
        self.manager.store.get(id)?;
        let (revision, settings) = self.manager.store.prompt_settings()?;
        let applied = self.manager.store.prompt_record(&format!("applied/{id}"))?;
        let policy = self.manager.store.prompt_record(&format!("policy/{id}"))?;
        Ok(
            json!({"revision":revision,"applied":applied,"include_project":policy["include_project"].as_bool().unwrap_or(settings.include_project),"project_override":policy["include_project"],"legacy":self.manager.store.prompt(id)?.is_some(),"pending":applied["revision"].as_u64().unwrap_or(0)!=revision}),
        )
    }
    pub(crate) async fn prepare_session_prompt(&self, id: &str) -> Result<()> {
        let policy = self.manager.store.prompt_record(&format!("policy/{id}"))?;
        if let Some(prepared) = self.resolve_session_prompt(id, &policy, false).await? {
            self.manager.store.put_prompt_record(&format!("prepared/{id}"), &prepared)?;
        }
        Ok(())
    }
    async fn resolve_session_prompt(&self, id: &str, policy: &Value, replace_legacy: bool) -> Result<Option<Value>> {
        let (revision, settings) = self.manager.store.prompt_settings()?;
        if revision == 0 && policy.is_null() {
            return Ok(None);
        }
        let legacy = self.manager.store.prompt(id)?.is_some();
        if legacy && !replace_legacy {
            return Ok(None);
        }
        let session = self.manager.store.get(id)?;
        let (rpc, _) = self.runtime_rpc().await?;
        if let Some(thread) = &session.thread_id {
            let state = rpc
                .call(
                    "thread/read",
                    json!({"threadId":thread,"includeTurns":false}),
                )
                .await?;
            ensure!(
                matches!(
                    state["thread"]["status"]["type"].as_str(),
                    Some("idle" | "notLoaded")
                ),
                "Prompt settings require an idle thread before reconnecting"
            );
        }
        let cwd = session.targets.first().map(|t| t.cwd.as_str());
        let data = catalog(&rpc, &self.prompt_profile(), cwd).await?;
        let saved = self.manager.store.model_settings(id)?;
        let model = saved["selection"]["model"]
            .as_str()
            .or_else(|| saved["effective"]["model"].as_str())
            .or_else(|| data["default_model"].as_str())
            .context("Missing selected model")?;
        let (base, developer) = compose(
            &settings,
            model,
            &data["defaults"][model],
            data["config"]["developer_instructions"]
                .as_str()
                .unwrap_or(""),
        )?;
        let base = base.or_else(|| data["defaults"][model]["effective_text"].as_str().map(str::to_owned));
        // New/inherited sessions can let Codex resolve its own default. Resuming
        // after an explicit base needs replacement text to clear the old override.
        ensure!(base.is_some() || (!legacy && !self.manager.store.prompt_record(&format!("applied/{id}"))?["base"].is_string()),
            "Current model prompt unavailable; cannot replace the previous base instructions");
        let include_project = policy["include_project"]
            .as_bool()
            .unwrap_or(settings.include_project);
        Ok(Some(json!({"revision":revision,"model":model,"base":base,"developer":developer,"include_project":include_project,"project_limit":data["config"]["project_doc_max_bytes"].as_u64().unwrap_or(32768)})))
    }
    pub(crate) async fn require_prompt_change_idle(&self, id: &str) -> Result<()> {
        let live = self.manager.runtime(id).await?;
        crate::manager::Manager::require_idle(&live).await?;
        ensure!(
            self.manager
                .store
                .pending(id)?
                .iter()
                .all(|p| !matches!(p.state.as_str(), "pending" | "responding" | "delivered")),
            "Resolve pending decisions before changing prompts"
        );
        ensure!(
            self.manager
                .queued(id)
                .await?
                .as_array()
                .is_some_and(Vec::is_empty),
            "Clear queued messages before changing prompts"
        );
        let controls = self.manager.control_snapshot(id).await?;
        ensure!(
            controls["goalError"].is_null() && controls["goal"]["status"] != "active",
            "Pause the goal before changing prompts; goal state must be available"
        );
        let background = self.manager.background_snapshot(id).await;
        ensure!(
            background["error"].is_null()
                && background["data"].as_array().is_some_and(Vec::is_empty),
            "Stop background terminals before reconnecting to apply prompts"
        );
        Ok(())
    }
    pub(crate) async fn apply_session_prompt_locked(
        &self,
        id: &str,
        include_project: Option<bool>,
        settings: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<Value> {
        ensure!(
            self.manager.store.uses_runtime(id)?
                || self.manager.store.host_sessions()?.iter().any(|s| s == id),
            "Prompt settings require a managed Codex session"
        );
        self.require_prompt_change_idle(id).await?;
        // Validate composition before releasing the live thread or removing its
        // legacy override. A missing default must leave the working session intact.
        let policy = json!({"include_project":include_project});
        let prepared = self.resolve_session_prompt(id, &policy, true).await?
            .context("Prompt application did not resolve instructions")?;
        let live = self.manager.runtime(id).await?;
        let unsubscribed = live
            .rpc
            .call("thread/unsubscribe", json!({"threadId":live.thread}))
            .await?;
        ensure!(
            matches!(
                unsubscribed["status"].as_str(),
                Some("unsubscribed" | "notSubscribed" | "notLoaded")
            ),
            "Codex did not confirm unsubscribing; prompt application is unconfirmed"
        );
        self.manager.store.put_prompt_record(
            &format!("policy/{id}"),
            &policy,
        )?;
        // Explicitly replace the old all-in-one override when the operator applies layers.
        self.manager.store.remove_legacy_prompt(id)?;
        self.manager
            .disconnect(id, "Reconnecting to apply prompt settings")
            .await?;
        self.connect_session_locked(id, settings, Some(&prepared)).await?;
        self.manager.changed();
        self.session_prompt_view(id)
    }
    pub(crate) async fn instruction_files(
        &self,
        target: Option<demodex_protocol::Selection>,
    ) -> Result<Value> {
        let (rpc, _) = self.runtime_rpc().await?;
        let config = rpc
            .call(
                "config/read",
                json!({"cwd":self.host_workspace,"includeLayers":false}),
            )
            .await?["config"]
            .clone();
        let mut files = Vec::new();
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = self.prompt_profile().join(name);
            if let Some(text) = read_text(&path).await?.filter(|s| !s.trim().is_empty()) {
                files.push(json!({"path":path,"scope":"global","text":text,"included":true}));
                break;
            }
        }
        if let Some(selection) = target {
            let targets = self
                .resolve_targets(std::slice::from_ref(&selection))
                .await?;
            let url = &targets.first().context("Missing instruction executor")?.url;
            files.extend(project_files(url, &selection.cwd, &config).await?);
        }
        Ok(
            json!({"files":files,"note":"Current files discovered from the selected directory. Existing conversation history may contain earlier instructions. Global instructions are controlled by the Codex runtime."}),
        )
    }
}

async fn project_files(url: &str, cwd: &str, config: &Value) -> Result<Vec<Value>> {
    use base64::Engine;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
    async fn call(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        socket
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        loop {
            if let Message::Text(text) = socket.next().await.context("Executor disconnected")?? {
                let v: Value = serde_json::from_str(&text)?;
                if v["id"] == id {
                    ensure!(
                        v["error"].is_null(),
                        "Executor file inspection failed: {}",
                        v["error"]
                    );
                    return Ok(v["result"].clone());
                }
            }
        }
    }
    call(
        &mut socket,
        1,
        "initialize",
        json!({"clientName":"demodex-instruction-preview"}),
    )
    .await?;
    socket
        .send(Message::Text(
            json!({"method":"initialized","params":{}})
                .to_string()
                .into(),
        ))
        .await?;
    let mut id = 2;
    let markers: Vec<&str> = config["project_root_markers"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_else(|| vec![".git"]);
    let mut dirs = Vec::new();
    let mut root_found = false;
    for path in Path::new(cwd).ancestors().take(100) {
        dirs.push(path.to_owned());
        for marker in &markers {
            ensure!(
                !marker.contains('/') && !marker.contains('\0'),
                "Unsupported project root marker"
            );
            id += 1;
            let probe=call(&mut socket,id,"fs/getMetadata",json!({"path":crate::ssh::sftp::path_uri(path.join(marker).to_str().context("Invalid path")?)})).await;
            if probe.is_ok() {
                root_found = true;
                break;
            }
            // Absence is normal; other failures must not become a host fallback.
            if let Err(e) = probe {
                let text = e.to_string();
                ensure!(
                    text.contains("status 2")
                        || text.contains("not found")
                        || text.contains("No such file")
                        || text.contains("ENOENT"),
                    "{text}"
                );
            }
        }
        if root_found || markers.is_empty() {
            break;
        }
    }
    if !root_found {
        dirs.truncate(1);
    }
    dirs.reverse();
    let mut names = vec!["AGENTS.override.md", "AGENTS.md"];
    for name in config["project_doc_fallback_filenames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !name.is_empty() && !name.contains(['/', '\\', '\0']) && !names.contains(&name) {
            names.push(name);
        }
    }
    let mut files = Vec::new();
    let mut preview_bytes = 0usize;
    let mut remaining = config["project_doc_max_bytes"]
        .as_u64()
        .unwrap_or(32768)
        .min(262144);
    for directory in dirs {
        for name in &names {
            let path = directory.join(name);
            let path = path.to_str().context("Invalid path")?;
            id += 1;
            let params = json!({"path":crate::ssh::sftp::path_uri(path)});
            let metadata = match call(&mut socket, id, "fs/getMetadata", params.clone()).await {
                Ok(v) => v,
                Err(e) => {
                    let text = e.to_string();
                    if text.contains("status 2")
                        || text.contains("not found")
                        || text.contains("No such file")
                        || text.contains("ENOENT")
                    {
                        continue;
                    }
                    return Err(e);
                }
            };
            if metadata["isFile"] != true {
                continue;
            }
            ensure!(
                metadata["size"].as_u64().is_some_and(|n| n <= 262144),
                "Instruction file exceeds preview limit or size is unavailable: {path}"
            );
            id += 1;
            let data = call(&mut socket, id, "fs/readFile", params).await?;
            let encoded = data["dataBase64"].as_str().context("Missing file data")?;
            ensure!(
                encoded.len() <= 262144usize.div_ceil(3) * 4,
                "Instruction file grew beyond preview limit"
            );
            let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
            ensure!(
                bytes.len() <= 262144,
                "Instruction file exceeds preview limit"
            );
            let text = String::from_utf8(bytes)?;
            preview_bytes += text.len();
            ensure!(
                files.len() < 32 && preview_bytes <= 1024 * 1024,
                "Instruction preview exceeds 32 files or 1 MiB"
            );
            let used = remaining.min(text.len() as u64);
            remaining -= used;
            files.push(json!({"path":path,"scope":"project","text":text,"included_bytes":used,"truncated":used<(text.len() as u64)}));
            break;
        }
    }
    socket.close(None).await?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layers_append_after_replacements_and_preserve_empty_overrides() -> Result<()> {
        let mut settings = PromptSettings {
            append: "Always appended".into(),
            ..Default::default()
        };
        let default = json!({"effective_text":"Profile default"});
        assert_eq!(
            compose(&settings, "one", &default, "Operator")?
                .0
                .as_deref(),
            Some("Profile default\n\nAlways appended")
        );
        settings.models.insert(
            "one".into(),
            demodex_protocol::ModelPromptOverride {
                text: String::new(),
                reviewed_default: String::new(),
            },
        );
        assert_eq!(
            compose(&settings, "one", &default, "")?.0.as_deref(),
            Some("\n\nAlways appended")
        );
        assert!(compose(&settings, "other", &Value::Null, "").is_err());
        settings.integration = Some(demodex_protocol::ModelPromptOverride {
            text: "Custom integration".into(),
            reviewed_default: String::new(),
        });
        assert_eq!(
            compose(&settings, "one", &default, "Operator")?.1,
            "Operator\n\nCustom integration"
        );
        Ok(())
    }
    #[test]
    fn settings_survive_restart_and_reject_stale_saves() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("db");
        let store = crate::store::Store::open(&path)?;
        assert_eq!(store.prompt_settings()?.0, 0);
        assert_eq!(store.set_prompt_settings(0, &PromptSettings::default())?, 1);
        assert!(
            store
                .set_prompt_settings(0, &PromptSettings::default())
                .is_err()
        );
        drop(store);
        assert_eq!(crate::store::Store::open(&path)?.prompt_settings()?.0, 1);
        Ok(())
    }
}
