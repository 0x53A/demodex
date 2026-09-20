use crate::{
    manager::Manager,
    rpc::Rpc,
    store::{Environment, Session, Target},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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
}
struct Machine {
    child: Child,
    tunnel: Option<Child>,
    ssh_port: u16,
    target: Option<Target>,
}

pub struct Orchestrator {
    manager: Arc<Manager>,
    root: PathBuf,
    host_workspace: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    image: Mutex<Option<PathBuf>>,
    runtime: Mutex<Option<Runtime>>,
    machines: Mutex<HashMap<String, Machine>>,
    jobs: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl Orchestrator {
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
            machines: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
        })
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
        for id in self.manager.store.host_sessions()? {
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
        *runtime = Some(Runtime {
            child,
            executor,
            host_target,
            rpc: Arc::new(rpc),
            endpoint,
        });
        Ok(json!({"running":true}))
    }

    async fn runtime_rpc(&self) -> Result<(Arc<Rpc>, String)> {
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
                Ok(account) => json!({"running":true,"account":account["account"]}),
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
        Ok(status)
    }

    pub async fn login(&self) -> Result<Value> {
        let (rpc, _) = self.runtime_rpc().await?;
        rpc.call("account/login/start", json!({"type":"chatgptDeviceCode"}))
            .await
    }

    pub async fn saved_threads(&self, cursor: Option<String>, search: String) -> Result<Value> {
        ensure!(self.is_host_mode(), "saved-thread discovery is available in host mode");
        let (rpc, _) = self.runtime_rpc().await?;
        let search = (!search.trim().is_empty()).then(|| search.trim().to_owned());
        rpc.call("thread/list", json!({"cursor":cursor,"limit":50,"sortKey":"updated_at","searchTerm":search,"modelProviders":[]})).await
    }

    pub fn create(
        &self,
        name: &str,
        memory_mib: u32,
        cpus: u16,
        internet: bool,
    ) -> Result<Environment> {
        ensure!(
            !self.is_host_mode(),
            "VM provisioning is disabled in host mode"
        );
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
        for session in self.manager.store.environment_sessions(id)? {
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
        use tokio::io::AsyncWriteExt;
        let extension = crate::uploads::extension(bytes)?;
        let _lifecycle = self.jobs.lock().await;
        self.manager.store.get(id)?;
        if self.is_host_mode() && self.manager.store.host_sessions()?.iter().any(|s| s == id) {
            return crate::uploads::save(&self.root, bytes, extension);
        }
        let environment = self.manager.store.session_environment(id)?
            .context("Image upload is available for managed host and VM sessions only")?;
        let machines = self.machines.lock().await;
        let machine = machines.get(&environment).context("Start the session's VM before uploading")?;
        ensure!(machine.target.is_some(), "VM executor is not ready");
        // No user text enters the remote shell. Each upload has a new private directory.
        let directory = format!("/workspace/.demodex-upload-{}", uuid::Uuid::new_v4());
        let path = format!("{directory}/image.{extension}");
        let script = format!("umask 077; mkdir '{directory}' && cat > '{directory}/pending' && mv '{directory}/pending' '{path}'");
        let mut child = self.ssh(&environment, machine.ssh_port).arg(script)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
            .kill_on_drop(true).spawn()?;
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut stdin = child.stdin.take().context("upload stdin unavailable")?;
            stdin.write_all(bytes).await?;
            stdin.shutdown().await?;
            drop(stdin);
            let output = child.wait_with_output().await?;
            ensure!(output.status.success(), "VM image upload failed: {}", String::from_utf8_lossy(&output.stderr));
            Ok::<(), anyhow::Error>(())
        }).await.context("VM image upload timed out; inspect its command receipt before retrying")??;
        Ok(path)
    }

    pub async fn connect_session(&self, id: &str) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        if self.manager.store.host_sessions()?.iter().any(|s| s == id) {
            let (_, endpoint) = self.runtime_rpc().await?;
            let mut target = self
                .runtime
                .lock()
                .await
                .as_ref()
                .and_then(|r| r.host_target.clone())
                .context("host execution is not configured")?;
            if let Some(previous) = self.manager.store.get(id)?.targets.first() {
                target.cwd = previous.cwd.clone();
            }
            self.manager.store.retarget(id, &endpoint, &[target])?;
        } else if let Some(environment) = self.manager.store.session_environment(id)? {
            ensure!(
                self.manager.store.environment(&environment)?.status == "running",
                "start the session's environment first"
            );
            let (_, endpoint) = self.runtime_rpc().await?;
            let target = self
                .machines
                .lock()
                .await
                .get(&environment)
                .and_then(|m| m.target.clone())
                .context("executor is disconnected; reconnect the environment")?;
            self.manager.store.retarget(id, &endpoint, &[target])?;
        }
        self.manager.connect(id).await
    }

    pub async fn host_session(&self, name: &str, thread_id: Option<&str>, sandbox: Option<crate::store::Sandbox>, cwd: Option<&str>) -> Result<Session> {
        ensure!(self.is_host_mode(), "host execution is not configured");
        ensure!(
            !name.trim().is_empty() && name.len() <= 120,
            "session name must be 1–120 characters"
        );
        let (rpc, endpoint) = self.runtime_rpc().await?;
        let requested_cwd = cwd.map(|path| -> Result<String> {
            ensure!(Path::new(path).is_absolute() && Path::new(path).is_dir(), "working directory must be an existing absolute directory on this host");
            Ok(Path::new(path).canonicalize()?.to_string_lossy().into_owned())
        }).transpose()?;
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
                ensure!(existing.targets.first().is_some_and(|t|Path::new(&t.cwd).canonicalize().ok().as_deref()==Some(Path::new(cwd))),
                    "this thread is already attached with a different working directory; select its existing session or start a new thread");
            }
            self.manager.change_sandbox(&existing.id,sandbox).await?;
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
        if let Some(cwd) = requested_cwd {
            target.cwd = cwd;
        } else if let Some(thread) = thread_id {
            let snapshot = rpc
                .call(
                    "thread/read",
                    json!({"threadId":thread,"includeTurns":false}),
                )
                .await?;
            let cwd = snapshot["thread"]["cwd"]
                .as_str()
                .context("saved thread has no working directory")?;
            ensure!(
                Path::new(cwd).is_absolute() && Path::new(cwd).is_dir(),
                "saved thread working directory is unavailable on this host: {cwd}"
            );
            target.cwd = cwd.into();
        }
        let session = self
            .manager
            .store
            .create(name.trim(), &endpoint, &[target], thread_id)?;
        self.manager.store.bind_host(&session.id)?;
        self.manager.store.sandbox(&session.id,sandbox)?;
        if let Err(error) = self.connect_session(&session.id).await {
            self.manager
                .store
                .status(&session.id, "disconnected", Some(&error.to_string()))?;
        }
        self.manager.store.get(&session.id)
    }

    pub async fn session(&self, id: &str, name: &str, sandbox: Option<crate::store::Sandbox>) -> Result<Session> {
        self.manager.store.environment(id)?;
        ensure!(!name.trim().is_empty(), "session name is required");
        let (_, endpoint) = self.runtime_rpc().await?;
        let session = self.manager.store.create(name, &endpoint, &[], None)?;
        self.manager.store.bind_environment(&session.id, id)?;
        self.manager.store.sandbox(&session.id,sandbox)?;
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
        if self.is_host_mode() && self.runtime_rpc().await.is_err() {
            for id in self.manager.store.host_sessions()? {
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
        Ok(())
    }

    pub async fn shutdown(&self) {
        let ids: Vec<_> = self.jobs.lock().await.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id).await;
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
