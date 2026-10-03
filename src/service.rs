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
        PushSettings { device_id } => crate::notifications::settings(&app.manager.store, &device_id)?,
        RegisterPush { input } => crate::notifications::register(&app.manager.store, input)?,
        RemovePush { device_id } => crate::notifications::remove(&app.manager.store, &device_id)?,
        TestPush { device_id } => crate::notifications::test(&app.manager.store, &device_id).await?,
        Sessions => return Ok(Response::Sessions(list(app).await?)),
        Detail { id } => return Ok(Response::Detail(detail(app, id).await?)),
        Events { id, after } => return Ok(Response::Events(app.manager.store.events(&id, after)?)),
        Runtime => runtime_status(app).await?,
        RuntimeModels => {
            let (rpc, _) = app.orchestrator.runtime_rpc().await?;
            crate::controls::model_catalog(&rpc).await?
        }
        PromptSettings => app.orchestrator.prompt_settings_view().await?,
        SavePromptSettings { expected_revision, settings } => {
            let revision=app.manager.store.set_prompt_settings(expected_revision,&settings)?;
            app.manager.changed();
            json!({"revision":revision})
        }
        InstructionFiles { target } => tokio::time::timeout(std::time::Duration::from_secs(15), app.orchestrator.instruction_files(target)).await.context("Instruction preview timed out")??,
        SessionPromptSettings { id } => app.orchestrator.session_prompt_view(&id)?,
        ApplySessionPromptSettings { id, include_project } => app.orchestrator.apply_session_prompt(&id,include_project).await?,
        DefaultPrompt => app.orchestrator.default_prompt().await?,
        CreateSessionWithPrompt { input, prompt } => {
            return Ok(Response::Session(app.orchestrator.selected_session(&input.name, &input.targets, input.sandbox, Some(&prompt), input.include_project, input.model.as_ref()).await?));
        }
        HostSessionWithPrompt { input, prompt } => {
            ensure!(input.thread_id.is_none(), "Prompt overrides require a new thread");
            return Ok(Response::Session(app.orchestrator.host_session(&input.name, None, input.sandbox, input.cwd.as_deref(), Some(&prompt)).await?));
        }
        StartRuntime => runtime_start(app).await?,
        SetRuntimeFeature { name, enabled } => app.orchestrator.set_runtime_feature(&name, enabled).await?,
        RestartRuntime => app.orchestrator.restart_runtime().await?,
        Login => runtime_login(app).await?,
        SavedThreads { cursor, search } => app.orchestrator.saved_threads(cursor, search).await?,
        CreateSession { input } => {
            return Ok(Response::Session(selected_session(app, input).await?));
        }
        HostSession { input } => return Ok(Response::Session(host_session(app, input).await?)),
        ExternalSession { input } => return Ok(Response::Session(create(app, input).await?)),
        Connect { id } => connect(app, id).await?,
        StarSession { id, starred } => {
            app.manager.store.star(&id, starred)?;
            json!({"ok":true})
        }
        ReorderSessions { expected, ids } => {
            app.manager.store.reorder_sessions(&expected, &ids)?;
            json!({"ok":true})
        }
        MoveSession { id, neighbor } => {
            app.manager.store.move_session(&id, &neighbor)?;
            json!({"ok":true})
        }
        RenameSession { id, name } => {
            app.manager.store.rename(&id, &name)?;
            json!({"ok":true})
        }
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
        MessageFiles { id, item } => crate::message_files::listing(&app.manager, &id, &item).await?,
        ReadMessageFile { id, item, destination, executor } => crate::message_files::read(&app.manager, &id, &item, &destination, &executor).await?,
        UploadFile { id, name, data } => {
            let bytes = crate::uploads::decode_file(&name, &data)?;
            json!({"path":app.orchestrator.upload_file(&id, &bytes, &name, false).await?})
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
        BrowseDirectories { target, path } => tokio::time::timeout(std::time::Duration::from_secs(15), app.orchestrator.browse_directories(&target, &path)).await.context("Directory listing timed out")??,
        RegisterTarget { input } => register_target(app, input).await?,
        RegisterSshTarget { input } => register_ssh_target(app, input.into()).await?,
        RegisterSessionSshTarget { id, input } => app.orchestrator.register_session_ssh_target(&id, input.into()).await?,
        CheckSshTarget { id } => check_ssh_target(app, id).await?,
        ReconnectSshTarget { id } => reconnect_ssh_target(app, id).await?,
        ForgetTarget { id } => forget_target(app, id).await?,
        SelectTargets { id, input } => select_targets(app, id, input).await?,
        ChangeTargets { id, input, mode } => {
            app.orchestrator.change_targets(&id, &input.targets, mode).await?;
            json!({"ok":true,"applies_on_next_message":true})
        }
        CreateContainer { input } => app.orchestrator.create_container(input).await?,
        StartContainer { id } => {
            app.orchestrator.start_container(&id).await?;
            json!({"ok":true})
        }
        StopContainer { id } => {
            app.orchestrator.stop_container(&id).await?;
            json!({"ok":true})
        }
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
    } else if let Operation::UploadFile { id, name, data } = &operation {
        use sha2::{Digest, Sha256};
        let bytes = crate::uploads::decode_file(name, data)?;
        json!({"UploadFile":{"id":id,"name":name,"sha256":Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()}}).to_string()
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
    let prompt_session = match &operation {
        Operation::Prompt { id, .. } => Some(id.clone()),
        _ => None,
    };
    let result = dispatch(app, operation).await.map_err(|e| format!("{e:#}"));
    app.manager.store.finish_command(
        request_id,
        &serde_json::to_string(&StoredReply {
            version: 16,
            result: result.clone(),
        })?,
    )?;
    if let Some(id) = prompt_session { app.manager.session_changed(&id, true); }
    else { app.manager.changed(); }
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
    let mut controls = app.manager.control_snapshot(&id).await?;
    controls["prompts"]=app.orchestrator.session_prompt_view(&id)?;
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
    app.orchestrator.change_session_model(&id, input).await
}
async fn change_goal(app: &App, id: String, input: controls::GoalAction) -> Result<Value> {
    app.manager.change_goal(&id, input).await
}

async fn runtime_status(app: &App) -> Result<Value> {
    use futures_util::StreamExt;
    let mut status = app.orchestrator.runtime_status().await?;
    let (revision,prompts)=app.manager.store.prompt_settings()?;
    status["prompt_defaults"]=json!({"revision":revision,"include_project":prompts.include_project});
    let sessions = app.manager.store.list()?;
    let managed = app.orchestrator.restart_session_ids().await.unwrap_or_default();
    let mut counts = serde_json::Map::new();
    let reads = futures_util::stream::iter(sessions.clone().into_iter().filter(|s| !s.archived).map(|session| async move {
        let (snapshot, controls, queue) = tokio::join!(app.manager.background_snapshot(&session.id), app.manager.control_snapshot(&session.id), app.manager.queued(&session.id));
        let controls = controls.unwrap_or_else(|error|json!({"goal":null,"goalError":error.to_string()}));
        let count = snapshot["data"].as_array().map(Vec::len);
        let subagents = match app.manager.runtime(&session.id).await { Ok(live)=>Some(live.subagents.lock().await.active()), Err(_)=>None };
        (session.id.clone(), json!({"count":count,"active_subagents":subagents,"error":snapshot["error"],"goal":controls["goal"],"goal_error":controls["goalError"],"queued_count":queue.ok().and_then(|q|q.as_array().map(Vec::len)),"pending_count":app.manager.store.pending(&session.id).ok().map(|items|items.iter().filter(|p|matches!(p.state.as_str(),"pending"|"responding"|"delivered")).count())}))
    })).buffer_unordered(4);
    tokio::pin!(reads);
    // Keep an unresponsive runtime from blocking the entire overview.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while let Some((id, value)) = reads.next().await { counts.insert(id, value); }
    }).await;
    let mut active = 0;
    let mut goals = 0;
    let mut queued = 0;
    let mut pending = 0;
    let mut background = 0;
    let mut unknown = 0;
    for session in sessions.iter().filter(|s| managed.contains(&s.id) && s.status != "disconnected") {
        if !matches!(session.status.as_str(),"idle"|"connected") { active += 1; }
        let activity = counts.get(&session.id).cloned().unwrap_or(Value::Null);
        if activity["goal"]["status"] == "active" { goals += 1; }
        queued += activity["queued_count"].as_u64().unwrap_or(0);
        pending += activity["pending_count"].as_u64().unwrap_or(0);
        background += activity["count"].as_u64().unwrap_or(0);
        if activity["count"].is_null() || activity["queued_count"].is_null() || activity["pending_count"].is_null() || !activity["goal_error"].is_null() { unknown += 1; }
    }
    status["restart_blockers"] = json!({"active_sessions":active,"active_goals":goals,"queued_messages":queued,"pending_decisions":pending,"background_terminals":background,"unavailable_sessions":unknown});
    status["background_terminals"] = json!(counts);
    Ok(status)
}
async fn runtime_start(app: &App) -> Result<Value> {
    app.orchestrator.start_runtime().await
}
async fn runtime_login(app: &App) -> Result<Value> {
    app.orchestrator.login().await
}

async fn selected_session(app: &App, input: SelectedSession) -> Result<store::Session> {
    app.orchestrator
        .selected_session(&input.name, &input.targets, input.sandbox, None, input.include_project, input.model.as_ref())
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
