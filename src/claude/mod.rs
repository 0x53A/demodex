//! Claude account credentials stay in the private daemon directory. The browser
//! sees only a pending authorization URL and non-secret account/model metadata.
mod bridge;
mod translate;
mod usage;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub(crate) const BASE_INSTRUCTIONS: &str = "You are a coding assistant running inside the Codex harness. Use the provided tools to complete the user's task. Follow the project instructions and verify your changes.";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const API: &str = "https://api.anthropic.com";
/// Environment variable carrying the bridge key to the Claude app-server. Codex's
/// default shell environment policy excludes `*KEY*` names from tool processes.
pub(crate) const INFERENCE_KEY_ENV: &str = "DEMODEX_INFERENCE_KEY";
pub(crate) struct RuntimeSettings {
    pub overrides: Vec<String>,
    pub key: String,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    access: String,
    refresh: String,
    expires: u64,
    #[serde(default)]
    models: Vec<Value>,
}
struct Pending {
    id: String,
    verifier: String,
    state: String,
    expires: u64,
}
struct State {
    generation: u64,
    credentials: Option<Credentials>,
    pending: Option<Pending>,
    error: Option<String>,
}
pub(crate) struct Claude {
    directory: PathBuf,
    profile: PathBuf,
    api: String,
    token_url: String,
    client: reqwest::Client,
    state: Mutex<State>,
    usage: Mutex<Option<usage::Cached>>,
    bridge: tokio::sync::OnceCell<bridge::Bridge>,
    applied_models: std::sync::Mutex<Vec<String>>,
}
impl Claude {
    pub fn new(root: &Path, profile: PathBuf) -> Result<Arc<Self>> {
        let directory = root.join("claude");
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        std::fs::create_dir_all(root.join("runtime/claude-catalogue"))?;
        let (credentials, error) = match std::fs::read(directory.join("account.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(c) => (Some(c), None),
                Err(e) => {
                    // serde errors describe position/shape, not values.
                    tracing::warn!("Claude account file invalid: {}", clip(&e.to_string()));
                    (
                        None,
                        Some("Saved Claude account is invalid; sign in again".into()),
                    )
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(e) => {
                tracing::warn!("Claude account file unreadable: {e}");
                (
                    None,
                    Some("Saved Claude account could not be read; check its file permissions".into()),
                )
            }
        };
        Ok(Arc::new(Self {
            directory,
            profile,
            api: API.into(),
            token_url: TOKEN_URL.into(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .build()?,
            usage: Mutex::new(None),
            state: Mutex::new(State {
                generation: 0,
                credentials,
                pending: None,
                error,
            }),
            applied_models: Default::default(),
            bridge: Default::default(),
        }))
    }
    fn save(&self, credentials: &Credentials) -> Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let temp = self
            .directory
            .join(format!("account-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            f.write_all(&serde_json::to_vec(credentials)?)?;
            f.sync_all()?;
            std::fs::rename(&temp, self.directory.join("account.json"))?;
            std::fs::File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    pub async fn status(&self) -> Value {
        let state = self.state.lock().await;
        json!({"connected":state.credentials.is_some(),"error":state.error, "models_available":!self.applied_models.lock().unwrap().is_empty(), "restart_required":state.credentials.as_ref().is_some_and(|c|c.models.iter().any(|m|!self.applied_models.lock().unwrap().iter().any(|id|m["id"]==*id))), "models":state.credentials.as_ref().map(|c|&c.models),
            "pending":state.pending.as_ref().filter(|p|p.expires>now()).map(|p|json!({"id":p.id,"expires":p.expires}))})
    }
    pub async fn begin(&self) -> Result<Value> {
        let p = Pending {
            id: uuid::Uuid::new_v4().to_string(),
            verifier: random(),
            state: random(),
            expires: now() + 600,
        };
        let mut url = reqwest::Url::parse("https://claude.ai/oauth/authorize")?;
        url.query_pairs_mut().extend_pairs([
            ("client_id",CLIENT_ID),("response_type","code"),("redirect_uri",REDIRECT),
            ("scope","org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"),("code","true"),
            ("code_challenge_method","S256"),("code_challenge",&URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes()))),("state",&p.state)]);
        let out = json!({"id":p.id,"url":url.as_str(),"expires":p.expires});
        self.state.lock().await.pending = Some(p);
        Ok(out)
    }
    async fn token(&self, body: Value) -> Result<Value> {
        let r = self
            .client
            .post(&self.token_url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("Claude authentication request failed")?;
        if !r.status().is_success() {
            let status = r.status().as_u16();
            let detail = match tokio::time::timeout(Duration::from_secs(5), r.bytes()).await {
                Ok(Ok(body)) if body.len() <= 64 * 1024 => error_summary(&body),
                _ => "error body unavailable".into(),
            };
            tracing::warn!("Claude token request failed (HTTP {status}): {detail}");
            bail!("Claude authentication failed (HTTP {status}); start a new login");
        }
        read_json(r, 64 * 1024)
            .await
            .context("Invalid Claude token response")
    }
    pub async fn complete(&self, id: &str, input: &str) -> Result<Value> {
        ensure!(input.len() <= 8192, "Authorization code is too long");
        let mut state = self.state.lock().await;
        let p = state.pending.as_ref().context("Start a new Claude login")?;
        ensure!(
            p.id == id && p.expires > now(),
            "Claude login expired or was replaced; start again"
        );
        let (code, returned_state) = input
            .trim()
            .split_once('#')
            .context("Paste the complete code#state shown by Anthropic")?;
        ensure!(
            !code.is_empty() && returned_state == p.state,
            "Authorization state does not match this login"
        );
        let p = state.pending.take().unwrap(); // A code exchange is never replayed automatically.
        let t=self.token(json!({"grant_type":"authorization_code","client_id":CLIENT_ID,"code":code,"state":p.state,"redirect_uri":REDIRECT,"code_verifier":p.verifier})).await?;
        let c = credentials(t, None)?;
        self.save(&c)?;
        state.generation += 1;
        state.credentials = Some(c);
        state.error = None;
        drop(state);
        self.refresh_models().await
    }
    pub async fn refresh_models(&self) -> Result<Value> {
        let access = self.access().await?;
        let models = self.fetch_models(&access).await?;
        let mut state = self.state.lock().await;
        let c = state
            .credentials
            .as_mut()
            .context("Claude account disconnected during model discovery")?;
        ensure!(
            c.access == access,
            "Claude account changed during model discovery; refresh again"
        );
        c.models = models;
        self.save(c)?;
        drop(state);
        Ok(self.status().await)
    }
    pub async fn disconnect(&self) -> Result<Value> {
        let mut state = self.state.lock().await;
        match std::fs::remove_file(self.directory.join("account.json")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        state.generation += 1;
        state.credentials = None;
        state.error = None;
        state.pending = None;
        Ok(json!({"connected":false}))
    }
    async fn model(&self, id: &str) -> Result<Value> {
        let state = self.state.lock().await;
        state
            .credentials
            .as_ref()
            .context("Claude account disconnected")?
            .models
            .iter()
            .find(|m| m["id"] == id)
            .cloned()
            .context("Claude model is not in the account catalogue")
    }
    async fn access(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        let c = state
            .credentials
            .as_mut()
            .context("Connect a Claude account in Server settings")?;
        if c.expires <= now() + 300 {
            let t=self.token(json!({"grant_type":"refresh_token","client_id":CLIENT_ID,"refresh_token":c.refresh})).await?;
            let mut updated = credentials(t, Some(&c.refresh))?;
            updated.models = c.models.clone();
            self.save(&updated)?;
            *c = updated;
        }
        Ok(c.access.clone())
    }
    async fn fetch_models(&self, access: &str) -> Result<Vec<Value>> {
        let mut models = Vec::new();
        let mut after = None;
        for _ in 0..20 {
            let mut r = self.client.get(format!("{}/v1/models?limit=100", self.api));
            if let Some(id) = &after {
                r = r.query(&[("after_id", id)]);
            }
            let r = anthropic_headers(r, access, false)
                .timeout(Duration::from_secs(30))
                .send()
                .await?;
            if !r.status().is_success() {
                let status = r.status().as_u16();
                let detail = match tokio::time::timeout(Duration::from_secs(5), r.bytes()).await {
                    Ok(Ok(body)) if body.len() <= 64 * 1024 => error_summary(&body),
                    _ => "error body unavailable".into(),
                };
                tracing::warn!("Claude model discovery failed (HTTP {status}): {detail}");
                bail!("Claude model discovery failed (HTTP {status})");
            }
            let v = read_json(r, 4 * 1024 * 1024).await?;
            for m in v["data"].as_array().context("Missing Claude model list")? {
                if m["id"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("claude-") && s.len() < 128)
                {
                    models.push(m.clone());
                }
            }
            if v["has_more"] != true {
                ensure!(!models.is_empty(), "Claude returned no models");
                return Ok(models);
            }
            let next = v["last_id"]
                .as_str()
                .context("Missing Claude model cursor")?
                .to_owned();
            ensure!(
                after.as_ref() != Some(&next),
                "Claude model cursor repeated"
            );
            after = Some(next);
        }
        anyhow::bail!("Claude model list exceeded page limit")
    }
    pub async fn add_models(&self, catalog: &mut Value) {
        let state = self.state.lock().await;
        if state.credentials.is_none() {
            if let Some(rows) = catalog["data"].as_array_mut() {
                rows.retain(|r| {
                    !r["model"]
                        .as_str()
                        .is_some_and(|m| m.starts_with("claude-"))
                });
            }
            return;
        }
        if let Some(rows) = catalog["data"].as_array_mut() {
            for model in &state.credentials.as_ref().unwrap().models {
                if rows.iter().any(|r| r["model"] == model["id"]) {
                    continue;
                }
                let levels = efforts(model);
                let default = if levels.contains(&"medium") {
                    "medium"
                } else {
                    levels[0]
                };
                rows.push(json!({"id":model["id"],"model":model["id"],"displayName":model["display_name"],"description":"Claude in an isolated runtime","isDefault":false,"hidden":false,"defaultReasoningEffort":default,"supportedReasoningEfforts":levels.iter().map(|e|json!({"reasoningEffort":e,"description":e})).collect::<Vec<_>>(),"serviceTiers":[],"inputModalities":["text","image"],"supportsPersonality":false}));
            }
            for row in rows.iter_mut().filter(|r| {
                r["model"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("claude-"))
            }) {
                let model = state
                    .credentials
                    .as_ref()
                    .unwrap()
                    .models
                    .iter()
                    .find(|m| m["id"] == row["model"]);
                row["context_window"] = model
                    .and_then(|m| m["max_input_tokens"].as_i64())
                    .filter(|n| *n >= 16000)
                    .map(|max| json!({"min":16000,"default":max.min(200000),"max":max}))
                    .unwrap_or(Value::Null);
            }
        }
    }
    pub async fn runtime_catalog(&self) -> Result<Option<PathBuf>> {
        let path = self
            .directory
            .parent()
            .unwrap()
            .join("runtime/claude-catalogue/models.json");
        // Logout must not rewrite the catalogue of a still-running Claude runtime.
        // New Claude runtimes require a connected account.
        if self.state.lock().await.credentials.is_none() {
            self.applied_models.lock().unwrap().clear();
            return Ok(None);
        }

        let config_path = self.profile.join("config.toml");
        let config: toml::Value = if config_path.exists() {
            toml::from_str(&std::fs::read_to_string(config_path)?)?
        } else {
            toml::Value::Table(Default::default())
        };
        let custom = config
            .get("model_catalog_json")
            .and_then(toml::Value::as_str)
            .map(PathBuf::from);
        let source = custom
            .clone()
            .unwrap_or_else(|| self.profile.join("models_cache.json"));
        let bytes = if source.exists() {
            ensure!(
                std::fs::metadata(&source)?.len() <= 32 * 1024 * 1024,
                "Model catalogue is too large"
            );
            std::fs::read(source)?
        } else {
            ensure!(
                custom.is_none(),
                "Configured model catalogue does not exist"
            );
            let output = tokio::process::Command::new(crate::orchestrator::find_binary("codex")?)
                .args(["debug", "models", "--bundled"])
                .env("CODEX_HOME", &self.profile)
                .output()
                .await?;
            ensure!(
                output.status.success(),
                "Could not read bundled Codex models"
            );
            output.stdout
        };
        let mut catalog: Value = serde_json::from_slice(&bytes)?;
        let models = catalog["models"]
            .as_array_mut()
            .context("Invalid Codex model catalogue")?;
        models.retain(|m| {
            !m["slug"]
                .as_str()
                .is_some_and(|slug| slug.starts_with("claude-"))
        });
        let template = models
            .first()
            .context("Empty Codex model catalogue")?
            .clone();
        let state = self.state.lock().await;
        for m in state
            .credentials
            .as_ref()
            .map(|c| c.models.as_slice())
            .unwrap_or(&[])
        {
            if models.iter().any(|r| r["slug"] == m["id"]) {
                continue;
            }
            let mut row = template.clone();
            row["slug"] = m["id"].clone();
            row["display_name"] = m["display_name"].clone();
            let maximum = m["max_input_tokens"]
                .as_i64()
                .filter(|n| *n >= 16000)
                .unwrap_or(200000);
            row["description"] = json!("Claude through Demodex");
            row["context_window"] = json!(maximum.min(200000));
            row["max_context_window"] = json!(maximum);
            let efforts = efforts(m);
            let default = if efforts.contains(&"medium") {
                "medium"
            } else {
                efforts[0]
            };
            row["default_reasoning_level"] = json!(default);
            row["supported_reasoning_levels"] = json!(
                efforts
                    .iter()
                    .map(|level| json!({"effort":level,"description":level}))
                    .collect::<Vec<_>>()
            );
            row["base_instructions"] = json!(BASE_INSTRUCTIONS);
            row["model_messages"] = json!({"instructions_template":row["base_instructions"]});
            for key in [
                "available_access_programs",
                "availability_nux",
                "upgrade",
                "guardian",
                "default_service_tier",
            ] {
                row.as_object_mut().unwrap().remove(key);
            }
            for key in [
                "additional_speed_tiers",
                "service_tiers",
                "experimental_supported_tools",
            ] {
                row[key] = json!([]);
            }
            row["tool_mode"] = json!("code_mode");
            row["supports_search_tool"] = json!(false);
            row["use_responses_lite"] = json!(false);
            row["support_verbosity"] = json!(false);
            row["multi_agent_reasoning_effort"] = json!(default);
            row["supports_reasoning_summaries"] = json!(true);
            row["prefer_websockets"] = json!(false);
            row["supported_in_api"] = json!(true);
            models.push(row);
        }
        models.retain(|m| {
            m["slug"]
                .as_str()
                .is_some_and(|slug| slug.starts_with("claude-"))
        });
        let ids = state
            .credentials
            .as_ref()
            .map(|c| {
                c.models
                    .iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        drop(state);
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp, serde_json::to_vec(&catalog)?)?;
        std::fs::rename(temp, &path)?;
        *self.applied_models.lock().unwrap() = ids;
        Ok(Some(path))
    }
    /// Config overrides for the Claude app-server plus the bridge key. The key is
    /// never part of an override: command lines are readable by every local user.
    /// Callers must deliver it as `INFERENCE_KEY_ENV` in the app-server environment.
    pub async fn runtime_settings(self: &Arc<Self>) -> Result<RuntimeSettings> {
        let bridge = self
            .bridge
            .get_or_try_init(|| bridge::start(self.clone()))
            .await?;
        let model = self
            .applied_models
            .lock()
            .unwrap()
            .first()
            .cloned()
            .context("No Claude models available; connect or refresh the account")?;
        let provider = json!({"name":"Demodex Claude","base_url":bridge.url,"wire_api":"responses","requires_openai_auth":false,"supports_websockets":false,"request_max_retries":0,"stream_max_retries":0,"env_http_headers":{"x-demodex-inference":INFERENCE_KEY_ENV}});
        Ok(RuntimeSettings {
            overrides: vec![
                "model_provider=\"demodex_mixed\"".into(),
                format!("model={}", serde_json::to_string(&model)?),
                format!(
                    "model_providers.demodex_mixed={}",
                    toml::Value::try_from(provider)?
                ),
            ],
            key: bridge.key.clone(),
        })
    }

    pub async fn configure(
        self: &Arc<Self>,
        params: &mut Value,
        upstream: &Value,
        _required: bool,
    ) -> Result<()> {
        ensure!(
            upstream["config"]["model_provider"] == "demodex_mixed",
            "Claude requires its isolated runtime"
        );
        let selected = params["model"]
            .as_str()
            .or_else(|| upstream["config"]["model"].as_str())
            .context("Claude model unavailable")?;
        ensure!(
            selected.starts_with("claude-"),
            "GPT models must use the native OpenAI runtime"
        );
        let mut agents = upstream["config"]["features"]["multi_agent_v2"].clone();
        if !agents.is_object() {
            agents = json!({"enabled":agents.as_bool().unwrap_or(true)});
        }
        agents["non_code_mode_only"] = json!(false);
        params["config"]["features"]["multi_agent_v2"] = agents;
        params["modelProvider"] = json!("demodex_mixed");
        Ok(())
    }
    pub async fn shutdown(&self) {
        if let Some(b) = self.bridge.get() {
            b.stop.cancel();
            if let Some(task) = b.task.lock().await.take() {
                let _ = task.await;
            }
        }
    }
}
fn credentials(t: Value, old_refresh: Option<&str>) -> Result<Credentials> {
    Ok(Credentials {
        access: t["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("Missing Claude access token")?
            .into(),
        refresh: t["refresh_token"]
            .as_str()
            .or(old_refresh)
            .context("Missing Claude refresh token")?
            .into(),
        expires: now()
            + t["expires_in"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 366 * 86400)
                .context("Invalid Claude token expiry")?,
        models: Vec::new(),
    })
}
/// Claude Code identity presented to Anthropic. Some models are gated on a minimum version
/// (opus-5-5 needs >= 2.1.280, HTTP 400 otherwise). Matches earendil-works/pi's `claudeCodeVersion`.
const CLAUDE_CLI_VERSION: &str = "2.1.280";

/// `interleaved_thinking` adds the interleaved-thinking beta; like Pi, only for requests with
/// non-adaptive (budgeted) thinking, since adaptive models have it built in.
fn anthropic_headers(
    r: reqwest::RequestBuilder,
    access: &str,
    interleaved_thinking: bool,
) -> reqwest::RequestBuilder {
    let beta = if interleaved_thinking {
        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14"
    } else {
        "claude-code-20250219,oauth-2025-04-20"
    };
    r.bearer_auth(access)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", beta)
        .header("user-agent", format!("claude-cli/{CLAUDE_CLI_VERSION}"))
        .header("x-app", "cli")
}

/// Bound diagnostic text for logs (characters, not bytes).
fn clip(text: &str) -> String {
    const MAX: usize = 300;
    let mut out: String = text.chars().take(MAX).collect();
    if text.chars().count() > MAX {
        out.push('…');
    }
    out
}

/// Anthropic/OAuth error bodies are `{"error":{"type","message"}}` or `{"error":"code"}`.
/// Only those fields are extracted (and clipped), never the whole body.
fn error_summary(body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return "unparseable error body".into();
    };
    clip(&match &v["error"] {
        Value::String(code) => code.clone(),
        e => format!(
            "{}: {}",
            e["type"].as_str().unwrap_or("?"),
            e["message"].as_str().unwrap_or("?")
        ),
    })
}

#[cfg(test)]
mod tests;

async fn read_json(mut response: reqwest::Response, limit: usize) -> Result<Value> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            data.len() + chunk.len() <= limit,
            "Provider response is too large"
        );
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).context("Invalid provider JSON")
}

fn efforts(model: &Value) -> Vec<&'static str> {
    if model["capabilities"]["thinking"]["supported"] == false {
        return vec!["none"];
    }
    if model["capabilities"]["effort"]["supported"] == true {
        let values: Vec<_> = ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .filter(|level| model["capabilities"]["effort"][level]["supported"] == true)
            .collect();
        if !values.is_empty() {
            return values;
        }
    }
    vec!["low", "medium", "high"]
}
