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
    connecting: Mutex<()>,
    pub updates: broadcast::Sender<()>,
}

impl Manager {
    pub fn new(store: Store) -> Arc<Self> {
        Arc::new(Self {
            store,
            live: Mutex::new(HashMap::new()),
            connecting: Mutex::new(()),
            updates: broadcast::channel(64).0,
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

    pub async fn disconnect(&self,id:&str,reason:&str)->Result<()> {
        if let Some(live)=self.live.lock().await.remove(id) {
            live.rpc.close();
            self.store.disconnected(id,&live.generation)?;
        }
        self.store.status(id,"disconnected",Some(reason))?;self.changed();Ok(())
    }

    pub async fn connect(self: &Arc<Self>, id: &str) -> Result<()> {
        let _connecting = self.connecting.lock().await;
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
            let mut active_turn=None;
            let thread=if let Some(thread)=&session.thread_id {
                let result=rpc.call("thread/resume",json!({"threadId":thread,"cwd":cwd,"sandbox":session.sandbox,"approvalPolicy":"on-request","approvalsReviewer":"user"})).await?;
                self.store.effective_sandbox(id, &result["sandbox"])?;
                // Preserve the snapshot so a newly imported conversation has its existing history.
                self.store.event(id,&json!({"method":"demodex/threadSnapshot","params":result}))?;
                active_turn=result["thread"]["turns"].as_array().and_then(|turns|turns.iter().rev().find(|t|t["status"]=="inProgress")).and_then(|t|t["id"].as_str()).map(str::to_string);
                result["thread"]["id"].as_str().context("missing resumed thread id")?.to_string()
            } else {
                let result=rpc.call("thread/start",json!({"environments":environments,"cwd":cwd,"sandbox":session.sandbox,"approvalPolicy":"on-request","approvalsReviewer":"user"})).await?;
                self.store.effective_sandbox(id, &result["sandbox"])?;
                let thread=result["thread"]["id"].as_str().context("missing new thread id")?.to_string();
                self.store.thread(id,&thread)?;
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
                drop(sessions);
                let _=manager.store.disconnected(&id,&live.generation);
                if current { let _=manager.store.status(&id,"disconnected",Some(&failure)); }
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
        if message["params"]["threadId"]
            .as_str()
            .is_some_and(|t| t != live.thread)
        {
            return Ok(());
        }
        self.store.event(id, message)?;
        let method = message["method"].as_str().unwrap_or("");
        if message.get("id").is_some() {
            self.store.request(id, &live.generation, message)?;
            self.store.status(id, "waiting", None)?;
        } else {
            match method {
                "turn/started" => {
                    *live.turn.lock().await =
                        message["params"]["turn"]["id"].as_str().map(str::to_string);
                    self.store.status(id, "working", None)?;
                }
                "turn/completed" => {
                    *live.turn.lock().await = None;
                    self.store.status(id, "idle", None)?;
                }
                "serverRequest/resolved" => {
                    self.store
                        .resolve(&live.generation, &message["params"]["requestId"])?;
                }
                "thread/status/changed" => {
                    let status = message["params"]["status"]["type"]
                        .as_str()
                        .unwrap_or("connected");
                    self.store.status(id, status, None)?;
                }
                "thread/settings/updated" => {
                    self.store.effective_sandbox(id, &message["params"]["threadSettings"]["sandboxPolicy"])?;
                }
                _ => {}
            }
        }
        self.changed();
        Ok(())
    }

    pub async fn prompt(&self, id: &str, text: &str) -> Result<Value> {
        let _settings = self.connecting.lock().await;
        if text.trim().is_empty() {
            bail!("prompt must not be empty")
        }
        let live = self.runtime(id).await?;
        let session = self.store.get(id)?;
        let result=live.rpc.call("turn/start",json!({"threadId":live.thread,"input":[{"type":"text","text":text}],"environments":environment_params(&session.targets)})).await?;
        self.store.event(id,&json!({"method":"demodex/promptAccepted","params":{"text":text,"turnId":result["turn"]["id"]}}))?;
        self.changed();
        Ok(result)
    }

    pub async fn change_sandbox(&self, id: &str, sandbox: Option<crate::store::Sandbox>) -> Result<()> {
        let _settings = self.connecting.lock().await;
        if self.store.get(id)?.sandbox == sandbox {return Ok(());}
        if let Ok(live) = self.runtime(id).await {
            let snapshot = live.rpc.call("thread/read", json!({"threadId":live.thread,"includeTurns":false})).await?;
            if snapshot["thread"]["status"]["type"] != "idle" || live.turn.lock().await.is_some() {
                bail!("finish or interrupt the current turn before changing its sandbox");
            }
            let policy = match sandbox {
                Some(crate::store::Sandbox::ReadOnly) => json!({"type":"readOnly","networkAccess":false}),
                Some(crate::store::Sandbox::WorkspaceWrite) => json!({"type":"workspaceWrite","networkAccess":false,"writableRoots":[],"excludeSlashTmp":false,"excludeTmpdirEnvVar":false}),
                Some(crate::store::Sandbox::DangerFullAccess) => json!({"type":"dangerFullAccess"}),
                None => self.store.get(id)?.effective_sandbox.unwrap_or(Value::Null),
            };
            if sandbox.is_some() {
                live.rpc.call("thread/settings/update", json!({"threadId":live.thread,"sandboxPolicy":policy})).await?;
            }
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
                if request["method"] == "initialized" {continue;}
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "thread/read" => json!({"thread":{"status":{"type":"active","activeFlags":[]}}}),
                    method => panic!("unexpected mutation during an active turn: {method}"),
                };
                ws.send(Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).await.unwrap();
            }
        });
        let manager = Manager::new(Store::open(std::path::Path::new(":memory:"))?);
        let session = manager.store.create("active", &url, &[], Some("thread"))?;
        let (rpc, _events) = Rpc::connect(&url).await?;
        manager.live.lock().await.insert(session.id.clone(), Arc::new(Live {
            rpc, generation:"fixture".into(), thread:"thread".into(), turn:Mutex::new(None),
        }));
        let result = manager.change_sandbox(&session.id, Some(crate::store::Sandbox::DangerFullAccess)).await;
        assert!(result.unwrap_err().to_string().contains("current turn"));
        assert_eq!(manager.store.get(&session.id)?.sandbox, None);
        manager.disconnect(&session.id, "done").await?;
        fake.await?;
        Ok(())
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
}
