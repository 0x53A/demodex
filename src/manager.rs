use crate::{
    rpc::Rpc,
    store::{Store, Target},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, broadcast};

pub struct Live {
    pub rpc: Rpc,
    pub generation: String,
    pub thread: String,
    pub turn: Mutex<Option<String>>,
}

pub struct Manager {
    pub store: Store,
    pub live: Mutex<HashMap<String, Arc<Live>>>,
    // Serializes connection creation; no database lock is held across network awaits.
    pub(crate) connecting: Mutex<()>,
    pub updates: broadcast::Sender<()>,
    settings_updates: broadcast::Sender<(String, String, Value)>,
    pub(crate) model_updates: broadcast::Sender<(String, String, Value)>,
}

impl Manager {
    pub fn new(store: Store) -> Arc<Self> {
        Arc::new(Self {
            store,
            live: Mutex::new(HashMap::new()),
            connecting: Mutex::new(()),
            updates: broadcast::channel(64).0,
            settings_updates: broadcast::channel(64).0,
            model_updates: broadcast::channel(64).0,
        })
    }
    pub fn changed(&self) {
        let _ = self.updates.send(());
    }
    pub async fn runtime(&self, id: &str) -> Result<Arc<Live>> {
        self.live
            .lock()
            .await
            .get(id)
            .cloned()
            .context("session is disconnected; reconnect first")
    }

    pub async fn disconnect(&self, id: &str, reason: &str) -> Result<()> {
        let mut sessions = self.live.lock().await;
        if let Some(live) = sessions.remove(id) {
            live.rpc.close();
            self.store.disconnected(id, &live.generation)?;
        }
        self.store.status(id, "disconnected", Some(reason))?;
        self.changed();
        Ok(())
    }

    pub async fn archive(&self, id: &str, archived: bool) -> Result<()> {
        let _settings = self.connecting.lock().await;
        let session = self.store.get(id)?;
        if archived {
            anyhow::ensure!(
                matches!(
                    session.status.as_str(),
                    "idle" | "connected" | "disconnected"
                ),
                "Stop the session before archiving it"
            );
            anyhow::ensure!(
                !self
                    .store
                    .pending(id)?
                    .iter()
                    .any(|p| matches!(p.state.as_str(), "pending" | "responding" | "delivered")),
                "Resolve pending decisions before archiving the session"
            );
            if let Ok(live) = self.runtime(id).await {
                Self::require_idle(&live).await?;
                anyhow::ensure!(
                    crate::background::list(&live).await?.is_empty(),
                    "Stop background terminals before archiving the session"
                );
                anyhow::ensure!(
                    self.queued(id)
                        .await?
                        .as_array()
                        .is_some_and(|q| q.is_empty()),
                    "Remove queued messages before archiving the session"
                );
                let goal = live
                    .rpc
                    .call("thread/goal/get", json!({"threadId":live.thread}))
                    .await?;
                anyhow::ensure!(
                    goal.get("goal").is_some(),
                    "Cannot verify the session goal state"
                );
                anyhow::ensure!(
                    goal["goal"]["status"] != "active",
                    "Pause the goal before archiving the session"
                );
                let turn = live.turn.lock().await;
                anyhow::ensure!(turn.is_none(), "Stop the session before archiving it");
                self.store.archive(id, true)?;
            } else {
                anyhow::ensure!(
                    session.status == "disconnected",
                    "Cannot verify that the session is stopped"
                );
                self.store.archive(id, true)?;
            }
        } else {
            self.store.archive(id, false)?;
        }
        self.changed();
        Ok(())
    }

    pub async fn connect(self: &Arc<Self>, id: &str) -> Result<()> {
        let _connecting = self.connecting.lock().await;
        anyhow::ensure!(
            !self.store.get(id)?.archived,
            "Restore the session before resuming it"
        );
        if self.live.lock().await.contains_key(id) {
            return Ok(());
        }
        let session = self.store.get(id)?;
        self.store.status(id, "connecting", None)?;
        self.changed();
        let generation = uuid::Uuid::new_v4().to_string();
        let outcome=async {
            let (rpc,mut events)=Rpc::connect(&session.endpoint).await?;
            for target in &session.targets {
                rpc.call("environment/add",json!({"environmentId":target.id,"execServerUrl":target.url})).await?;
            }
            let environments=environment_params(&session.targets);
            let cwd=if session.targets.len()==1 {Some(&session.targets[0].cwd)} else {None};
            let mut params = json!({"cwd":cwd,"sandbox":session.sandbox,"approvalPolicy":"on-request","approvalsReviewer":"user"});
            if let Some(selection) = self.store.model_settings(id)?["selection"].as_object() {
                for key in ["model","serviceTier"] { if let Some(value) = selection.get(key) { params[key] = value.clone(); } }
                if let Some(effort) = selection.get("effort").filter(|v|v.is_string()) {
                    params["config"] = json!({"model_reasoning_effort":effort});
                }
            }
            if let Some(prompt) = self.store.prompt(id)? {
                params["baseInstructions"] = json!(prompt);
                params["developerInstructions"] = json!("");
                // The caller supplied the complete editable instruction text.
                // Retain runtime context/tools, but do not load AGENTS.md again.
                params["config"]["project_doc_max_bytes"] = json!(0);
            } else if session.thread_id.is_none() || session.presentation.context_reporting {
                // Read effective instructions instead of replacing the operator's
                // configuration with our integration snippet. Never write the profile.
                let config = rpc.call("config/read", json!({"cwd":cwd,"includeLayers":false})).await?;
                let existing = config["config"]["developer_instructions"].as_str().unwrap_or("");
                params["developerInstructions"] = json!(format!("{existing}\n\n{}\n\n{}", crate::session_context::INSTRUCTIONS, crate::ssh::AGENT_INSTRUCTIONS));
            }
            let mut active_turn=None;
            let thread=if let Some(thread)=&session.thread_id {
                params["threadId"] = json!(thread);
                let result=rpc.call("thread/resume",params).await?;
                self.store.model_effective(id, &crate::controls::start_settings(&result))?;
                self.store.effective_sandbox(id, &result["sandbox"])?;
                // Preserve the snapshot so a newly imported conversation has its existing history.
                self.store.event(id,&json!({"method":"demodex/threadSnapshot","params":result}))?;
                active_turn=result["thread"]["turns"].as_array().and_then(|turns|turns.iter().rev().find(|t|t["status"]=="inProgress")).and_then(|t|t["id"].as_str()).map(str::to_string);
                result["thread"]["id"].as_str().context("missing resumed thread id")?.to_string()
            } else {
                params["environments"] = environments;
                params["dynamicTools"] = crate::session_context::tools();
                let result=rpc.call("thread/start",params).await?;
                self.store.model_effective(id, &crate::controls::start_settings(&result))?;
                self.store.effective_sandbox(id, &result["sandbox"])?;
                let thread=result["thread"]["id"].as_str().context("missing new thread id")?.to_string();
                self.store.thread(id,&thread)?;
                self.store.enable_context_reporting(id)?;
                thread
            };
            let live=Arc::new(Live {rpc,generation:generation.clone(),thread,turn:Mutex::new(active_turn)});
            self.live.lock().await.insert(id.into(),live.clone());
            self.store.status(id,"connected",None)?; self.changed();
            let manager=self.clone(); let id=id.to_string();
            tokio::spawn(async move {
                let mut failure="app-server disconnected".to_string();
                while let Some(event)=events.recv().await {
                    match event {
                        Ok(message) => {
                            if let Err(error)=manager.ingest(&id,&live,&message).await { failure=error.to_string(); break; }
                        }
                        Err(error)=>{failure=error.to_string();break;}
                    }
                }
                let mut sessions=manager.live.lock().await;
                let current=sessions.get(&id).is_some_and(|s|s.generation==live.generation);
                if current { sessions.remove(&id); }
                let _=manager.store.disconnected(&id,&live.generation);
                if current { let _=manager.store.status(&id,"disconnected",Some(&failure)); }
                drop(sessions);
                manager.changed();
            });
            Ok(())
        }.await;
        if let Err(error) = &outcome {
            self.store
                .status(id, "disconnected", Some(&format!("{error:#}")))?;
            self.store.disconnected(id, &generation)?;
            self.changed();
        }
        outcome
    }

    async fn ingest(&self, id: &str, live: &Live, message: &Value) -> Result<()> {
        let method = message["method"].as_str().unwrap_or("");
        // A prompt holds this lock while awaiting its RPC. Do not hold the
        // daemon-wide live map while waiting for that one session's turn.
        let mut turn = if matches!(
            method,
            "turn/started" | "turn/completed" | "thread/status/changed"
        ) {
            Some(live.turn.lock().await)
        } else {
            None
        };
        // Buffered events from a closed connection must not revive decisions or
        // overwrite a replacement's status. Hold the generation guard while
        // applying state so disconnect cannot invalidate it halfway through.
        let sessions = self.live.lock().await;
        if !sessions
            .get(id)
            .is_some_and(|s| s.generation == live.generation)
        {
            return Ok(());
        }
        if message["params"]["threadId"]
            .as_str()
            .is_some_and(|t| t != live.thread)
        {
            return Ok(());
        }
        self.store.event(id, message)?;
        if method == "item/tool/call" && message.get("id").is_some() {
            // Never execute a tool delivered by a superseded RPC generation.
            let result = self
                .store
                .context_tool(id, &live.thread, &message["params"])
                .unwrap_or_else(|error| crate::session_context::response(Err(error)));
            drop(sessions);
            self.changed();
            live.rpc
                .send(json!({"id":message["id"],"result":result}))
                .await?;
            return Ok(());
        }
        if message.get("id").is_some() {
            self.store.request(id, &live.generation, message)?;
            self.store.status(id, "waiting", None)?;
        } else {
            match method {
                "turn/started" => {
                    **turn.as_mut().unwrap() =
                        message["params"]["turn"]["id"].as_str().map(str::to_string);
                    self.store.archive(id, false)?;
                    self.store.status(id, "working", None)?;
                }
                "turn/completed" => {
                    let turn = turn.as_mut().unwrap();
                    // A completion buffered during steering fallback belongs to
                    // the old turn; it must not clear a newly accepted turn.
                    if turn.is_none() || turn.as_deref() == message["params"]["turn"]["id"].as_str()
                    {
                        **turn = None;
                        self.store.status(id, "idle", None)?;
                    }
                }
                "serverRequest/resolved" => {
                    self.store
                        .resolve(&live.generation, &message["params"]["requestId"])?;
                }
                "thread/status/changed" => {
                    let status = message["params"]["status"]["type"]
                        .as_str()
                        .unwrap_or("connected");
                    if status != "idle" || turn.as_ref().unwrap().is_none() {
                        self.store.status(id, status, None)?;
                    }
                }
                "thread/tokenUsage/updated" => {
                    if message["params"]["threadId"].as_str() == Some(live.thread.as_str())
                        && sessions
                            .get(id)
                            .is_some_and(|s| s.generation == live.generation)
                    {
                        self.store
                            .context_usage(id, &message["params"]["tokenUsage"])?;
                    }
                }
                "thread/settings/updated" => {
                    let settings = &message["params"]["threadSettings"];
                    if settings["model"].is_string() {
                        let accepted = crate::controls::effective_settings(settings);
                        self.store.model_effective(id, &accepted)?;
                        let _ =
                            self.model_updates
                                .send((id.into(), live.generation.clone(), accepted));
                    }
                    let policy = &message["params"]["threadSettings"]["sandboxPolicy"];
                    if policy["type"].is_string() {
                        self.store.effective_sandbox(id, policy)?;
                        let _ = self.settings_updates.send((
                            id.into(),
                            live.generation.clone(),
                            policy.clone(),
                        ));
                    }
                }
                _ => {}
            }
        }
        self.changed();
        Ok(())
    }

    pub async fn prompt(&self, id: &str, text: &str) -> Result<Value> {
        let _settings = self.connecting.lock().await;
        anyhow::ensure!(
            !self.store.get(id)?.archived,
            "Restore the session before sending a message"
        );
        if text.trim().is_empty() {
            bail!("prompt must not be empty")
        }
        let live = self.runtime(id).await?;
        let session = self.store.get(id)?;
        // Serialize selection with turn notifications. An idle submission binds
        // current executor IDs; a busy submission steers that exact active turn.
        let mut turn = live.turn.lock().await;
        let client_message_id = uuid::Uuid::new_v4().to_string();
        let steered = if let Some(active_turn) = turn.as_ref() {
            match live
                .rpc
                .call(
                    "turn/steer",
                    json!({"threadId":live.thread,
                "expectedTurnId":active_turn,"input":[{"type":"text","text":text}],
                "clientUserMessageId":client_message_id}),
                )
                .await
            {
                Ok(result) => Some(result),
                Err(error) if crate::rpc::no_active_turn(&error) => {
                    // Codex explicitly confirmed non-delivery. Verify idle, since
                    // the same rejection may also describe a non-steerable mode.
                    let state = live
                        .rpc
                        .call(
                            "thread/read",
                            json!({"threadId":live.thread,"includeTurns":false}),
                        )
                        .await?;
                    if state["thread"]["status"]["type"] != "idle" {
                        return Err(error);
                    }
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let (result, method) = if let Some(result) = steered {
            (result, "demodex/promptSteered")
        } else {
            let result = live
                .rpc
                .call(
                    "turn/start",
                    json!({"threadId":live.thread,
                "clientUserMessageId":client_message_id,"input":[{"type":"text","text":text}],
                "environments":environment_params(&session.targets)}),
                )
                .await?;
            *turn = result["turn"]["id"].as_str().map(str::to_owned);
            self.store.targets_applied(id)?;
            (result, "demodex/promptAccepted")
        };
        drop(turn);
        self.store.event(id,&json!({"method":method,"params":{"text":text,"turnId":result.get("turnId").unwrap_or(&result["turn"]["id"]),"queuedSubmission":result["queuedSubmission"]}}))?;
        self.changed();
        Ok(result)
    }

    pub async fn queue_prompt(&self, id: &str, text: &str) -> Result<Value> {
        let _settings = self.connecting.lock().await;
        anyhow::ensure!(
            !self.store.get(id)?.archived,
            "Restore the session before queueing a message"
        );
        anyhow::ensure!(!text.trim().is_empty(), "Message must not be empty");
        anyhow::ensure!(
            !self.store.targets_pending(id)?,
            "Send a message to apply selected targets before queueing work"
        );
        let live = self.runtime(id).await?;
        let turn = live.turn.lock().await;
        anyhow::ensure!(
            turn.is_some(),
            "The session is idle. Use Send to start work; nothing was queued"
        );
        let result = live.rpc.call("thread/queue/add", json!({"threadId":live.thread,
            "input":[{"type":"text","text":text}],"clientUserMessageId":uuid::Uuid::new_v4().to_string()})).await?;
        drop(turn);
        self.store.event(id,&json!({"method":"demodex/promptQueued","params":{"text":text,"queuedSubmission":result["queuedSubmission"]}}))?;
        self.changed();
        Ok(result)
    }

    pub async fn queued(&self, id: &str) -> Result<Value> {
        let live = self.runtime(id).await?;
        let mut data = Vec::new();
        let mut cursor = Value::Null;
        let mut cursors = std::collections::HashSet::new();
        loop {
            let page = live
                .rpc
                .call(
                    "thread/queue/list",
                    json!({"threadId":live.thread,"limit":100,"cursor":cursor}),
                )
                .await?;
            data.extend(
                page["data"]
                    .as_array()
                    .context("Codex did not return a message queue")?
                    .iter()
                    .cloned(),
            );
            let next = page
                .get("nextCursor")
                .context("Missing message queue cursor")?
                .clone();
            if next.is_null() {
                break;
            }
            let key = next.as_str().context("Invalid message queue cursor")?;
            anyhow::ensure!(
                cursors.len() < 100 && cursors.insert(key.to_owned()) && data.len() < 10000,
                "Invalid message queue pagination"
            );
            cursor = next;
        }
        Ok(json!(data))
    }

    pub async fn cancel_queued(&self, id: &str, queued_id: &str) -> Result<Value> {
        let live = self.runtime(id).await?;
        let result = live
            .rpc
            .call(
                "thread/queue/delete",
                json!({"threadId":live.thread,"queuedSubmissionId":queued_id}),
            )
            .await?;
        self.changed();
        Ok(result)
    }

    pub async fn resume_queue(&self, id: &str) -> Result<Value> {
        let _settings = self.connecting.lock().await;
        anyhow::ensure!(
            !self.store.get(id)?.archived,
            "Restore the session before resuming queued work"
        );
        anyhow::ensure!(
            !self.store.targets_pending(id)?,
            "Send a message to apply the selected targets before resuming the queue"
        );
        let live = self.runtime(id).await?;
        let mut turn = live.turn.lock().await;
        anyhow::ensure!(
            turn.is_none(),
            "Wait for the active turn before resuming the queue"
        );
        let result = live
            .rpc
            .call("thread/queue/start", json!({"threadId":live.thread}))
            .await?;
        *turn = result["turn"]["id"].as_str().map(str::to_owned);
        self.changed();
        Ok(result)
    }

    pub async fn change_sandbox(
        &self,
        id: &str,
        sandbox: Option<crate::store::Sandbox>,
    ) -> Result<()> {
        let _settings = self.connecting.lock().await;
        self.store.get(id)?;
        if let Ok(live) = self.runtime(id).await {
            let snapshot = live
                .rpc
                .call(
                    "thread/read",
                    json!({"threadId":live.thread,"includeTurns":false}),
                )
                .await?;
            if snapshot["thread"]["status"]["type"] != "idle" || live.turn.lock().await.is_some() {
                bail!("finish or interrupt the current turn before changing its sandbox");
            }
            let policy = match sandbox {
                Some(crate::store::Sandbox::ReadOnly) => {
                    json!({"type":"readOnly","networkAccess":false})
                }
                Some(crate::store::Sandbox::WorkspaceWrite) => {
                    json!({"type":"workspaceWrite","networkAccess":false,"writableRoots":[],"excludeSlashTmp":false,"excludeTmpdirEnvVar":false})
                }
                Some(crate::store::Sandbox::DangerFullAccess) => json!({"type":"dangerFullAccess"}),
                None => self.store.get(id)?.effective_sandbox.unwrap_or(Value::Null),
            };
            if sandbox.is_some() && self.store.get(id)?.effective_sandbox.as_ref() == Some(&policy)
            {
                // Codex emits no notification for an identical policy. Only an
                // already reported effective policy justifies this no-op; the
                // saved override alone is not evidence of the runtime setting.
                self.store.sandbox(id, sandbox)?;
                self.store.effective_sandbox(id, &policy)?;
                self.changed();
                return Ok(());
            }
            if sandbox.is_some() {
                // The RPC only acknowledges the update. Codex reports the accepted
                // settings separately; subscribe before sending to catch early events.
                let mut settings = self.settings_updates.subscribe();
                if let Err(error) = live
                    .rpc
                    .call(
                        "thread/settings/update",
                        json!({"threadId":live.thread,"sandboxPolicy":policy}),
                    )
                    .await
                {
                    // Transport loss can happen after Codex applied the policy.
                    // Only an explicit rejection preserves the previously accepted state.
                    if error.downcast_ref::<crate::rpc::RemoteError>().is_none() {
                        self.store.effective_sandbox(id, &Value::Null)?;
                        self.changed();
                    }
                    return Err(error);
                }
                self.store.sandbox(id, sandbox)?; // Effective state is unknown until confirmed.
                self.changed();
                let accepted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let (session, generation, accepted) = settings.recv().await?;
                        if session == id && generation == live.generation {
                            return Ok::<Value, anyhow::Error>(accepted);
                        }
                    }
                }).await.context("Sandbox update was acknowledged, but Codex did not confirm its policy; active sandbox is unconfirmed")??;
                self.store.effective_sandbox(id, &accepted)?;
                self.changed();
                anyhow::ensure!(
                    accepted["type"] == policy["type"],
                    "Codex reported sandbox {} instead of requested {}",
                    accepted["type"],
                    policy["type"]
                );
                return Ok(());
            }
            // No override preserves the current thread policy without changing Codex.
            self.store.sandbox(id, sandbox)?;
            self.store.effective_sandbox(id, &policy)?;
            self.changed();
            return Ok(());
        }
        self.store.sandbox(id, sandbox)?;
        self.changed();
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> Result<Value> {
        let live = self.runtime(id).await?;
        let turn = live
            .turn
            .lock()
            .await
            .clone()
            .context("no known active turn")?;
        live.rpc
            .call(
                "turn/interrupt",
                json!({"threadId":live.thread,"turnId":turn}),
            )
            .await
    }

    pub async fn answer(&self, id: &str, key: &str, result: Value) -> Result<()> {
        let live = self.runtime(id).await?;
        let request = self
            .store
            .pending(id)?
            .into_iter()
            .find(|p| p.key == key)
            .context("request not found")?;
        validate_answer(&request.method, &request.params, &result)?;
        let rpc_id = self.store.claim(id, key, &live.generation)?;
        self.store.event(
            id,
            &json!({"method":"demodex/responseSubmitted","params":{"key":key,"result":result}}),
        )?;
        self.changed();
        let outcome = live.rpc.send(json!({"id":rpc_id,"result":result})).await;
        self.store.request_state(
            key,
            if outcome.is_ok() {
                "delivered"
            } else {
                "unavailable"
            },
        )?;
        self.changed();
        outcome
    }
}

fn validate_answer(method: &str, params: &Value, result: &Value) -> Result<()> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            anyhow::ensure!(
                matches!(
                    result["decision"].as_str(),
                    Some("accept" | "decline" | "cancel")
                ),
                "choose approve once, decline, or cancel"
            );
        }
        "item/tool/requestUserInput" => {
            for question in params["questions"]
                .as_array()
                .context("malformed questions")?
            {
                let id = question["id"].as_str().context("missing question ID")?;
                let values = result["answers"][id]["answers"]
                    .as_array()
                    .context("every question requires an explicit answer")?;
                anyhow::ensure!(
                    !values.is_empty()
                        && values
                            .iter()
                            .all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())),
                    "answers cannot be empty"
                );
            }
        }
        _ => {
            anyhow::ensure!(result.is_object(), "protocol response must be an object");
        }
    }
    Ok(())
}

fn environment_params(targets: &[Target]) -> Value {
    // Empty is intentional: never silently fall back to execution on the agent host.
    json!(
        targets
            .iter()
            .map(|t| json!({"environmentId":t.id,"cwd":t.cwd}))
            .collect::<Vec<_>>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn old_completion_cannot_clear_the_fallback_turn() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: Value = serde_json::from_str(&raw).unwrap();
                if request["method"] == "initialized" {
                    continue;
                }
                assert_eq!(request["method"], "initialize");
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":{}}).to_string().into(),
                ))
                .await
                .unwrap();
            }
        });
        let manager = Manager::new(Store::open(std::path::Path::new(":memory:"))?);
        let session = manager
            .store
            .create("test", &endpoint, &[], Some("thread"))?;
        let (rpc, _events) = Rpc::connect(&endpoint).await?;
        let live = Arc::new(Live {
            rpc,
            generation: "generation".into(),
            thread: "thread".into(),
            turn: Mutex::new(Some("new".into())),
        });
        manager
            .live
            .lock()
            .await
            .insert(session.id.clone(), live.clone());
        manager.store.status(&session.id, "working", None)?;
        manager.ingest(&session.id,&live,&json!({"method":"thread/status/changed","params":{"threadId":"thread","status":{"type":"idle"}}})).await?;
        assert_eq!(manager.store.get(&session.id)?.status, "working");
        manager.ingest(&session.id,&live,&json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"old"}}})).await?;
        assert_eq!(live.turn.lock().await.as_deref(), Some("new"));
        assert_eq!(manager.store.get(&session.id)?.status, "working");
        manager.ingest(&session.id,&live,&json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"new"}}})).await?;
        assert!(live.turn.lock().await.is_none());
        assert_eq!(manager.store.get(&session.id)?.status, "idle");
        manager.disconnect(&session.id, "done").await?;
        let count = manager.store.events(&session.id, 0)?.len();
        for message in [
            json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"stale"}}}),
            json!({"id":99,"method":"item/tool/requestUserInput","params":{"threadId":"thread"}}),
            json!({"method":"thread/status/changed","params":{"threadId":"thread","status":{"type":"idle"}}}),
        ] {
            manager.ingest(&session.id, &live, &message).await?;
        }
        assert_eq!(manager.store.get(&session.id)?.status, "disconnected");
        assert!(manager.store.pending(&session.id)?.is_empty());
        assert_eq!(manager.store.events(&session.id, 0)?.len(), count);
        fake.await?;
        Ok(())
    }

    #[tokio::test]
    async fn queue_pagination_rejects_cycles_even_when_pages_are_empty() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let request: Value = serde_json::from_str(&raw).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "thread/queue/list" => {
                        json!({"data":[],"nextCursor":if request["params"]["cursor"] == "a" {"b"} else {"a"}})
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
        let manager = Manager::new(Store::open(std::path::Path::new(":memory:"))?);
        let session = manager.store.create("test", &url, &[], Some("thread"))?;
        let (rpc, _events) = Rpc::connect(&url).await?;
        manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(Live {
                rpc,
                generation: "fixture".into(),
                thread: "thread".into(),
                turn: Mutex::new(None),
            }),
        );
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            manager.queued(&session.id),
        )
        .await?
        .unwrap_err();
        assert!(error.to_string().contains("pagination"));
        manager.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
    }

    #[tokio::test]
    async fn active_turn_cannot_change_sandbox() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                if request["method"] == "initialized" {
                    continue;
                }
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "thread/read" => {
                        json!({"thread":{"status":{"type":"active","activeFlags":[]}}})
                    }
                    method => panic!("unexpected mutation during an active turn: {method}"),
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
        let manager = Manager::new(Store::open(std::path::Path::new(":memory:"))?);
        let session = manager.store.create("active", &url, &[], Some("thread"))?;
        let (rpc, _events) = Rpc::connect(&url).await?;
        manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(Live {
                rpc,
                generation: "fixture".into(),
                thread: "thread".into(),
                turn: Mutex::new(None),
            }),
        );
        manager
            .store
            .sandbox(&session.id, Some(crate::store::Sandbox::DangerFullAccess))?;
        let result = manager
            .change_sandbox(&session.id, Some(crate::store::Sandbox::DangerFullAccess))
            .await;
        assert!(result.unwrap_err().to_string().contains("current turn"));
        assert_eq!(
            manager.store.get(&session.id)?.sandbox,
            Some(crate::store::Sandbox::DangerFullAccess)
        );
        manager.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
    }
    async fn sandbox_confirmation_fixture(
        accepted: Option<Value>,
        response: &'static str,
    ) -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let reported = accepted.clone();
        let fake = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut updates = 0;
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let method = request["method"].as_str().unwrap();
                if method == "initialized" {
                    continue;
                }
                let result = match method {
                    "initialize" => json!({}),
                    "thread/resume" => {
                        json!({"thread":{"id":"thread","turns":[]},"sandbox":{"type":"readOnly","networkAccess":false}})
                    }
                    "thread/read" => json!({"thread":{"status":{"type":"idle"}}}),
                    "thread/settings/update" => {
                        updates += 1;
                        assert_eq!(
                            request["params"]["sandboxPolicy"]["type"],
                            "dangerFullAccess"
                        );
                        if response == "disconnect" {
                            break;
                        }
                        if response == "reject" {
                            ws.send(Message::Text(
                                json!({"id":request["id"],"error":{"message":"fixture rejection"}})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .unwrap();
                            continue;
                        }
                        // Unrelated threads must not confirm this session's update.
                        ws.send(Message::Text(json!({"method":"thread/settings/updated","params":{"threadId":"other-thread","threadSettings":{"sandboxPolicy":{"type":"dangerFullAccess"}}}}).to_string().into())).await.unwrap();
                        if let Some(policy) = &reported {
                            ws.send(Message::Text(json!({"method":"thread/settings/updated","params":{"threadId":"thread","threadSettings":{"sandboxPolicy":policy}}}).to_string().into())).await.unwrap();
                        }
                        json!({})
                    }
                    _ => panic!("unexpected RPC: {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            assert_eq!(updates, 1, "reapplying a saved override must contact Codex");
        });
        let manager = Manager::new(Store::open(std::path::Path::new(":memory:"))?);
        let session = manager
            .store
            .create("settings", &url, &[], Some("thread"))?;
        manager
            .store
            .sandbox(&session.id, Some(crate::store::Sandbox::DangerFullAccess))?;
        manager.connect(&session.id).await?;
        let result = manager
            .change_sandbox(&session.id, Some(crate::store::Sandbox::DangerFullAccess))
            .await;
        let current = manager.store.get(&session.id)?;
        if response == "disconnect" {
            assert!(result.is_err());
            assert!(current.effective_sandbox.is_none_or(|v| v.is_null()));
        } else if response == "reject" {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("fixture rejection")
            );
            assert_eq!(current.effective_sandbox.unwrap()["type"], "readOnly");
        } else if let Some(policy) = accepted {
            assert_eq!(current.effective_sandbox, Some(policy.clone()));
            assert_eq!(result.is_ok(), policy["type"] == "dangerFullAccess");
        } else {
            assert!(result.unwrap_err().to_string().contains("did not confirm"));
            assert!(current.effective_sandbox.is_none_or(|v| v.is_null()));
        }
        manager.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
    }
    #[tokio::test]
    async fn same_override_is_reapplied_and_confirmed_by_early_notification() -> Result<()> {
        sandbox_confirmation_fixture(Some(json!({"type":"dangerFullAccess"})), "accept").await
    }
    #[tokio::test]
    async fn reports_codex_policy_instead_of_requested_policy() -> Result<()> {
        sandbox_confirmation_fixture(
            Some(json!({"type":"readOnly","networkAccess":false})),
            "accept",
        )
        .await
    }
    #[tokio::test]
    async fn missing_confirmation_leaves_effective_policy_unknown() -> Result<()> {
        sandbox_confirmation_fixture(None, "accept").await
    }
    #[tokio::test]
    async fn rejected_update_preserves_previous_policy() -> Result<()> {
        sandbox_confirmation_fixture(None, "reject").await
    }
    #[tokio::test]
    async fn lost_sandbox_reply_leaves_effective_policy_unknown() -> Result<()> {
        sandbox_confirmation_fixture(None, "disconnect").await
    }
    #[test]
    fn questions_require_explicit_nonempty_answers() {
        let params = json!({"questions":[{"id":"workspace"}]});
        assert!(validate_answer("item/tool/requestUserInput", &params, &json!({})).is_err());
        assert!(
            validate_answer(
                "item/tool/requestUserInput",
                &params,
                &json!({"answers":{"workspace":{"answers":[""]}}})
            )
            .is_err()
        );
        assert!(
            validate_answer(
                "item/tool/requestUserInput",
                &params,
                &json!({"answers":{"workspace":{"answers":["scratch"]}}})
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn prompt_override_replaces_defaults_on_start_and_after_restart() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let fake = tokio::spawn(async move {
            for expected in ["thread/start", "thread/resume"] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let method = request["method"].as_str().unwrap();
                    if method == "initialized" { continue; }
                    let result = if method == "initialize" { json!({}) } else {
                        assert_eq!(method, expected);
                        assert_eq!(request["params"]["baseInstructions"], "Custom Codex base");
                        assert_eq!(request["params"]["developerInstructions"], "");
                        assert_eq!(request["params"]["config"]["project_doc_max_bytes"], 0);
                        json!({"thread":{"id":"thread","turns":[]},"sandbox":{"type":"readOnly","networkAccess":false}})
                    };
                    ws.send(Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).await.unwrap();
                }
            }
        });
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("db");
        let manager = Manager::new(Store::open(&path)?);
        let session = manager.store.create("custom", &url, &[], None)?;
        manager.store.save_prompt(&session.id, "Custom Codex base")?;
        manager.connect(&session.id).await?;
        manager.disconnect(&session.id, "restart").await?;
        let resumed = Manager::new(Store::open(&path)?);
        resumed.connect(&session.id).await?;
        resumed.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
    }
}
