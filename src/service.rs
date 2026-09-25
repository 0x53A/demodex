use crate::{App, controls, ssh, store};
use anyhow::{Context, Result, ensure};
use demodex_protocol::{
    CodexRecord, HostSession, NewEnvironment, NewSession, Operation, RegisterTarget, Response,
    SandboxChoice, SelectTargets, SelectedSession, SessionDetail, SessionName,
};
use serde::Deserialize;
use serde_json::{Value, json};

impl crate::Service {
    /// Execute a command in-process. Mutations require a stable UUID receipt ID.
    /// Authentication belongs to the caller; this handle carries daemon authority.
    pub async fn call(
        &self,
        request_id: &str,
        operation: Operation,
    ) -> Result<demodex_protocol::Response> {
        let _admission = self.lifecycle.admit().await?;
        execute(self, request_id, operation).await
    }
}

pub(crate) async fn dispatch(app: &App, operation: Operation) -> Result<Response> {
    use Operation::*;
    let value = match operation {
        Sessions => return Ok(Response::Sessions(list(app).await?)),
        Detail { id } => return Ok(Response::Detail(detail(app, id).await?)),
        Events { id, after } => return Ok(Response::Events(app.manager.store.events(&id, after)?)),
        Runtime => runtime_status(app).await?,
        DefaultPrompt => app.orchestrator.default_prompt().await?,
        CreateSessionWithPrompt { input, prompt } => {
            return Ok(Response::Session(app.orchestrator.selected_session(&input.name, &input.targets, input.sandbox, Some(&prompt)).await?));
        }
        HostSessionWithPrompt { input, prompt } => {
            ensure!(input.thread_id.is_none(), "Prompt overrides require a new thread");
            return Ok(Response::Session(app.orchestrator.host_session(&input.name, None, input.sandbox, input.cwd.as_deref(), Some(&prompt)).await?));
        }
        StartRuntime => runtime_start(app).await?,
        Login => runtime_login(app).await?,
        SavedThreads { cursor, search } => app.orchestrator.saved_threads(cursor, search).await?,
        CreateSession { input } => {
            return Ok(Response::Session(selected_session(app, input).await?));
        }
        HostSession { input } => return Ok(Response::Session(host_session(app, input).await?)),
        ExternalSession { input } => return Ok(Response::Session(create(app, input).await?)),
        Connect { id } => connect(app, id).await?,
        Archive { id, archived } => archive(app, id, ArchiveChoice { archived }).await?,
        Sandbox { id, input } => change_sandbox(app, id, input).await?,
        Models { id } => models(app, id).await?,
        Model { id, input } => change_model(app, id, input).await?,
        Goal { id, input } => change_goal(app, id, input).await?,
        Prompt { id, text } => app.manager.prompt(&id, &text).await?,
        QueuePrompt { id, text } => app.manager.queue_prompt(&id, &text).await?,
        CancelQueued { id, queued_id } => app.manager.cancel_queued(&id, &queued_id).await?,
        ResumeQueue { id } => app.manager.resume_queue(&id).await?,
        StopBackground {
            id,
            generation,
            processes,
        } => {
            app.manager
                .stop_background(&id, &generation, &processes)
                .await?
        }
        UploadImage { id, bytes } => {
            json!({"path":app.orchestrator.upload_image(&id, &bytes).await?})
        }
        Interrupt { id } => interrupt(app, id).await?,
        Answer { id, key, result } => {
            answer(
                app,
                id,
                self::Answer {
                    key,
                    result: result.0,
                },
            )
            .await?
        }
        Environments => return Ok(Response::Environments(environments(app).await?)),
        Targets => targets(app).await?,
        RegisterTarget { input } => register_target(app, input).await?,
        RegisterSshTarget { input } => register_ssh_target(app, input.into()).await?,
        CheckSshTarget { id } => check_ssh_target(app, id).await?,
        ReconnectSshTarget { id } => reconnect_ssh_target(app, id).await?,
        ForgetTarget { id } => forget_target(app, id).await?,
        SelectTargets { id, input } => select_targets(app, id, input).await?,
        CreateEnvironment { input } => {
            return Ok(Response::Environment(environment_create(app, input).await?));
        }
        StartEnvironment { id } => environment_start(app, id).await?,
        StopEnvironment { id } => environment_stop(app, id).await?,
        EnvironmentSession { id, input } => {
            return Ok(Response::Session(
                environment_session(app, id, input).await?,
            ));
        }
        Receipt { .. } => unreachable!("receipts are handled before dispatch"),
    };
    Ok(Response::Record(CodexRecord(value)))
}

pub async fn execute(app: &App, request_id: &str, operation: Operation) -> Result<Response> {
    if let Operation::Receipt { id } = &operation {
        return Ok(Response::Record(CodexRecord(
            match app.manager.store.receipt(id)? {
                None => json!({"state":"unknown"}),
                Some((_, None)) => {
                    json!({"state":"pending-or-interrupted","message":"Do not automatically retry this command."})
                }
                Some((previous, Some(response))) => {
                    let cached = if let Ok(stored) = serde_json::from_str::<StoredReply>(&response)
                    {
                        ensure!(stored.version == 16, "unknown receipt encoding");
                        stored.result.map(Response::into_value)
                    } else {
                        let _ = previous;
                        match serde_json::from_str::<Result<String, String>>(&response)? {
                            Ok(encoded) => Ok(serde_json::from_str::<Value>(&encoded)?),
                            Err(error) => Err(error),
                        }
                    };
                    json!({"state":"completed","result":cached})
                }
            },
        )));
    }
    if !operation.is_mutation() {
        return dispatch(app, operation).await;
    }
    ensure!(
        uuid::Uuid::parse_str(request_id).is_ok(),
        "a UUID request ID is required for commands"
    );
    // Receipts retain identity and a content digest, not a second copy of every image.
    let encoded = if let Operation::UploadImage { id, bytes } = &operation {
        use sha2::{Digest, Sha256};
        crate::uploads::extension(bytes)?;
        json!({"UploadImage":{"id":id,"sha256":Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()}}).to_string()
    } else {
        serde_json::to_string(&operation)?
    };
    let _lock = app.commands.lock().await;
    if let Some((previous, response)) = app.manager.store.receipt(request_id)? {
        ensure!(
            previous == encoded || same_legacy_command(&previous, &operation),
            "request ID already belongs to a different command"
        );
        let response = response
            .context("command outcome is uncertain after an interruption; refusing to replay it")?;
        if let Ok(stored) = serde_json::from_str::<StoredReply>(&response) {
            ensure!(stored.version == 16, "unknown receipt encoding");
            return stored.result.map_err(anyhow::Error::msg);
        }
        let encoded = serde_json::from_str::<Result<String, String>>(&response)?
            .map_err(anyhow::Error::msg)?;
        return Ok(Response::from_value(
            &operation,
            serde_json::from_str(&encoded)?,
        )?);
    }
    app.manager.store.begin_command(request_id, &encoded)?;
    drop(_lock);
    let result = dispatch(app, operation).await.map_err(|e| format!("{e:#}"));
    app.manager.store.finish_command(
        request_id,
        &serde_json::to_string(&StoredReply {
            version: 16,
            result: result.clone(),
        })?,
    )?;
    app.manager.changed();
    result.map_err(anyhow::Error::msg)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredReply {
    version: u32,
    result: Result<Response, String>,
}

// v14 receipts stored form DTOs and approval results as JSON strings. Normalize
// those old fields for equality only; never execute an interrupted receipt again.
fn same_legacy_command(previous: &str, operation: &Operation) -> bool {
    let Ok(mut value) = serde_json::from_str::<Value>(previous) else {
        return false;
    };
    let Some(variant) = value.as_object_mut().and_then(|v| v.values_mut().next()) else {
        return false;
    };
    for key in ["input", "result"] {
        if let Some(encoded) = variant.get(key).and_then(Value::as_str) {
            let Ok(decoded) = serde_json::from_str(encoded) else {
                return false;
            };
            variant[key] = decoded;
        }
    }
    serde_json::from_value::<Operation>(value).is_ok_and(|previous| previous == *operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn legacy_receipts_survive_restart_without_reexecution() -> Result<()> {
        let root = tempfile::tempdir()?;
        let open = || {
            crate::Service::open(crate::Config {
                data_dir: root.path().join("state"),
                host_workspace: None,
                codex_home: None,
                vm_image: None,
            })
        };
        let service = open()?;
        let input = json!({"name":"Legacy", "endpoint":"ws://127.0.0.1:1"});
        let operation = Operation::ExternalSession {
            input: serde_json::from_value(input.clone())?,
        };
        let created = service
            .manager
            .store
            .create("Legacy", "ws://127.0.0.1:1", &[], None)?;
        let completed = uuid::Uuid::new_v4().to_string();
        let pending = uuid::Uuid::new_v4().to_string();
        let legacy = json!({"ExternalSession":{"input": input.to_string()}}).to_string();
        service.manager.store.begin_command(&completed, &legacy)?;
        service.manager.store.finish_command(
            &completed,
            &serde_json::to_string(&Ok::<_, String>(serde_json::to_string(&created)?))?,
        )?;
        service.manager.store.begin_command(&pending, &legacy)?;
        drop(service);
        let service = open()?;
        let response = service.call(&completed, operation.clone()).await?;
        assert_eq!(response.into_value()["id"], created.id);
        assert!(
            service
                .call(&pending, operation)
                .await
                .unwrap_err()
                .to_string()
                .contains("refusing to replay")
        );
        assert_eq!(service.manager.store.list()?.len(), 1);
        Ok(())
    }
}

async fn list(app: &App) -> Result<Vec<store::Session>> {
    app.manager.store.list()
}

async fn create(app: &App, input: NewSession) -> Result<store::Session> {
    if input.name.trim().is_empty()
        || !(input.endpoint.starts_with("ws://") || input.endpoint.starts_with("unix:///"))
    {
        return Err(anyhow::anyhow!(
            "name and ws:// or absolute unix:// app-server endpoint are required"
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for t in &input.targets {
        if t.id.is_empty()
            || !ids.insert(&t.id)
            || !t.cwd.starts_with('/')
            || !t.url.starts_with("ws://")
        {
            return Err(anyhow::anyhow!(
                "targets need unique IDs, absolute working directories and ws:// executor URLs"
            ));
        }
    }
    let session = app.manager.store.create(
        input.name.trim(),
        &input.endpoint,
        &input.targets,
        input.thread_id.as_deref().filter(|s| !s.is_empty()),
    )?;
    app.manager.store.ensure_target_selection(&session.id)?;
    app.manager.changed();
    app.manager.store.sandbox(&session.id, input.sandbox)?;
    app.manager.store.get(&session.id)
}
async fn detail(app: &App, id: String) -> Result<SessionDetail> {
    app.manager.store.ensure_target_selection(&id)?;
    let session = app.manager.store.get(&id)?;
    let (queued, queue_error) = match app.manager.queued(&id).await {
        Ok(queued) => (queued, Value::Null),
        Err(error) => (Value::Null, json!(format!("{error:#}"))),
    };
    let controls = app.manager.control_snapshot(&id).await?;
    let background = app.manager.background_snapshot(&id).await;
    Ok(SessionDetail {
        session,
        pending: app.manager.store.pending(&id)?,
        queued,
        queue_error,
        controls,
        background,
        target_selection: app.manager.store.target_selection(&id)?,
        targets_pending: app.manager.store.targets_pending(&id)?,
    })
}

async fn targets(app: &App) -> Result<Value> {
    app.orchestrator.targets().await
}

async fn register_target(app: &App, input: RegisterTarget) -> Result<Value> {
    let result = app
        .manager
        .store
        .register_target(&input.name, &input.url, &input.cwd)?;
    app.manager.changed();
    Ok(json!(result))
}
async fn register_ssh_target(app: &App, input: ssh::Config) -> Result<Value> {
    app.orchestrator.register_ssh_target(input).await
}
async fn reconnect_ssh_target(app: &App, id: String) -> Result<Value> {
    app.orchestrator.reconnect_ssh_target(&id).await?;
    Ok(json!({"ok":true}))
}
async fn check_ssh_target(app: &App, id: String) -> Result<Value> {
    app.orchestrator.check_ssh_target(&id).await?;
    Ok(json!({"ok":true}))
}
async fn forget_target(app: &App, id: String) -> Result<Value> {
    app.orchestrator.forget_target(&id).await?;
    Ok(json!({"ok":true}))
}

async fn select_targets(app: &App, id: String, input: SelectTargets) -> Result<Value> {
    app.orchestrator.select_targets(&id, &input.targets).await?;
    Ok(json!({"ok":true,"applies_on_next_message":true}))
}
#[derive(Deserialize)]
struct ArchiveChoice {
    archived: bool,
}
async fn archive(app: &App, id: String, input: ArchiveChoice) -> Result<Value> {
    app.manager.archive(&id, input.archived).await?;
    Ok(json!({"archived":input.archived}))
}
async fn connect(app: &App, id: String) -> Result<Value> {
    app.orchestrator.connect_session(&id).await?;
    Ok(json!({"ok":true}))
}

async fn change_sandbox(app: &App, id: String, input: SandboxChoice) -> Result<Value> {
    app.manager.change_sandbox(&id, input.sandbox).await?;
    app.orchestrator.connect_session(&id).await?;
    Ok(json!({"ok":true}))
}
async fn models(app: &App, id: String) -> Result<Value> {
    app.manager.model_catalog(&id).await
}
async fn change_model(app: &App, id: String, input: controls::ModelChoice) -> Result<Value> {
    app.manager.change_model(&id, input).await
}
async fn change_goal(app: &App, id: String, input: controls::GoalAction) -> Result<Value> {
    app.manager.change_goal(&id, input).await
}

async fn runtime_status(app: &App) -> Result<Value> {
    app.orchestrator.runtime_status().await
}
async fn runtime_start(app: &App) -> Result<Value> {
    app.orchestrator.start_runtime().await
}
async fn runtime_login(app: &App) -> Result<Value> {
    app.orchestrator.login().await
}

async fn selected_session(app: &App, input: SelectedSession) -> Result<store::Session> {
    app.orchestrator
        .selected_session(&input.name, &input.targets, input.sandbox, None)
        .await
}

async fn host_session(app: &App, input: HostSession) -> Result<store::Session> {
    app.orchestrator
        .host_session(
            &input.name,
            input.thread_id.as_deref().filter(|s| !s.is_empty()),
            input.sandbox,
            input
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            None,
        )
        .await
}
async fn environments(app: &App) -> Result<Vec<store::Environment>> {
    app.manager.store.environments()
}

async fn environment_create(app: &App, input: NewEnvironment) -> Result<store::Environment> {
    let environment =
        app.orchestrator
            .create(&input.name, input.memory_mib, input.cpus, input.internet)?;
    app.orchestrator.start(&environment.id).await?;
    app.manager.store.environment(&environment.id)
}
async fn environment_start(app: &App, id: String) -> Result<Value> {
    app.orchestrator.start(&id).await?;
    Ok(json!({"ok":true}))
}
async fn environment_stop(app: &App, id: String) -> Result<Value> {
    app.orchestrator.stop(&id).await?;
    Ok(json!({"ok":true}))
}

async fn environment_session(app: &App, id: String, input: SessionName) -> Result<store::Session> {
    app.orchestrator
        .session(&id, &input.name, input.sandbox)
        .await
}
async fn interrupt(app: &App, id: String) -> Result<Value> {
    app.manager.interrupt(&id).await
}
#[derive(Deserialize)]
struct Answer {
    key: String,
    result: Value,
}
async fn answer(app: &App, id: String, input: Answer) -> Result<Value> {
    app.manager.answer(&id, &input.key, input.result).await?;
    Ok(json!({"ok":true}))
}
