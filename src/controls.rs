//! Explicit UI counterparts of Codex commands; never interpreted as chat text.
use crate::manager::{Live, Manager};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub use demodex_protocol::ModelChoice;

pub use demodex_protocol::GoalAction;

pub fn effective_settings(settings: &Value) -> Value {
    if !settings["model"].is_string() {
        return Value::Null;
    }
    json!({"model":settings["model"],"effort":settings["effort"],"serviceTier":settings["serviceTier"]})
}
pub fn start_settings(settings: &Value) -> Value {
    effective_settings(
        &json!({"model":settings["model"],"effort":settings["reasoningEffort"],"serviceTier":settings["serviceTier"]}),
    )
}

fn validate_model(choice: &ModelChoice, catalog: &[Value]) -> Result<Value> {
    let model = catalog
        .iter()
        .find(|m| m["model"] == choice.model)
        .context("model is not in this app-server's model catalog")?;
    ensure!(
        model["supportedReasoningEfforts"]
            .as_array()
            .is_some_and(|levels| levels.iter().any(|v| v["reasoningEffort"] == choice.effort)),
        "reasoning effort is not supported by this model"
    );
    if let Some(tier) = &choice.service_tier {
        ensure!(
            model["serviceTiers"]
                .as_array()
                .is_some_and(|tiers| tiers.iter().any(|t| t["id"] == *tier)),
            "service tier is not supported by this model"
        );
    }
    Ok(json!({"model":choice.model,"effort":choice.effort,"serviceTier":choice.service_tier}))
}

fn same_model_settings(accepted: &Value, requested: &Value) -> bool {
    let tier = |value: &Value| match value["serviceTier"].as_str() {
        None | Some("default") => String::new(),
        Some(tier) => tier.into(),
    };
    accepted["model"] == requested["model"]
        && accepted["effort"] == requested["effort"]
        && tier(accepted) == tier(requested)
}

fn goal_params(thread: &str, input: &GoalAction) -> Result<(&'static str, Value)> {
    let mut params = json!({"threadId":thread});
    match input.action.as_str() {
        "save" => {
            let objective = input
                .objective
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .context("enter a goal objective")?;
            ensure!(
                objective.chars().count() <= 4000,
                "goal objective must be at most 4000 characters"
            );
            params["objective"] = json!(objective);
            params["status"] = json!("paused");
            if let Some(budget) = input.token_budget {
                ensure!(budget > 0, "token budget must be positive");
                params["tokenBudget"] = json!(budget);
            }
        }
        "pause" | "resume" | "complete" | "clear" => {
            ensure!(
                input.objective.is_none() && input.token_budget.is_none(),
                "status actions must not replace the objective or budget"
            );
            if input.action == "clear" {
                return Ok(("thread/goal/clear", params));
            }
            params["status"] = json!(match input.action.as_str() {
                "pause" => "paused",
                "resume" => "active",
                _ => "complete",
            });
        }
        _ => anyhow::bail!("unsupported goal action"),
    }
    Ok(("thread/goal/set", params))
}

impl Manager {
    pub(crate) async fn require_idle(live: &Live) -> Result<()> {
        let state = live
            .rpc
            .call(
                "thread/read",
                json!({"threadId":live.thread,"includeTurns":false}),
            )
            .await?;
        ensure!(
            state["thread"]["status"]["type"] == "idle" && live.turn.lock().await.is_none(),
            "finish or interrupt the current turn before changing these session controls"
        );
        Ok(())
    }

    pub async fn model_catalog(&self, id: &str) -> Result<Value> {
        let live = self.runtime(id).await?;
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        let mut seen = std::collections::HashSet::new();
        loop {
            let result = live
                .rpc
                .call(
                    "model/list",
                    json!({"cursor":cursor,"limit":100,"includeHidden":false}),
                )
                .await?;
            models.extend(
                result["data"]
                    .as_array()
                    .context("model/list returned no model catalog")?
                    .iter()
                    .cloned(),
            );
            cursor = result["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
            ensure!(
                seen.len() < 100 && seen.insert(cursor.to_string()),
                "model catalog pagination did not finish"
            );
        }
        Ok(json!({"data":models}))
    }

    pub async fn control_snapshot(&self, id: &str) -> Result<Value> {
        let settings = self.store.model_settings(id)?;
        let mut snapshot =
            json!({"settings":settings,"goal":null,"goalError":null,"connected":false});
        match self.runtime(id).await {
            Ok(live) => {
                snapshot["connected"] = json!(true);
                match live
                    .rpc
                    .call("thread/goal/get", json!({"threadId":live.thread}))
                    .await
                {
                    Ok(result) if result.get("goal").is_some() => {
                        snapshot["goal"] = result["goal"].clone()
                    }
                    Ok(_) => {
                        snapshot["goalError"] =
                            json!("Codex returned no goal state; this API may be unsupported.")
                    }
                    Err(error) => {
                        snapshot["goalError"] = json!(format!("Goal state unavailable: {error:#}"))
                    }
                }
            }
            Err(_) => {
                snapshot["goalError"] = json!("Connect this session to read or change its goal.")
            }
        }
        Ok(snapshot)
    }

    pub async fn change_model(&self, id: &str, choice: ModelChoice) -> Result<Value> {
        let _settings = self.connecting.lock().await;
        let live = self.runtime(id).await?;
        Self::require_idle(&live).await?;
        let catalog = self.model_catalog(id).await?;
        let requested = validate_model(
            &choice,
            catalog["data"].as_array().context("missing catalog")?,
        )?;
        let current = self.store.model_settings(id)?["effective"].clone();
        if same_model_settings(&current, &requested) {
            self.store.model_selection(id, &current)?;
            return Ok(current);
        }
        let mut updates = self.model_updates.subscribe();
        let mut params = requested.clone();
        params["threadId"] = json!(live.thread);
        self.store.model_effective(id, &Value::Null)?;
        self.changed();
        // No optimistic success: the RPC acknowledges, the notification confirms.
        live.rpc.call("thread/settings/update", params).await?;
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let (session,generation,settings) = updates.recv().await?;
                if session == id && generation == live.generation { return Ok::<Value,anyhow::Error>(settings); }
            }
        }).await.context("Model change was acknowledged but Codex did not confirm its settings; model is unconfirmed")??;
        ensure!(
            self.runtime(id).await?.generation == live.generation,
            "connection changed while confirming the model"
        );
        self.store.model_selection(id, &accepted)?;
        self.changed();
        ensure!(
            same_model_settings(&accepted, &requested),
            "Codex accepted different model settings: {accepted}"
        );
        Ok(accepted)
    }

    pub async fn change_goal(&self, id: &str, action: GoalAction) -> Result<Value> {
        let _settings = self.connecting.lock().await;
        ensure!(
            !self.store.get(id)?.archived,
            "Restore the session before changing its goal"
        );
        let live = self.runtime(id).await?;
        let (method, params) = goal_params(&live.thread, &action)?;
        if action.action == "resume" {
            ensure!(
                !self.store.targets_pending(id)?,
                "Send a message to apply the selected targets before resuming the goal"
            );
        }
        // Pausing prevents further autonomous turns, and remains available while
        // the current turn is running. It does not implicitly interrupt that turn.
        if action.action != "pause" {
            Self::require_idle(&live).await?;
        }
        let result = live.rpc.call(method, params).await?;
        self.changed();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_capabilities_come_from_the_server() {
        let catalog = vec![
            json!({"model":"fixture","supportedReasoningEfforts":[{"reasoningEffort":"future-level"}],"serviceTiers":[{"id":"priority"}]}),
        ];
        let mut choice = ModelChoice {
            model: "fixture".into(),
            effort: "future-level".into(),
            service_tier: None,
        };
        assert!(validate_model(&choice, &catalog).is_ok());
        choice.effort = "invented".into();
        assert!(validate_model(&choice, &catalog).is_err());
        choice.effort = "future-level".into();
        choice.service_tier = Some("unknown".into());
        assert!(validate_model(&choice, &catalog).is_err());
    }
    #[test]
    fn saving_a_goal_never_implicitly_starts_it_or_assigns_a_budget() {
        let input = GoalAction {
            action: "save".into(),
            objective: Some("Build it".into()),
            token_budget: None,
        };
        let (_, params) = goal_params("thread", &input).unwrap();
        assert_eq!(params["status"], "paused");
        assert!(params.get("tokenBudget").is_none());
        let input = GoalAction {
            action: "pause".into(),
            objective: Some("Replacement".into()),
            token_budget: None,
        };
        assert!(goal_params("thread", &input).is_err());
    }
    #[test]
    fn default_tier_confirmation_is_equivalent_but_other_changes_are_not() {
        let requested = json!({"model":"fixture","effort":"high","serviceTier":null});
        assert!(same_model_settings(
            &json!({"model":"fixture","effort":"high","serviceTier":"default"}),
            &requested
        ));
        assert!(!same_model_settings(
            &json!({"model":"fixture","effort":"low","serviceTier":"default"}),
            &requested
        ));
        assert!(!same_model_settings(
            &json!({"model":"fixture","effort":"high","serviceTier":"priority"}),
            &requested
        ));
    }
    #[tokio::test]
    async fn model_update_needs_confirmation_from_its_own_thread() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "thread/resume" => {
                        json!({"thread":{"id":"thread","turns":[]},"model":"fixture","reasoningEffort":"low"})
                    }
                    "thread/read" => json!({"thread":{"status":{"type":"idle"}}}),
                    "model/list" => {
                        json!({"data":[{"model":"fixture","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}],"nextCursor":null})
                    }
                    "thread/settings/update" => {
                        // An unrelated thread must not turn an acknowledgement into confirmation.
                        ws.send(Message::Text(json!({"method":"thread/settings/updated","params":{"threadId":"other","threadSettings":{"model":"fixture","effort":"high"}}}).to_string().into())).await.unwrap();
                        json!({})
                    }
                    method => panic!("unexpected RPC: {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
        });
        let manager = Manager::new(crate::store::Store::open(std::path::Path::new(":memory:"))?);
        let session = manager
            .store
            .create("fixture", &endpoint, &[], Some("thread"))?;
        manager.connect(&session.id).await?;
        let result = manager
            .change_model(
                &session.id,
                ModelChoice {
                    model: "fixture".into(),
                    effort: "high".into(),
                    service_tier: None,
                },
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("did not confirm"));
        let saved = manager.store.model_settings(&session.id)?;
        assert!(saved["effective"].is_null() && saved["selection"].is_null());
        manager.disconnect(&session.id, "done").await?;
        server.await?;
        Ok(())
    }
}
