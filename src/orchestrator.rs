use crate::{
    manager::Manager,
    rpc::Rpc,
    store::{Environment, Session, Target},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
    task::JoinHandle,
};

struct Runtime {
    child: Child,
    executor: Option<Child>,
    host_target: Option<Target>,
    rpc: Arc<Rpc>,
    endpoint: String,
    feature_overrides: BTreeMap<String, bool>,
    feature_catalog: Value,
}

pub(crate) fn validate_prompt(prompt: Option<&str>) -> Result<()> {
    ensure!(prompt.is_none_or(|text| text.len() <= 262144 && !text.contains('\0')), "Prompt must be at most 262144 UTF-8 bytes without NUL characters");
    Ok(())
}
struct Machine {
    child: Child,
    tunnel: Option<Child>,
    ssh_port: u16,
    target: Option<Target>,
}

struct ContainerRuntime {
    target: Target,
}

pub struct Orchestrator {
    pub(crate) manager: Arc<Manager>,
    pub(crate) root: PathBuf,
    pub(crate) host_workspace: Option<PathBuf>,
    pub(crate) codex_home: Option<PathBuf>,
    image: Mutex<Option<PathBuf>>,
    runtime: Mutex<Option<Runtime>>,
    usage: crate::usage::RateLimitsCache,
    machines: Mutex<HashMap<String, Machine>>,
    containers: Mutex<HashMap<String, ContainerRuntime>>,
    ssh: Mutex<HashMap<String, crate::ssh::Engine>>,
    jobs: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl Orchestrator {
    pub async fn browse_directories(&self, target: &str, path: &str) -> Result<Value> {
        crate::targets::validate_cwd(path)?;
        if target == "host" {
            ensure!(self.host_workspace.is_some(), "Host filesystem browsing is not enabled");
            return crate::directories::local(path).await;
        }
        if target.starts_with("ssh-") {
            let config = self.manager.store.ssh_targets()?.into_iter().find(|(id,_)|id==target).context("Unknown SSH target")?.1;
            config.validate()?;
            let mut sftp = crate::ssh::sftp::Sftp::connect(&config).await?;
            let path = sftp.realpath(path).await?;
            let listing = sftp.call("fs/readDirectory", &json!({"path":crate::ssh::sftp::path_uri(&path)})).await?;
            return crate::directories::listing(&path, &listing);
        }
        let resolved = self.resolve_targets(&[crate::targets::Selection { id:target.into(), cwd:path.into() }]).await?;
        crate::directories::remote(&resolved[0].url, path).await
    }

    pub async fn targets(&self) -> Result<Value> {
        for session in self.manager.store.list()? {
            self.manager.store.ensure_target_selection(&session.id)?;
        }
        let mut targets = Vec::new();
        if let Some(cwd) = &self.host_workspace {
            let ready = self.runtime_rpc().await.is_ok();
            targets.push(json!({"id":"host","name":"This host","kind":"host","cwd":cwd,"available":ready,"users":self.manager.store.target_users("host")?}));
        }
        let machines = self.machines.lock().await;
        for vm in self.manager.store.environments()? {
            let id = format!("vm-{}", vm.id);
            targets.push(json!({"id":id,"name":vm.name,"kind":"vm","cwd":"/workspace","available":vm.status=="running" && machines.get(&vm.id).is_some_and(|m|m.target.is_some()),"environment_id":vm.id,"users":self.manager.store.target_users(&id)?}));
        }
        let containers = self.containers.lock().await;
        for container in self.manager.store.containers()? {
            let id = format!("container-{}", container.id);
            targets.push(json!({"id":id,"name":container.name,"kind":"container","cwd":"/workspace","engine":container.engine,"image":container.image,"status":container.status,"error":container.error,"available":container.status=="running" && containers.contains_key(&container.id),"users":self.manager.store.target_users(&id)?}));
        }
        for target in self.manager.store.registered_targets()? {
            targets.push(json!({"id":target.id,"name":target.name,"kind":"external","cwd":target.cwd,"url":target.url,"available":true,"users":self.manager.store.target_users(&target.id)?}));
        }
        for (id, config) in self.manager.store.ssh_targets()? {
            let owner = self.manager.store.ssh_target_owner(&id)?;
            targets.push(json!({"id":id,"name":config.name,"kind":"ssh","cwd":config.cwd,"destination":config.destination,"port":config.port,"identity_file":config.identity_file,"available":true,"users":self.manager.store.target_users(&id)?,"owner":owner}));
        }
        Ok(json!(targets))
    }

    pub(crate) async fn resolve_targets(
        &self,
        selection: &[crate::targets::Selection],
    ) -> Result<Vec<Target>> {
        crate::targets::validate_selection(selection)?;
        let registered = self.manager.store.registered_targets()?;
        let mut targets = Vec::new();
        for chosen in selection {
            let mut target = if chosen.id == "host" {
                self.runtime_rpc()
                    .await
                    .context("Start the host runtime before attaching this target")?;
                ensure!(
                    Path::new(&chosen.cwd).is_dir(),
                    "Host working directory does not exist"
                );
                self.runtime
                    .lock()
                    .await
                    .as_ref()
                    .and_then(|r| r.host_target.clone())
                    .context("Start the host runtime before attaching this target")?
            } else if let Some(vm) = chosen.id.strip_prefix("vm-") {
                ensure!(
                    self.manager.store.environment(vm)?.status == "running",
                    "Start the selected VM before attaching it"
                );
                self.machines
                    .lock()
                    .await
                    .get(vm)
                    .and_then(|m| m.target.clone())
                    .context("VM executor is unavailable")?
            } else if let Some(id) = chosen.id.strip_prefix("container-") {
                ensure!(self.manager.store.container(id)?.status == "running", "Start the selected container before attaching it");
                self.containers.lock().await.get(id).map(|container| container.target.clone())
                    .context("Container executor is unavailable")?
            } else if chosen.id.starts_with("ssh-") {
                let config = self
                    .manager
                    .store
                    .ssh_targets()?
                    .into_iter()
                    .find(|(id, _)| id == &chosen.id)
                    .context("Unknown SSH target")?
                    .1;
                let mut ssh = self.ssh.lock().await;
                if !ssh.contains_key(&chosen.id) {
                    let engine = crate::ssh::Engine::start(&chosen.id, config).await?;
                    ssh.insert(chosen.id.clone(), engine);
                }
                ssh[&chosen.id].check_directory(&chosen.cwd).await?;
                ssh[&chosen.id].target.clone()
            } else {
                let external = registered
                    .iter()
                    .find(|t| t.id == chosen.id)
                    .context("Unknown target")?;
                // A registry entry is immutable. A changed endpoint gets a new identity.
                Target {
                    id: format!("{}-{}", external.id, uuid::Uuid::new_v4().simple()),
                    url: external.url.clone(),
                    cwd: external.cwd.clone(),
                }
            };
            target.cwd = chosen.cwd.clone();
            targets.push(target);
        }
        Ok(targets)
    }

    pub async fn select_targets(
        &self,
        id: &str,
        selection: &[crate::targets::Selection],
    ) -> Result<()> {
        self.select_targets_inner(id, selection, None).await
    }

    pub async fn change_targets(
        &self,
        id: &str,
        selection: &[crate::targets::Selection],
        mode: demodex_protocol::TargetChangeMode,
    ) -> Result<()> {
        self.select_targets_inner(id, selection, Some(mode)).await
    }

    async fn select_targets_inner(
        &self,
        id: &str,
        selection: &[crate::targets::Selection],
        mode: Option<demodex_protocol::TargetChangeMode>,
    ) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        let _settings = self.manager.connecting.lock().await;
        self.manager.store.ensure_target_selection(id)?;
        for target in selection {
            if let Some(owner) = self.manager.store.ssh_target_owner(&target.id)? {
                ensure!(owner == id, "SSH target belongs to another session");
            }
        }
        let session = self.manager.store.get(id)?;
        if selection
            .iter()
            .any(|target| target.id.starts_with("container-"))
        {
            ensure!(
                matches!(
                    session.sandbox,
                    Some(crate::store::Sandbox::DangerFullAccess)
                ),
                "Container targets require danger-full-access"
            );
        }
        ensure!(
            !session.archived,
            "Restore the session before changing its targets"
        );
        let live = self
            .manager
            .runtime(id)
            .await
            .context("Connect the session before changing targets")?;
        if mode.is_none() {
            Manager::require_idle(&live).await?;
        } else {
            let state = live
                .rpc
                .call(
                    "thread/read",
                    json!({"threadId":live.thread,"includeTurns":false}),
                )
                .await?;
            ensure!(
                matches!(
                    state["thread"]["status"]["type"].as_str(),
                    Some("idle" | "active")
                ),
                "Cannot verify the session activity; targets were not saved"
            );
        }
        ensure!(
            !self
                .manager
                .store
                .pending(id)?
                .iter()
                .any(|p| matches!(p.state.as_str(), "pending" | "responding" | "delivered")),
            "Resolve pending decisions before changing targets"
        );
        ensure!(
            self.manager
                .queued(id)
                .await?
                .as_array()
                .is_some_and(|q| q.is_empty()),
            "Remove queued messages before changing targets"
        );
        let goal = live
            .rpc
            .call("thread/goal/get", json!({"threadId":live.thread}))
            .await?;
        ensure!(
            goal.get("goal").is_some() && goal["goal"]["status"] != "active",
            "Pause the goal before changing targets"
        );
        crate::ssh::require_local_app_server(&session.endpoint, selection)?;
        let targets = self.resolve_targets(selection).await?;
        for target in &targets {
            live.rpc
                .call(
                    "environment/add",
                    json!({"environmentId":target.id,"execServerUrl":target.url}),
                )
                .await?;
        }
        if mode == Some(demodex_protocol::TargetChangeMode::Interrupt) {
            // Subscribe before sending so completion cannot race the wait. Never
            // infer interruption from an accepted RPC response or a timeout.
            let mut notices = self.manager.updates.subscribe();
            let interrupted_turn = live.turn.lock().await.clone();
            if let Some(turn) = interrupted_turn {
                live.rpc.call("turn/interrupt", json!({"threadId":live.thread,"turnId":turn})).await
                    .context("Interrupt was not confirmed; targets were not saved. Check session state before retrying")?;
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        let current = self.manager.runtime(id).await?;
                        ensure!(current.generation == live.generation, "Session connection changed; targets were not saved");
                        let active = live.turn.lock().await.clone();
                        if active.is_none() { return Ok::<(),anyhow::Error>(()); }
                        ensure!(active.as_ref() == Some(&turn), "Another turn started; targets were not saved");
                        match notices.recv().await {
                            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                            Err(error) => return Err(error.into()),
                        }
                    }
                }).await.context("Interruption is still unconfirmed; targets were not saved. Check session state before retrying")??;
            }
        }
        // Recheck after connection/interrupt work, then retain the generation
        // guard through persistence. Old turns keep their effective targets.
        if mode != Some(demodex_protocol::TargetChangeMode::NextTurn) {
            Manager::require_idle(&live).await?;
        }
        let turn = live.turn.lock().await;
        if mode != Some(demodex_protocol::TargetChangeMode::NextTurn) {
            ensure!(
                turn.is_none(),
                "Another turn started; targets were not saved"
            );
        }
        let sessions = self.manager.live.lock().await;
        ensure!(
            sessions
                .get(id)
                .is_some_and(|current| current.generation == live.generation),
            "Session connection changed; targets were not saved"
        );
        ensure!(
            !self
                .manager
                .store
                .pending(id)?
                .iter()
                .any(|p| matches!(p.state.as_str(), "pending" | "responding" | "delivered")),
            "Resolve pending decisions before changing targets; targets were not saved"
        );
        if mode.is_some() {
            self.manager
                .store
                .stage_target_selection(id, selection, &targets)?;
        } else {
            self.manager
                .store
                .save_target_selection(id, selection, &targets)?;
        }
        drop(sessions);
        drop(turn);
        self.manager.store.event(id,&json!({"method":"demodex/targetsSelected","params":{"selection":selection,"appliesOnNextMessage":true}}))?;
        self.manager.changed();
        Ok(())
    }

    pub async fn register_ssh_target(&self, config: crate::ssh::Config) -> Result<Value> {
        // Verify credentials, host key, required remote utilities and cwd before saving.
        config.probe().await?;
        let id = self.manager.store.register_ssh_target(&config)?;
        self.manager.changed();
        Ok(json!({"id":id,"name":config.name,"kind":"ssh","cwd":config.cwd}))
    }
    pub async fn register_session_ssh_target(&self, session_id: &str, config: crate::ssh::Config) -> Result<Value> {
        let session = self.manager.store.get(session_id)?;
        ensure!(matches!(session.sandbox, Some(crate::store::Sandbox::DangerFullAccess)), "Set session sandbox to danger-full-access before adding SSH");
        config.probe().await?;
        self.manager.store.ensure_target_selection(session_id)?;
        let mut selection = self.manager.store.target_selection(session_id)?.unwrap_or_default();
        let target_id = self.manager.store.register_ssh_target(&config)?;
        self.manager.store.own_ssh_target(&target_id, session_id)?;
        selection.push(crate::targets::Selection { id: target_id.clone(), cwd: config.cwd.clone() });
        if let Err(error) = self.select_targets(session_id, &selection).await {
            self.ssh.lock().await.remove(&target_id);
            self.manager.store.forget_target(&target_id)?;
            return Err(error);
        }
        Ok(json!({"id":target_id,"name":config.name,"kind":"ssh","cwd":config.cwd,"owner":session_id}))
    }
    pub async fn check_ssh_target(&self, id: &str) -> Result<()> {
        let config = self
            .manager
            .store
            .ssh_targets()?
            .into_iter()
            .find(|(key, _)| key == id)
            .context("Unknown SSH target")?
            .1;
        if let Some(engine) = self.ssh.lock().await.get(id) {
            engine.check_directory(&config.cwd).await?;
        } else {
            config.probe().await?;
        }
        Ok(())
    }
    pub async fn reconnect_ssh_target(&self, id: &str) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        let _settings = self.manager.connecting.lock().await;
        let config = self
            .manager
            .store
            .ssh_targets()?
            .into_iter()
            .find(|(key, _)| key == id)
            .context("Unknown SSH target")?
            .1;
        let users = self.manager.store.target_users(id)?;
        for session in &users {
            ensure!(
                !self
                    .manager
                    .store
                    .pending(session)?
                    .iter()
                    .any(|p| matches!(p.state.as_str(), "pending" | "responding" | "delivered")),
                "Resolve pending decisions in every attached session before replacing the executor"
            );
            if let Ok(live) = self.manager.runtime(session).await {
                Manager::require_idle(&live).await?;
                ensure!(
                    self.manager
                        .queued(session)
                        .await?
                        .as_array()
                        .is_some_and(|q| q.is_empty()),
                    "Clear queued messages in every attached session first"
                );
                let goal = live
                    .rpc
                    .call("thread/goal/get", json!({"threadId":live.thread}))
                    .await?;
                ensure!(
                    goal.get("goal").is_some() && goal["goal"]["status"] != "active",
                    "Pause every attached session's goal first"
                );
            }
        }
        let replacement = crate::ssh::Engine::start(id, config).await?;
        for session in &users {
            if let Ok(live) = self.manager.runtime(session).await {
                Manager::require_idle(&live).await?;
            }
        }
        for session in users {
            self.manager
                .disconnect(
                    &session,
                    "SSH executor replaced; reconnect explicitly to obtain fresh handles",
                )
                .await?;
        }
        self.ssh.lock().await.insert(id.into(), replacement);
        self.manager.changed();
        Ok(())
    }
    pub async fn forget_target(&self, id: &str) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        self.manager.store.forget_target(id)?;
        self.ssh.lock().await.remove(id);
        self.manager.changed();
        Ok(())
    }

    fn container_directory(&self, id: &str) -> PathBuf {
        self.root.join("containers").join(id)
    }

    fn container_name(id: &str) -> String {
        format!("demodex-container-{id}")
    }

    fn container_network(id: &str) -> String {
        format!("demodex-network-{id}")
    }

    async fn remove_container_network(engine: &Path, id: &str) -> Result<()> {
        let output = Command::new(engine)
            .args(["network", "rm", &Self::container_network(id)])
            .kill_on_drop(true).output().await?;
        let error = String::from_utf8_lossy(&output.stderr);
        let lower = error.to_ascii_lowercase();
        ensure!(output.status.success() || lower.contains("not found")
            || lower.contains("no such network") || lower.contains("does not exist"),
            "Cannot remove container network: {error}");
        Ok(())
    }

    pub async fn reap_stale_containers(&self) -> Result<()> {
        for container in self.manager.store.containers()? {
            let name = Self::container_name(&container.id);
            let result = async {
                let output = Command::new(find_binary(&container.engine)?)
                    .args(["container", "rm", "--force", &name]).output().await?;
                // A missing container is normal after an orderly shutdown.
                let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
                if !output.status.success() && !stderr.contains("no such container")
                    && !stderr.contains("no container with name") {
                    bail!("{}", String::from_utf8_lossy(&output.stderr));
                }
                Self::remove_container_network(&find_binary(&container.engine)?, &container.id).await?;
                Ok::<(), anyhow::Error>(())
            };
            let cleanup = tokio::time::timeout(Duration::from_secs(10), result).await;
            if let Err(error) = cleanup.unwrap_or_else(|timeout| Err(timeout.into())) {
                self.manager.store.container_status(&container.id, "error", Some(&format!("Cannot clean up stale container: {error:#}")))?;
            }
        }
        Ok(())
    }

    async fn container_running(engine: &str, name: &str) -> Result<bool> {
        let output = tokio::time::timeout(Duration::from_secs(10), Command::new(find_binary(engine)?)
            .args(["container", "inspect", "--format", "{{.State.Running}}", name])
            .kill_on_drop(true).output()).await.context("Container inspection timed out")??;
        if output.status.success() {
            return match String::from_utf8_lossy(&output.stdout).trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => bail!("Unrecognized container state: {other}"),
            };
        }
        let error = String::from_utf8_lossy(&output.stderr);
        let lower = error.to_ascii_lowercase();
        ensure!(lower.contains("no such container") || lower.contains("no container with name"), "{engine} inspect failed: {error}");
        Ok(false)
    }

    async fn disconnect_container(&self, id: &str, reason: &str) -> Result<()> {
        for session in self.manager.store.target_users(&format!("container-{id}"))? {
            self.manager.disconnect(&session, reason).await?;
        }
        Ok(())
    }

    pub async fn create_container(&self, input: demodex_protocol::NewContainer) -> Result<Value> {
        ensure!(matches!(input.engine.as_str(), "docker" | "podman"), "Choose Docker or Podman");
        let engine = find_binary(&input.engine)?;
        run(Command::new(&engine).args(["image", "inspect", &input.image]))
            .await.context("Container image must be present locally in the selected engine; pull or build it explicitly")?;
        let container = self.manager.store.container_create(
            &input.name, &input.engine, &input.image, input.memory_mib, input.cpus,
        )?;
        self.start_container(&container.id).await?;
        Ok(json!(self.manager.store.container(&container.id)?))
    }

    pub async fn start_container(&self, id: &str) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        let container = self.manager.store.container(id)?;
        let name = Self::container_name(id);
        if self.containers.lock().await.contains_key(id) && Self::container_running(&container.engine, &name).await? {
            self.manager.store.container_status(id, "running", None)?;
            self.manager.changed();
            return Ok(());
        }
        self.disconnect_container(id, "container executor is restarting").await?;
        self.containers.lock().await.remove(id);
        self.manager.store.container_status(id, "starting", None)?;
        let result = self.start_container_inner(&container).await;
        match result {
            Ok(target) => {
                self.containers.lock().await.insert(id.into(), ContainerRuntime { target });
                self.manager.store.container_status(id, "running", None)?;
                self.manager.changed();
                Ok(())
            }
            Err(error) => {
                self.manager.store.container_status(id, "error", Some(&format!("{error:#}")))?;
                self.manager.changed();
                Err(error)
            }
        }
    }

    async fn start_container_inner(&self, container: &crate::targets::ContainerRecord) -> Result<Target> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let engine = find_binary(&container.engine)?;
        run(Command::new(&engine).args(["image", "inspect", &container.image]))
            .await.context("Docker image is unavailable locally")?;
        let directory = self.container_directory(&container.id);
        let workspace = directory.join("workspace");
        let home = directory.join("home");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(&home)?;
        for path in [&directory, &workspace, &home] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            ensure!(!path.to_string_lossy().contains([',', ':']), "Container data path cannot contain a comma or colon");
        }
        let metadata = std::fs::metadata(&directory)?;
        let name = Self::container_name(&container.id);
        // A previous daemon may have left this Demodex-owned container running.
        let _ = Command::new(&engine).args(["container", "rm", "--force", &name]).output().await;
        let port = free_port()?;
        let codex = find_binary("codex")?;
        let nix_codex = codex.starts_with("/nix/store/");
        let executable = if nix_codex { codex.to_string_lossy().into_owned() } else { "codex".into() };
        // Never share the executor's network with unrelated containers. Recreate
        // it rather than trusting potentially stale network configuration.
        let network = Self::container_network(&container.id);
        if container.engine == "podman" {
            // Older Netavark versions only isolate networks whose peers also
            // opt in, which leaves the executor reachable from default bridges.
            let version = run(Command::new(&engine).args(["version", "--format", "{{.Client.Version}}"])).await?;
            ensure!(version.trim().split('.').next().and_then(|v| v.parse::<u32>().ok()).is_some_and(|major| major >= 6),
                "Container executors require Podman 6 or newer with strict bridge isolation");
        }
        Self::remove_container_network(&engine, &container.id).await?;
        let mut create_network = Command::new(&engine);
        create_network.args(["network", "create", "--driver", "bridge", "--opt",
            if container.engine == "podman" { "isolate=strict" }
            else { "com.docker.network.bridge.enable_icc=false" }, &network]);
        run(&mut create_network).await.context("Creating isolated executor network (Podman requires strict bridge isolation support)")?;
        let mut command = Command::new(&engine);
        command.args(["run", "--detach", "--rm", "--pull", "never", "--init",
            "--name", &name, "--network", &network, "--cap-drop", "ALL",
            "--security-opt", "no-new-privileges", "--pids-limit", "512",
            "--user", &format!("{}:{}", metadata.uid(), metadata.gid()),
            "--memory", &format!("{}m", container.memory_mib),
            "--cpus", &container.cpus.to_string(),
            "--publish", &format!("127.0.0.1:{port}:4501"),
            "--mount", &format!("type=bind,source={},target=/workspace", workspace.display()),
            "--mount", &format!("type=bind,source={},target=/home/agent", home.display()),
            "--env", "HOME=/home/agent", "--workdir", "/workspace"]);
        if container.engine == "podman" {
            command.args(["--userns", "keep-id"]);
        }
        if nix_codex {
            command.args(["--mount", "type=bind,source=/nix/store,target=/nix/store,readonly"]);
        }
        command.arg(&container.image).args([executable.as_str(), "exec-server", "--listen", "ws://0.0.0.0:4501"]);
        if let Err(error) = run(&mut command).await {
            let _ = Self::remove_container_network(&engine, &container.id).await;
            return Err(error).with_context(|| format!("starting {} container", container.engine));
        }
        let ready = async {
            for _ in 0..100 {
                if !Self::container_running(&container.engine, &name).await? { bail!("container exited before executor became ready"); }
                if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                    probe_executor(port).await?;
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            bail!("container executor did not become ready")
        }.await;
        if let Err(error) = ready {
            let logs = Command::new(&engine).args(["logs", &name]).output().await.ok()
                .map(|output| String::from_utf8_lossy(&output.stderr).into_owned()).unwrap_or_default();
            let _ = Command::new(&engine).args(["container", "rm", "--force", &name]).output().await;
            let _ = Self::remove_container_network(&engine, &container.id).await;
            bail!("{error:#}; container logs: {logs}");
        }
        Ok(Target { id: format!("container-{}-{}",container.id,uuid::Uuid::new_v4().simple()),
            url: format!("ws://127.0.0.1:{port}"), cwd: "/workspace".into() })
    }

    pub async fn stop_container(&self, id: &str) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        let container = self.manager.store.container(id)?;
        self.disconnect_container(id, "container stopped; workspace preserved").await?;
        let name = Self::container_name(id);
        if Self::container_running(&container.engine, &name).await? {
            run(Command::new(find_binary(&container.engine)?).args(["container", "stop", "--time", "3", &name])).await?;
        }
        self.containers.lock().await.remove(id);
        Self::remove_container_network(&find_binary(&container.engine)?, id).await?;
        self.manager.store.container_status(id, "stopped", None)?;
        self.manager.changed();
        Ok(())
    }
    pub fn new(
        manager: Arc<Manager>,
        root: PathBuf,
        image: Option<PathBuf>,
        host_workspace: Option<PathBuf>,
        codex_home: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            root,
            host_workspace,
            codex_home,
            image: Mutex::new(image),
            runtime: Mutex::new(None),
            usage: crate::usage::RateLimitsCache::default(),
            machines: Mutex::new(HashMap::new()),
            containers: Mutex::new(HashMap::new()),
            ssh: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
        })
    }

    fn runtime_sessions(&self) -> Result<Vec<String>> {
        let hosts = self.manager.store.host_sessions()?;
        let host_users = self.manager.store.target_users("host")?;
        let mut sessions = Vec::new();
        for session in self.manager.store.list()? {
            if self.manager.store.uses_runtime(&session.id)?
                || hosts.contains(&session.id)
                || host_users.contains(&session.id)
                || self
                    .manager
                    .store
                    .session_environment(&session.id)?
                    .is_some()
            {
                sessions.push(session.id);
            }
        }
        Ok(sessions)
    }

    pub fn is_host_mode(&self) -> bool {
        self.host_workspace.is_some()
    }

    pub async fn start_runtime(&self) -> Result<Value> {
        let mut runtime = self.runtime.lock().await;
        if let Some(active) = runtime.as_mut() {
            if active.child.try_wait()?.is_none()
                && match active.executor.as_mut() {
                    Some(child) => child.try_wait()?.is_none(),
                    None => true,
                }
            {
                return Ok(json!({"running":true}));
            }
            active.rpc.close();
        }
        *runtime = None;
        let feature_overrides = self.manager.store.runtime_features()?;
        for id in self.runtime_sessions()? {
            self.manager
                .disconnect(&id, "host runtime is restarting; reconnect to resume")
                .await?;
        }
        let directory = self.root.join("runtime");
        let profile = self
            .codex_home
            .clone()
            .unwrap_or_else(|| directory.join("home"));
        std::fs::create_dir_all(&profile)?;
        std::fs::create_dir_all(directory.join("ipc"))?;
        let config = profile.join("config.toml");
        // An inherited profile belongs to its user; never write defaults into it.
        if self.codex_home.is_none() && !config.exists() {
            std::fs::write(
                &config,
                "model_reasoning_effort = \"medium\"\nsandbox_mode = \"danger-full-access\"\n[analytics]\nenabled = false\n[features]\napps = false\n",
            )?;
        }
        let socket = directory.join("ipc/app.sock");
        let endpoint = format!("unix://{}", socket.display());
        let codex = find_binary("codex")?;
        let certificate = Path::new("/etc/ssl/certs/ca-certificates.crt").canonicalize()?;
        let mut command = Command::new("bwrap");
        command
            .args([
                "--unshare-all",
                "--share-net",
                "--die-with-parent",
                "--ro-bind",
                "/nix/store",
                "/nix/store",
                "--ro-bind",
                "/run/current-system/sw",
                "/run/current-system/sw",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--ro-bind",
                "/etc/resolv.conf",
                "/etc/resolv.conf",
                "--dir",
                "/home/agent",
                "--dir",
                "/workspace",
                "--bind",
            ])
            .arg(directory.join("home"))
            .arg("/home/agent/.codex")
            .arg("--bind")
            .arg(directory.join("ipc"))
            .arg("/run/demodex")
            .args([
                "--clearenv",
                "--setenv",
                "HOME",
                "/home/agent",
                "--setenv",
                "CODEX_HOME",
                "/home/agent/.codex",
                "--setenv",
                "PATH",
                "/run/current-system/sw/bin",
                "--setenv",
                "SSL_CERT_FILE",
            ])
            .arg(certificate)
            .args(["--chdir", "/workspace", "--"])
            .arg(codex)
            .args(["app-server", "--listen", "unix:///run/demodex/app.sock"]);
        let mut executor = None;
        let mut host_target = None;
        if let Some(workspace) = &self.host_workspace {
            let codex = find_binary("codex")?;
            let port = free_port()?;
            let mut execute = Command::new(&codex);
            execute
                .args(["exec-server", "--listen", &format!("ws://127.0.0.1:{port}")])
                .current_dir(workspace);
            let mut child = spawn_logged(execute, &directory.join("executor.log"))?;
            wait_port(port, &mut child).await?;
            probe_executor(port).await?;
            host_target = Some(Target {
                id: format!("host-{}", uuid::Uuid::new_v4().simple()),
                url: format!("ws://127.0.0.1:{port}"),
                cwd: workspace.to_string_lossy().into_owned(),
            });
            executor = Some(child);
            command = Command::new(codex);
            command
                .args(["app-server", "--listen", &endpoint])
                .env("CODEX_HOME", &profile)
                .env_remove("OPENAI_API_KEY")
                .env_remove("CODEX_API_KEY")
                .env_remove("CODEX_ACCESS_TOKEN")
                .current_dir(workspace);
        }
        for (name, enabled) in &feature_overrides {
            command
                .arg(if *enabled { "--enable" } else { "--disable" })
                .arg(name);
        }
        let mut child = spawn_logged(command, &directory.join("app-server.log"))?;
        let mut ready = false;
        for _ in 0..100 {
            ensure!(
                child.try_wait()?.is_none(),
                "isolated app-server exited; inspect runtime/app-server.log"
            );
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ensure!(ready, "isolated app-server socket did not become ready");
        let (rpc, mut events) = Rpc::connect(&endpoint).await?;
        // Authentication notifications are consumed here, never written to conversation logs.
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let feature_catalog = match runtime_feature_catalog(&rpc).await {
            Ok(features) => json!({"data":features}),
            Err(error) => {
                json!({"data":[],"error":format!("Feature discovery unavailable: {error:#}")})
            }
        };
        *runtime = Some(Runtime {
            child,
            executor,
            host_target,
            rpc: Arc::new(rpc),
            endpoint,
            feature_overrides,
            feature_catalog,
        });
        Ok(json!({"running":true}))
    }

    pub(crate) async fn runtime_rpc(&self) -> Result<(Arc<Rpc>, String)> {
        let mut runtime = self.runtime.lock().await;
        let active = runtime
            .as_mut()
            .context("start the managed runtime first")?;
        ensure!(
            active.child.try_wait()?.is_none(),
            "managed runtime exited; restart it from Environments"
        );
        if let Some(executor) = active.executor.as_mut() {
            ensure!(
                executor.try_wait()?.is_none(),
                "host executor exited; restart the runtime from Environments"
            );
        }
        Ok((active.rpc.clone(), active.endpoint.clone()))
    }

    pub async fn runtime_status(&self) -> Result<Value> {
        let runtime = self.runtime_rpc().await;
        let mut status = match runtime {
            Ok((rpc, _)) => match rpc
                .call("account/read", json!({"refreshToken":false}))
                .await
            {
                Ok(account) => {
                    let usage = self.usage.read(rpc.clone(), &account["account"]).await;
                    json!({"running":true,"account":account["account"],"weekly_usage":usage})
                }
                Err(error) => json!({"running":false,"error":error.to_string()}),
            },
            Err(error) => json!({"running":false,"error":error.to_string()}),
        };
        status["mode"] = json!(if self.is_host_mode() { "host" } else { "vm" });
        status["workspace"] = json!(self.host_workspace);
        status["profile"] = json!(if self.codex_home.is_some() {
            "inherited"
        } else {
            "dedicated"
        });
        let saved = self.manager.store.runtime_features()?;
        let runtime = self.runtime.lock().await;
        status["features"] = match runtime.as_ref() {
            Some(active) => json!({"saved":saved,"applied":active.feature_overrides,
                "restart_required":saved != active.feature_overrides,"catalog":active.feature_catalog}),
            None => json!({"saved":saved,"applied":null,"restart_required":false,
                "catalog":{"data":[],"error":"Start the Codex runtime to discover available features."}}),
        };
        Ok(status)
    }

    pub async fn set_runtime_feature(&self, name: &str, enabled: Option<bool>) -> Result<Value> {
        if enabled.is_some() {
            let runtime = self.runtime.lock().await;
            let runtime = runtime
                .as_ref()
                .context("Start the runtime to discover available features")?;
            let feature = runtime.feature_catalog["data"]
                .as_array()
                .and_then(|features| features.iter().find(|feature| feature["name"] == name))
                .context("Unknown feature; restart the runtime to refresh feature discovery")?;
            ensure!(
                feature["stage"] != "removed",
                "This feature has been removed from Codex"
            );
        }
        self.manager.store.set_runtime_feature(name, enabled)?;
        self.manager.changed();
        Ok(json!({"saved":true}))
    }

    pub async fn restart_session_ids(&self) -> Result<Vec<String>> {
        let (_, endpoint) = self.runtime_rpc().await?;
        Ok(self.manager.store.list()?.into_iter().filter(|s| s.endpoint == endpoint).map(|s|s.id).collect())
    }

    pub async fn restart_runtime(&self) -> Result<Value> {
        let _lifecycle = self.jobs.lock().await;
        let _settings = self.manager.connecting.lock().await;
        let (rpc, endpoint) = self.runtime_rpc().await?;
        let sessions: Vec<_> = self
            .manager
            .store
            .list()?
            .into_iter()
            .filter(|session| session.endpoint == endpoint)
            .collect();
        // Refuse to terminate threads controlled outside this manager, including
        // independently running subagents. Never use restart as an implicit stop.
        let loaded = rpc.call("thread/loaded/list", json!({})).await?;
        let threads = loaded["data"]
            .as_array()
            .context("Cannot verify loaded Codex threads")?;
        ensure!(
            loaded["nextCursor"].is_null(),
            "Too many loaded threads to verify a safe restart"
        );
        for thread in threads {
            ensure!(
                sessions
                    .iter()
                    .any(|s| s.thread_id.as_deref() == thread.as_str()),
                "Codex has a loaded thread outside the managed sessions; stop/unload it before restarting"
            );
        }
        for session in &sessions {
            if let Ok(live) = self.manager.runtime(&session.id).await {
                Manager::require_idle(&live)
                    .await
                    .context("Stop all active sessions before restarting Codex")?;
                ensure!(
                    !self
                        .manager
                        .store
                        .pending(&session.id)?
                        .iter()
                        .any(|p| matches!(
                            p.state.as_str(),
                            "pending" | "responding" | "delivered"
                        )),
                    "Resolve pending decisions before restarting Codex"
                );
                ensure!(
                    self.manager
                        .queued(&session.id)
                        .await?
                        .as_array()
                        .is_some_and(|q| q.is_empty()),
                    "Remove queued messages before restarting Codex"
                );
                let goal = live
                    .rpc
                    .call("thread/goal/get", json!({"threadId":live.thread}))
                    .await?;
                ensure!(
                    goal.get("goal").is_some() && goal["goal"]["status"] != "active",
                    "Pause all active goals before restarting Codex"
                );
                ensure!(
                    crate::background::list(&live).await?.is_empty(),
                    "Stop background terminals before restarting Codex"
                );
            } else {
                ensure!(
                    !threads
                        .iter()
                        .any(|thread| thread.as_str() == session.thread_id.as_deref()),
                    "Reconnect loaded sessions so their activity can be checked before restarting"
                );
            }
        }
        for session in &sessions {
            self.manager
                .disconnect(&session.id, "Codex restarted; reconnect to resume")
                .await?;
        }
        if let Some(mut runtime) = self.runtime.lock().await.take() {
            runtime.rpc.close();
            terminate(&mut runtime.child).await;
            if let Some(mut executor) = runtime.executor.take() {
                terminate(&mut executor).await;
            }
            let _ = std::fs::remove_file(self.root.join("runtime/ipc/app.sock"));
        }
        let result = self.start_runtime().await;
        self.manager.changed();
        result
    }

    pub async fn login(&self) -> Result<Value> {
        let (rpc, _) = self.runtime_rpc().await?;
        rpc.call("account/login/start", json!({"type":"chatgptDeviceCode"}))
            .await
    }

    /// Resolve the editable instruction layers from this runtime's profile/catalogue.
    /// This does not start a thread or submit a model turn.
    pub async fn default_prompt(&self) -> Result<Value> {
        ensure!(self.is_host_mode(), "Prompt discovery requires a local host runtime");
        let (rpc, _) = self.runtime_rpc().await?;
        let response = rpc.call("config/read", json!({"cwd":self.host_workspace,"includeLayers":false})).await?;
        let config = &response["config"];
        let models = rpc.call("model/list", json!({"includeHidden":false})).await?;
        let model = config["model"].as_str().or_else(|| models["data"].as_array()?.iter().find(|m|m["isDefault"] == true)?["model"].as_str()).context("Codex did not identify its default model")?;
        let base = if let Some(path) = config["model_instructions_file"].as_str() {
            ensure!(Path::new(path).is_absolute(), "Model instructions path must be resolved by Codex");
            tokio::fs::read_to_string(path).await?
        } else if let Some(text) = config["instructions"].as_str() {
            text.to_owned()
        } else {
            let profile = self.codex_home.clone().unwrap_or_else(||self.root.join("runtime/home"));
            let cache = profile.join("models_cache.json");
            let catalog: Value = if cache.is_file() {
                serde_json::from_slice(&tokio::fs::read(cache).await?)?
            } else {
                let output = tokio::time::timeout(Duration::from_secs(30), Command::new(find_binary("codex")?).args(["debug", "models", "--bundled"]).env("CODEX_HOME", &profile).kill_on_drop(true).output()).await??;
                ensure!(output.status.success(), "Codex model catalogue unavailable");
                serde_json::from_slice(&output.stdout)?
            };
            let entry = catalog["models"].as_array().context("Invalid model catalogue")?.iter().find(|entry|entry["slug"] == model).context("Default model missing from Codex catalogue")?;
            entry["model_messages"]["instructions_template"].as_str().or_else(||entry["base_instructions"].as_str()).context("Model does not expose base instructions")?.to_owned()
        };
        Ok(json!({"model":model,"text":format!("{base}\n\n{}\n\n{}\n\n{}", config["developer_instructions"].as_str().unwrap_or(""), crate::session_context::INSTRUCTIONS, crate::ssh::AGENT_INSTRUCTIONS)}))
    }

    pub async fn saved_threads(&self, cursor: Option<String>, search: String) -> Result<Value> {
        ensure!(
            self.is_host_mode(),
            "saved-thread discovery is available in host mode"
        );
        let (rpc, _) = self.runtime_rpc().await?;
        let search = search.trim();
        if let Ok(id) = uuid::Uuid::parse_str(search) {
            let snapshot = rpc.call("thread/read", json!({"threadId":id.to_string(),"includeTurns":false})).await?;
            return Ok(json!({"data":[snapshot["thread"]],"nextCursor":null}));
        }
        let needle = search.to_lowercase();
        let mut cursor = cursor;
        // Upstream search does not provide substring semantics. Scan metadata
        // pages ourselves; bound each request and preserve the continuation even
        // when this batch contains no matches. Never load transcript turns.
        for page in 0..10 {
            let mut result = rpc.call("thread/list", json!({"cursor":cursor,"limit":50,"sortKey":"updated_at","modelProviders":[]})).await?;
            let rows = result["data"].as_array_mut().context("Invalid saved-thread list")?;
            if !needle.is_empty() {
                rows.retain(|thread| ["id", "name", "preview"].iter().any(|field| {
                    thread[*field].as_str().is_some_and(|text| text.to_lowercase().contains(&needle))
                }));
            }
            if !rows.is_empty() || result["nextCursor"].is_null() || page == 9 {
                return Ok(result);
            }
            let next = result["nextCursor"].as_str().context("Invalid saved-thread cursor")?.to_owned();
            ensure!(cursor.as_ref() != Some(&next), "Saved-thread cursor did not advance");
            cursor = Some(next);
        }
        unreachable!("bounded saved-thread scan always returns its last page")
    }

    pub fn create(
        &self,
        name: &str,
        memory_mib: u32,
        cpus: u16,
        internet: bool,
    ) -> Result<Environment> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 120,
            "name must be 1–120 characters"
        );
        ensure!(
            (512..=65536).contains(&memory_mib) && (1..=32).contains(&cpus),
            "invalid VM resource limits"
        );
        self.manager
            .store
            .environment_create(name.trim(), memory_mib, cpus, internet)
    }

    pub async fn start(self: &Arc<Self>, id: &str) -> Result<()> {
        let mut jobs = self.jobs.lock().await;
        let environment = self.manager.store.environment(id)?;
        if environment.status == "running" {
            return Ok(());
        }
        ensure!(
            jobs.get(id).is_none_or(|j| j.is_finished()),
            "environment operation already in progress"
        );
        self.manager
            .store
            .environment_status(id, "starting", None)?;
        let owner = self.clone();
        let id = id.to_string();
        let job = tokio::spawn(async move {
            if let Err(error) = owner.start_inner(&environment).await {
                let _ = owner.manager.store.environment_status(
                    &environment.id,
                    "error",
                    Some(&format!("{error:#}")),
                );
            }
        });
        jobs.insert(id, job);
        Ok(())
    }

    async fn base_image(&self) -> Result<PathBuf> {
        let mut image = self.image.lock().await;
        if let Some(path) = image.as_ref() {
            return path
                .canonicalize()
                .context("configured VM image does not exist");
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("nix");
        let link = self.root.join("base-image");
        let mut build = Command::new("nix");
        build
            .args([
                "build",
                &format!("path:{}#vm-image", source.display()),
                "--builders",
                "",
                "--out-link",
            ])
            .arg(&link);
        run(&mut build)
            .await
            .context("building the pinned NixOS base")?;
        let path = std::fs::read_dir(&link)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "qcow2"))
            .context("base build did not produce a QCOW2 image")?
            .canonicalize()?;
        *image = Some(path.clone());
        Ok(path)
    }

    fn directory(&self, id: &str) -> PathBuf {
        self.root.join("environments").join(id)
    }

    async fn start_inner(&self, environment: &Environment) -> Result<()> {
        let id = &environment.id;
        self.disconnect_environment(id, "execution environment is reconnecting")
            .await?;
        let directory = self.directory(id);
        std::fs::create_dir_all(&directory)?;
        let machine_dir = directory.join("machine");
        if !machine_dir.exists() {
            let base = self.base_image().await?;
            let staging = directory.join(format!("creating-{}", uuid::Uuid::new_v4()));
            let mut create = Command::new(std::env::current_exe()?);
            create
                .args(["vm", "create", "--base"])
                .arg(base)
                .arg("--directory")
                .arg(&staging)
                .args([
                    "--memory-mib",
                    &environment.memory_mib.to_string(),
                    "--cpus",
                    &environment.cpus.to_string(),
                    "--ssh-port",
                    &free_port()?.to_string(),
                    "--network",
                    if environment.internet {
                        "nat"
                    } else {
                        "isolated"
                    },
                ]);
            run(&mut create).await?;
            std::fs::rename(staging, &machine_dir)?;
        }
        let ssh_port = {
            let mut machines = self.machines.lock().await;
            if let Some(machine) = machines.get_mut(id)
                && machine.child.try_wait()?.is_some()
            {
                machines.remove(id);
            }
            if let Some(machine) = machines.get_mut(id) {
                if let Some(mut tunnel) = machine.tunnel.take() {
                    let _ = tunnel.kill().await;
                }
                machine.target = None;
                machine.ssh_port
            } else {
                let port = free_port()?;
                let config = machine_dir.join("vm.json");
                let mut definition: Value = serde_json::from_slice(&std::fs::read(&config)?)?;
                definition["ssh_port"] = json!(port);
                std::fs::write(&config, serde_json::to_vec_pretty(&definition)?)?;
                let mut command = Command::new(std::env::current_exe()?);
                command.arg("vm").arg("run").arg(&machine_dir);
                let child = spawn_logged(command, &directory.join("console.log"))?;
                machines.insert(
                    id.clone(),
                    Machine {
                        child,
                        tunnel: None,
                        ssh_port: port,
                        target: None,
                    },
                );
                port
            }
        };
        self.manager.store.environment_status(id, "booting", None)?;
        let mut ready = false;
        for _ in 0..90 {
            if run(self.ssh(id, ssh_port).arg("true")).await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        ensure!(
            ready,
            "guest SSH did not become ready; inspect the environment console log"
        );
        self.manager
            .store
            .environment_status(id, "provisioning", None)?;
        let codex = find_binary("codex")?;
        if run(self
            .ssh(id, ssh_port)
            .arg(format!("test -x {}", quote(&codex.to_string_lossy()))))
        .await
        .is_err()
        {
            let closure = run(Command::new("nix-store")
                .arg("-qR")
                .arg(codex.parent().unwrap().parent().unwrap()))
            .await?;
            let mut export = Command::new("nix-store")
                .arg("--export")
                .args(closure.lines())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let pipe: Stdio = export
                .stdout
                .take()
                .context("export pipe unavailable")?
                .try_into()?;
            let imported = run(self
                .ssh(id, ssh_port)
                .arg("sudo nix-store --import >/dev/null")
                .stdin(pipe))
            .await;
            ensure!(export.wait().await?.success(), "Nix closure export failed");
            imported?;
        }
        let service = format!(
            "systemctl is-active --quiet demodex-executor || (sudo systemctl reset-failed demodex-executor 2>/dev/null; sudo systemd-run --unit=demodex-executor --uid=worker --working-directory=/workspace --setenv=HOME=/home/worker --setenv=PATH=/run/current-system/sw/bin {} exec-server --listen ws://127.0.0.1:4501)",
            quote(&codex.to_string_lossy())
        );
        run(self.ssh(id, ssh_port).arg(service)).await?;
        let executor_port = free_port()?;
        let mut tunnel_command = self.ssh_base(id, ssh_port);
        tunnel_command.args([
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            "-L",
            &format!("127.0.0.1:{executor_port}:127.0.0.1:4501"),
            "worker@127.0.0.1",
        ]);
        let mut tunnel = spawn_logged(tunnel_command, &directory.join("tunnel.log"))?;
        wait_port(executor_port, &mut tunnel).await?;
        // Probe the actual remote executor, not merely the forwarding socket.
        probe_executor(executor_port).await?;
        let target = Target {
            id: format!("vm-{id}-{}", uuid::Uuid::new_v4().simple()),
            url: format!("ws://127.0.0.1:{executor_port}"),
            cwd: "/workspace".into(),
        };
        let mut machines = self.machines.lock().await;
        let machine = machines
            .get_mut(id)
            .context("environment stopped during startup")?;
        machine.tunnel = Some(tunnel);
        machine.target = Some(target);
        self.manager.store.environment_status(id, "running", None)?;
        Ok(())
    }

    fn ssh_base(&self, id: &str, port: u16) -> Command {
        let directory = self.directory(id);
        let mut command = Command::new("ssh");
        command
            .args(["-F", "/dev/null", "-i"])
            .arg(directory.join("machine/id_ed25519"))
            .args([
                "-p",
                &port.to_string(),
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=2",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
            ])
            .arg(format!(
                "UserKnownHostsFile={}",
                directory.join("known_hosts").display()
            ));
        command
    }
    fn ssh(&self, id: &str, port: u16) -> Command {
        let mut command = self.ssh_base(id, port);
        command.arg("worker@127.0.0.1");
        command
    }

    async fn disconnect_environment(&self, id: &str, reason: &str) -> Result<()> {
        for session in self.manager.store.target_users(&format!("vm-{id}"))? {
            self.manager.disconnect(&session, reason).await?;
        }
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        self.manager.store.environment(id)?;
        // Serialize lifecycle requests so a concurrent start cannot overtake shutdown.
        let mut jobs = self.jobs.lock().await;
        if let Some(job) = jobs.remove(id) {
            job.abort();
            let _ = job.await;
        }
        self.disconnect_environment(id, "execution environment stopped; disk preserved")
            .await?;
        self.manager
            .store
            .environment_status(id, "stopping", None)?;
        let machine = self.machines.lock().await.remove(id);
        if let Some(mut machine) = machine {
            // Request guest shutdown first. If the guest is broken, stop its owned QEMU process.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                run(self.ssh(id, machine.ssh_port).arg("sudo poweroff")),
            )
            .await;
            if let Some(mut tunnel) = machine.tunnel.take() {
                let _ = tunnel.kill().await;
            }
            if tokio::time::timeout(Duration::from_secs(15), machine.child.wait())
                .await
                .is_err()
            {
                // SIGTERM is handled by the vm subcommand, which also reaps QEMU.
                terminate(&mut machine.child).await;
            }
        }
        self.manager.store.environment_status(id, "stopped", None)?;
        drop(jobs);
        Ok(())
    }

    pub async fn upload_image(&self, id: &str, bytes: &[u8]) -> Result<String> {
        let extension = crate::uploads::extension(bytes)?;
        self.upload_file(id, bytes, &format!("image.{extension}"), true).await
    }

    pub async fn upload_file(&self, id: &str, bytes: &[u8], name: &str, image: bool) -> Result<String> {
        use tokio::io::AsyncWriteExt;
        crate::uploads::validate_name(name)?;
        ensure!(bytes.len() <= crate::uploads::MAX_FILE_BYTES, "Files are limited to 32 MiB");
        let _lifecycle = self.jobs.lock().await;
        self.manager.store.get(id)?;
        if self.manager.store.staged_targets(id)?.is_some()
            && let Ok(live) = self.manager.runtime(id).await
        {
            ensure!(live.turn.lock().await.is_none(), "Wait for the current turn to finish before uploading to the newly selected targets");
        }
        self.manager.store.ensure_target_selection(id)?;
        let selection = self.manager.store.target_selection(id)?.unwrap_or_default();
        let primary = selection
            .first()
            .context("Select an execution target before uploading")?;
        if primary.id == "host" {
            ensure!(self.is_host_mode(), "Host execution is not configured");
            return if image {
                crate::uploads::save(&self.root, bytes, crate::uploads::extension(bytes)?)
            } else { crate::uploads::save_file(&self.root, bytes, name) };
        }
        if primary.id.starts_with("ssh-") {
            let ssh = self.ssh.lock().await;
            let engine = ssh
                .get(&primary.id)
                .context("Reconnect the session before uploading to SSH")?;
            return engine.upload_file(&primary.cwd, bytes, name).await;
        }
        if let Some(container_id) = primary.id.strip_prefix("container-") {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            ensure!(self.containers.lock().await.contains_key(container_id),
                "Start the session's container before uploading");
            let directory_name = format!(".demodex-upload-{}", uuid::Uuid::new_v4());
            let directory = self.container_directory(container_id).join("workspace").join(&directory_name);
            std::fs::create_dir(&directory)?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .open(directory.join(name))?;
            file.write_all(bytes)?;
            return Ok(format!("/workspace/{directory_name}/{name}"));
        }
        let environment = primary
            .id
            .strip_prefix("vm-")
            .context("File upload is unsupported for the first selected target")?
            .to_owned();
        let machines = self.machines.lock().await;
        let machine = machines
            .get(&environment)
            .context("Start the session's VM before uploading")?;
        ensure!(machine.target.is_some(), "VM executor is not ready");
        // Each upload has a private directory; the validated original basename is shell-quoted.
        let directory = format!("/workspace/.demodex-upload-{}", uuid::Uuid::new_v4());
        let path = format!("{directory}/{name}");
        let quoted_path = format!("'{}'", path.replace('\'', "'\\''"));
        let temporary = format!("{directory}/.pending-{}", uuid::Uuid::new_v4());
        let script = format!(
            "umask 077; mkdir '{directory}' && cat > '{temporary}' && mv '{temporary}' {quoted_path}"
        );
        let mut child = self
            .ssh(&environment, machine.ssh_port)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut stdin = child.stdin.take().context("upload stdin unavailable")?;
            stdin.write_all(bytes).await?;
            stdin.shutdown().await?;
            drop(stdin);
            let output = child.wait_with_output().await?;
            ensure!(
                output.status.success(),
                "VM image upload failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("VM image upload timed out; inspect its command receipt before retrying")??;
        Ok(path)
    }

    pub async fn connect_session(&self, id: &str) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        let settings = self.manager.connecting.lock().await;
        self.connect_session_locked(id, &settings, None).await
    }

    // Caller holds jobs, then manager.connecting, throughout the transition.
    pub(crate) async fn connect_session_locked(
        &self,
        id: &str,
        settings: &tokio::sync::MutexGuard<'_, ()>,
        prepared_prompt: Option<&Value>,
    ) -> Result<()> {
        if self.manager.live.lock().await.contains_key(id) {
            return Ok(());
        }
        self.manager.store.ensure_target_selection(id)?;
        let selection = self.manager.store.target_selection(id)?.unwrap_or_default();
        let session = self.manager.store.get(id)?;
        let managed = self.manager.store.uses_runtime(id)?
            || self.manager.store.host_sessions()?.iter().any(|s| s == id)
            || self.manager.store.session_environment(id)?.is_some();
        let endpoint = if managed {
            self.runtime_rpc().await?.1
        } else {
            session.endpoint
        };
        if managed {
            if let Some(prepared) = prepared_prompt {
                self.manager.store.put_prompt_record(&format!("prepared/{id}"), prepared)?;
            } else {
                self.prepare_session_prompt(id).await?;
            }
        }
        crate::ssh::require_local_app_server(&endpoint, &selection)?;
        let targets = self.resolve_targets(&selection).await?;
        if self.manager.store.staged_targets(id)?.is_some() {
            // A surviving app-server may resume an active turn. Keep its exact
            // executor identities and registry usage until turn/start accepts
            // the staged selection; reconnecting is not target application.
            self.manager.store.retarget(id, &endpoint, &session.targets)?;
            self.manager.store.stage_target_selection(id, &selection, &targets)?;
            return self.manager.connect_locked(id, settings).await;
        }
        self.manager.store.retarget(id, &endpoint, &targets)?;
        // Resuming does not update Codex's selected environments until turn/start.
        if session.thread_id.is_some() {
            self.manager
                .store
                .save_target_selection(id, &selection, &targets)?;
        }
        self.manager.connect_locked(id, settings).await
    }

    pub(crate) async fn apply_session_prompt(
        &self,
        id: &str,
        include_project: Option<bool>,
    ) -> Result<Value> {
        let _lifecycle = self.jobs.lock().await;
        let settings = self.manager.connecting.lock().await;
        self.apply_session_prompt_locked(id, include_project, &settings).await
    }

    pub(crate) async fn change_session_model(
        &self,
        id: &str,
        input: demodex_protocol::ModelChoice,
    ) -> Result<Value> {
        let _lifecycle = self.jobs.lock().await;
        let settings = self.manager.connecting.lock().await;
        let applied = self.manager.store.prompt_record(&format!("applied/{id}"))?;
        let managed = self.manager.store.uses_runtime(id)?
            || self.manager.store.host_sessions()?.iter().any(|s| s == id);
        let current = self.manager.store.model_settings(id)?;
        let model_changed = current["effective"]["model"].as_str() != Some(input.model.as_str());
        let reapply = model_changed && managed && self.manager.store.prompt(id)?.is_none()
            && (applied["base"].is_string() || !self.manager.store.prompt_settings()?.1.models.is_empty());
        if reapply { self.require_prompt_change_idle(id).await?; }
        let result = self.manager.change_model_locked(id, input, &settings).await?;
        if reapply {
            let project = self.manager.store.prompt_record(&format!("policy/{id}"))?["include_project"].as_bool();
            self.apply_session_prompt_locked(id, project, &settings).await?;
        }
        Ok(result)
    }

    pub async fn selected_session(
        &self,
        name: &str,
        selection: &[crate::targets::Selection],
        sandbox: Option<crate::store::Sandbox>,
        prompt: Option<&str>,
        include_project: Option<bool>,
        model: Option<&demodex_protocol::ModelChoice>,
    ) -> Result<Session> {
        validate_prompt(prompt)?;
        let name = if name.trim().is_empty() { "Untitled session" } else { name };
        ensure!(name.len() <= 120, "session name must be at most 120 characters");
        crate::targets::validate_selection(selection)?;
        for target in selection {
            ensure!(self.manager.store.ssh_target_owner(&target.id)?.is_none(), "SSH target belongs to another session");
        }
        if selection.iter().any(|target| target.id.starts_with("container-")) {
            ensure!(matches!(sandbox, Some(crate::store::Sandbox::DangerFullAccess)),
                "Container targets require danger-full-access");
        }
        if selection.iter().any(|target| target.id.starts_with("ssh-")) {
            ensure!(
                matches!(sandbox, Some(crate::store::Sandbox::DangerFullAccess)),
                "SSH targets require danger-full-access"
            );
        }
        let session = {
            let _lifecycle = self.jobs.lock().await;
            let (rpc, endpoint) = self.runtime_rpc().await?;
            let model = if let Some(choice) = model {
                let catalog = crate::controls::model_catalog(&rpc).await?;
                Some(crate::controls::validate_model(
                    choice,
                    catalog["data"].as_array().context("missing model catalog")?,
                )?)
            } else {
                None
            };
            crate::ssh::require_local_app_server(&endpoint, selection)?;
            let targets = self.resolve_targets(selection).await?;
            for target in &targets {
                rpc.call(
                    "environment/add",
                    json!({"environmentId":target.id,"execServerUrl":target.url}),
                )
                .await?;
            }
            let session = self.manager.store.create_selected(
                name.trim(),
                &endpoint,
                &targets,
                selection,
                sandbox,
            )?;
            if let Some(model) = model {
                self.manager.store.model_selection(&session.id, &model)?;
            }
            session
        };
        if let Some(prompt) = prompt {
            self.manager.store.save_prompt(&session.id, prompt)?;
        }
        if let Some(include)=include_project { self.manager.store.put_prompt_record(&format!("policy/{}",session.id),&json!({"include_project":include}))?; }
        // Preserve the created session on an uncertain thread/start outcome; never retry it here.
        if let Err(error) = self.connect_session(&session.id).await {
            self.manager
                .store
                .status(&session.id, "disconnected", Some(&error.to_string()))?;
        }
        self.manager.changed();
        self.manager.store.get(&session.id)
    }

    pub async fn host_session(
        &self,
        name: &str,
        thread_id: Option<&str>,
        sandbox: Option<crate::store::Sandbox>,
        cwd: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<Session> {
        validate_prompt(prompt)?;
        ensure!(prompt.is_none() || thread_id.is_none(), "Prompt overrides require a new thread");
        ensure!(self.is_host_mode(), "host execution is not configured");
        ensure!(name.trim().chars().count() <= 120, "session name must be at most 120 characters");
        let (rpc, endpoint) = self.runtime_rpc().await?;
        let requested_cwd = cwd
            .map(|path| -> Result<String> {
                ensure!(
                    Path::new(path).is_absolute() && Path::new(path).is_dir(),
                    "working directory must be an existing absolute directory on this host"
                );
                Ok(Path::new(path)
                    .canonicalize()?
                    .to_string_lossy()
                    .into_owned())
            })
            .transpose()?;
        if let Some(thread) = thread_id
            && let Some(existing) = self
                .manager
                .store
                .list()?
                .into_iter()
                .find(|s| s.thread_id.as_deref() == Some(thread))
        {
            ensure!(
                self.manager.store.host_sessions()?.contains(&existing.id),
                "thread is already attached through another environment"
            );
            if let Some(cwd) = &requested_cwd {
                ensure!(
                    existing.targets.first().is_some_and(|t| Path::new(&t.cwd)
                        .canonicalize()
                        .ok()
                        .as_deref()
                        == Some(Path::new(cwd))),
                    "this thread is already attached with a different working directory; select its existing session or start a new thread"
                );
            }
            self.manager.change_sandbox(&existing.id, sandbox).await?;
            self.connect_session(&existing.id).await?;
            return self.manager.store.get(&existing.id);
        }
        let mut target = self
            .runtime
            .lock()
            .await
            .as_ref()
            .and_then(|r| r.host_target.clone())
            .context("host execution is not configured")?;
        let snapshot = if let Some(thread) = thread_id {
            Some(rpc.call("thread/read", json!({"threadId":thread,"includeTurns":false})).await?)
        } else {
            None
        };
        let saved_name = snapshot.as_ref().and_then(|s| s["thread"]["name"].as_str()).unwrap_or("");
        let name = if name.trim().is_empty() { saved_name } else { name }.trim();
        let name: String = if name.is_empty() { "Untitled session" } else { name }.chars().take(120).collect();
        if let Some(cwd) = requested_cwd {
            target.cwd = cwd;
        } else if let Some(snapshot) = &snapshot {
            let cwd = snapshot["thread"]["cwd"]
                .as_str()
                .context("saved thread has no working directory")?;
            ensure!(
                Path::new(cwd).is_absolute() && Path::new(cwd).is_dir(),
                "saved thread working directory is unavailable on this host: {cwd}"
            );
            target.cwd = cwd.into();
        }
        let attachment = crate::targets::Selection {
            id: "host".into(),
            cwd: target.cwd.clone(),
        };
        let session = self.manager.store.create_attached(
            name.trim(),
            &endpoint,
            &[target],
            thread_id,
            Some(&attachment),
        )?;
        self.manager.store.sandbox(&session.id, sandbox)?;
        if let Some(prompt) = prompt {
            self.manager.store.save_prompt(&session.id, prompt)?;
        }
        if let Err(error) = self.connect_session(&session.id).await {
            self.manager
                .store
                .status(&session.id, "disconnected", Some(&error.to_string()))?;
        }
        self.manager.store.get(&session.id)
    }

    pub async fn session(
        &self,
        id: &str,
        name: &str,
        sandbox: Option<crate::store::Sandbox>,
    ) -> Result<Session> {
        self.manager.store.environment(id)?;
        ensure!(!name.trim().is_empty(), "session name is required");
        let (_, endpoint) = self.runtime_rpc().await?;
        let attachment = crate::targets::Selection {
            id: format!("vm-{id}"),
            cwd: "/workspace".into(),
        };
        let session =
            self.manager
                .store
                .create_attached(name, &endpoint, &[], None, Some(&attachment))?;
        self.manager.store.sandbox(&session.id, sandbox)?;
        // Leave the session reviewable and reconnectable even if connection setup fails.
        if let Err(error) = self.connect_session(&session.id).await {
            self.manager
                .store
                .status(&session.id, "disconnected", Some(&error.to_string()))?;
        }
        self.manager.store.get(&session.id)
    }

    pub async fn monitor(&self) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        if self.runtime_rpc().await.is_err() {
            for id in self.runtime_sessions()? {
                if self.manager.store.get(&id)?.status != "disconnected" {
                    self.manager
                        .disconnect(
                            &id,
                            "host runtime disconnected; restart it from Environments",
                        )
                        .await?;
                }
            }
        }
        let mut failures = Vec::new();
        {
            let mut machines = self.machines.lock().await;
            for (id, machine) in machines.iter_mut() {
                if machine.target.is_none() {
                    continue;
                }
                if machine.child.try_wait()?.is_some()
                    || machine
                        .tunnel
                        .as_mut()
                        .is_none_or(|t| t.try_wait().ok().flatten().is_some())
                {
                    machine.target = None;
                    failures.push(id.clone());
                }
            }
        }
        for id in failures {
            self.disconnect_environment(
                &id,
                "executor connection ended; reconnect the environment before resuming",
            )
            .await?;
            self.manager.store.environment_status(
                &id,
                "error",
                Some("VM or SSH connection ended. Reconnect preserves the guest disk."),
            )?;
        }
        let container_ids: Vec<_> = self.containers.lock().await.keys().cloned().collect();
        for id in container_ids {
            let engine = self.manager.store.container(&id)?.engine;
            let health = Self::container_running(&engine, &Self::container_name(&id)).await;
            self.record_container_health(&id, health).await?;
        }
        Ok(())
    }

    async fn record_container_health(&self, id: &str, health: Result<bool>) -> Result<()> {
        let (status, error) = match health {
            Ok(true) => ("running", None),
            Ok(false) => {
                self.disconnect_container(id, "container executor ended; restart it before reconnecting").await?;
                self.containers.lock().await.remove(id);
                ("error", Some("Container executor ended. Workspace preserved.".to_owned()))
            }
            // An unavailable engine is not evidence that its container stopped.
            // Retain ownership for later monitoring and orderly shutdown.
            Err(error) => ("unknown", Some(format!("Cannot verify container state: {error:#}"))),
        };
        let previous = self.manager.store.container(id)?;
        if previous.status != status || previous.error != error {
            self.manager.store.container_status(id, status, error.as_deref())?;
            self.manager.changed();
        }
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.ssh.lock().await.clear();
        let ids: Vec<_> = self.jobs.lock().await.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id).await;
        }
        // Include containers whose start/inspection failed: the engine may
        // have recovered, and stopped containers can still own bridge networks.
        for container in self.manager.store.containers().unwrap_or_default() {
            let _ = self.stop_container(&container.id).await;
        }
        if let Some(mut runtime) = self.runtime.lock().await.take() {
            runtime.rpc.close();
            terminate(&mut runtime.child).await;
            if let Some(mut executor) = runtime.executor.take() {
                terminate(&mut executor).await;
            }
            // Bubblewrap terminates the namespace; app-server may not run its socket guard.
            let _ = std::fs::remove_file(self.root.join("runtime/ipc/app.sock"));
        }
    }
}

async fn runtime_feature_catalog(rpc: &Rpc) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let mut cursor = Value::Null;
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = rpc
            .call(
                "experimentalFeature/list",
                json!({"cursor":cursor,"limit":100}),
            )
            .await?;
        let data = page["data"]
            .as_array()
            .context("Invalid feature discovery response")?;
        result.extend(data.iter().cloned());
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
        ensure!(
            seen.insert(cursor.to_string()) && result.len() <= 10000,
            "Invalid feature pagination"
        );
    }
    Ok(result)
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn find_binary(name: &str) -> Result<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join(name);
        if path.is_file() {
            return Ok(path.canonicalize()?);
        }
    }
    bail!("required executable {name} is not installed")
}
fn spawn_logged(mut command: Command, path: &Path) -> Result<Child> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    Ok(command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?)
}
async fn run(command: &mut Command) -> Result<String> {
    let output = command.kill_on_drop(true).output().await?;
    ensure!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
async fn wait_port(port: u16, child: &mut Child) -> Result<()> {
    for _ in 0..100 {
        ensure!(
            child.try_wait()?.is_none(),
            "process exited before becoming ready"
        );
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("process did not become ready")
}
async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await;
    }
    if tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
}
async fn probe_executor(port: u16) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}")).await?;
        socket
            .send(Message::Text(
                json!({"id":1,"method":"initialize","params":{"clientName":"demodex-health"}})
                    .to_string()
                    .into(),
            ))
            .await?;
        let response: Value =
            serde_json::from_str(socket.next().await.context("executor closed")??.to_text()?)?;
        ensure!(
            response.get("result").is_some(),
            "executor initialization failed: {response}"
        );
        socket.close(None).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("executor health check timed out")?
}

#[cfg(test)]
mod prompt_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::{Semaphore, mpsc};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn prompt_transition_blocks_sends_through_unsubscribe_and_resume() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), transition()).await?
    }

    async fn transition() -> Result<()> {
        let root = tempfile::tempdir()?;
        // The runtime knows the model but the local catalogue does not. Inclusion
        // settings must still work without manufacturing replacement instructions.
        std::fs::write(root.path().join("models_cache.json"), r#"{"models":[]}"#)?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let gate = Arc::new(Semaphore::new(0));
        let (phases, mut observed) = mpsc::unbounded_channel();
        let server = tokio::spawn({
            let gate = gate.clone();
            async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let gate = gate.clone();
                    let phases = phases.clone();
                    connections.spawn(async move {
                        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                        while let Some(Ok(Message::Text(raw))) = ws.next().await {
                            let request: Value = serde_json::from_str(&raw).unwrap();
                            let method = request["method"].as_str().unwrap();
                            let result = match method {
                                "initialize" => json!({}),
                                "initialized" => continue,
                                "config/read" => json!({"config":{"model":"fixture"}}),
                                "model/list" => json!({"data":[{"model":"fixture","isDefault":true}],"nextCursor":null}),
                                "thread/start" => json!({"thread":{"id":"thread"},"model":"fixture","reasoningEffort":"low","sandbox":{"type":"readOnly"}}),
                                "thread/read" => json!({"thread":{"status":{"type":"idle"}}}),
                                "thread/queue/list" | "thread/backgroundTerminals/list" => json!({"data":[],"nextCursor":null}),
                                "thread/goal/get" => json!({"goal":null}),
                                "thread/unsubscribe" => {
                                    phases.send("unsubscribe").unwrap();
                                    gate.acquire().await.unwrap().forget();
                                    json!({"status":"unsubscribed"})
                                }
                                "thread/resume" => {
                                    assert!(request["params"].get("baseInstructions").is_none());
                                    phases.send("resume").unwrap();
                                    gate.acquire().await.unwrap().forget();
                                    json!({"thread":{"id":"thread","turns":[],"status":{"type":"idle"}},"model":"fixture","reasoningEffort":"low","sandbox":{"type":"readOnly"}})
                                }
                                "turn/start" => {
                                    phases.send("send").unwrap();
                                    json!({"turn":{"id":"turn"}})
                                }
                                _ => panic!("unexpected method: {method}"),
                            };
                            ws.send(Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).await.unwrap();
                        }
                    });
                }
            }
        });
        let manager = Manager::new(crate::store::Store::open(Path::new(":memory:"))?);
        let orchestrator = Orchestrator::new(manager.clone(), root.path().into(), None, None, Some(root.path().into()));
        let (rpc, _events) = Rpc::connect(&endpoint).await?;
        *orchestrator.runtime.lock().await = Some(Runtime {
            child: Command::new("sleep").arg("60").kill_on_drop(true).spawn()?,
            executor: None, host_target: None, rpc: Arc::new(rpc), endpoint: endpoint.clone(),
            feature_overrides: Default::default(), feature_catalog: Value::Null,
        });
        let session = manager.store.create_selected("fixture", &endpoint, &[], &[], None)?;
        orchestrator.connect_session(&session.id).await?;
        let apply = tokio::spawn({
            let orchestrator = orchestrator.clone(); let id = session.id.clone();
            async move { orchestrator.apply_session_prompt(&id, Some(false)).await }
        });
        assert_eq!(observed.recv().await, Some("unsubscribe"));
        assert!(manager.connecting.try_lock().is_err());
        assert!(orchestrator.jobs.try_lock().is_err());
        let mut send = tokio::spawn({
            let manager = manager.clone(); let id = session.id.clone();
            async move { manager.prompt(&id, "fixture message; no inference").await }
        });
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut send).await.is_err());
        gate.add_permits(1);
        assert_eq!(observed.recv().await, Some("resume"));
        assert!(manager.connecting.try_lock().is_err());
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut send).await.is_err());
        gate.add_permits(1);
        let applied = apply.await??;
        assert!(applied["applied"]["base"].is_null());
        assert_eq!(applied["applied"]["include_project"], false);
        send.await??;
        assert_eq!(observed.recv().await, Some("send"));
        manager.disconnect(&session.id, "test complete").await?;
        if let Some(mut runtime) = orchestrator.runtime.lock().await.take() {
            runtime.child.kill().await?;
        }
        server.abort();
        Ok(())
    }
}

#[cfg(test)]
mod container_tests {
    use super::*;

    #[tokio::test]
    async fn uncertain_inspection_retains_ownership_and_recovers_same_executor() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = crate::store::Store::open(&root.path().join("db"))?;
        let container = store.container_create("Fixture", "docker", "local-image", 512, 1)?;
        store.container_status(&container.id, "running", None)?;
        let manager = Manager::new(store);
        let orchestrator = Orchestrator::new(manager.clone(), root.path().into(), None, None, None);
        let target = Target { id:"original-executor".into(), url:"ws://127.0.0.1:1".into(), cwd:"/workspace".into() };
        orchestrator.containers.lock().await.insert(container.id.clone(), ContainerRuntime { target:target.clone() });
        for _ in 0..2 {
            orchestrator.record_container_health(&container.id, Err(anyhow::anyhow!("engine temporarily unavailable"))).await?;
            assert_eq!(orchestrator.containers.lock().await[&container.id].target, target);
            let record = manager.store.container(&container.id)?;
            assert_eq!(record.status, "unknown");
            assert!(record.error.unwrap().contains("engine temporarily unavailable"));
        }
        orchestrator.record_container_health(&container.id, Ok(true)).await?;
        assert_eq!(orchestrator.containers.lock().await[&container.id].target, target);
        assert_eq!(manager.store.container(&container.id)?.status, "running");
        assert!(manager.store.container(&container.id)?.error.is_none());
        orchestrator.record_container_health(&container.id, Ok(false)).await?;
        assert!(!orchestrator.containers.lock().await.contains_key(&container.id));
        assert_eq!(manager.store.container(&container.id)?.status, "error");
        Ok(())
    }
}
