//! Foreground-only local exec-server adapter over SSH and the standard SFTP subsystem.
mod sftp;
pub const AGENT_INSTRUCTIONS: &str = "Demodex SSH targets (environment IDs starting with ssh-) support non-interactive foreground commands only, with no adapter-imposed runtime limit. If you explicitly want a deadline, check for the remote timeout utility and wrap the command with your chosen duration. Do not request PTYs, stdin pipes, background process handles or later stdin writes. Cancellation closes SSH; remote termination is best effort and an interrupted command may still be running. For background or longer-running work, you may first check whether tmux is installed on the remote host (for example, command -v tmux). If available, manage explicitly named tmux sessions with ordinary foreground commands, redirect output to remote files, and inspect completion separately. Tmux jobs belong to the remote host and are not tracked, resumed or cleaned up by Demodex. Do not assume tmux is installed or install it without authorization. Never automatically rerun a command with an uncertain outcome.";

use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    process::Command,
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug)]
struct RpcFailure {
    code: i64,
    message: String,
}
impl std::fmt::Display for RpcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RpcFailure {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub destination: String,
    pub cwd: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub known_hosts_file: Option<String>,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 120,
            "Target name must be 1–120 characters"
        );
        ensure!(
            !self.destination.is_empty()
                && self.destination.len() <= 255
                && !self.destination.starts_with('-')
                && self
                    .destination
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"@._:-[]".contains(&c)),
            "Use an SSH config alias or user@hostname"
        );
        crate::targets::validate_cwd(&self.cwd)?;
        ensure!(self.port != Some(0), "SSH port must be nonzero");
        for path in [&self.identity_file, &self.known_hosts_file]
            .into_iter()
            .flatten()
        {
            crate::targets::validate_cwd(path)?;
        }
        Ok(())
    }
    pub async fn probe(&self) -> Result<Value> {
        self.validate()?;
        self.probe_directory(&self.cwd).await
    }
    fn transport(&self) -> Command {
        let mut c = Command::new("ssh");
        c.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ClearAllForwardings=yes",
        ]);
        if let Some(port) = self.port {
            c.args(["-p", &port.to_string()]);
        }
        if let Some(path) = &self.identity_file {
            c.args(["-i", path, "-o", "IdentitiesOnly=yes"]);
        }
        if let Some(path) = &self.known_hosts_file {
            c.arg("-o").arg(format!(
                "UserKnownHostsFile=\"{}\"",
                path.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    }
    fn command(&self, script: &str) -> Command {
        let mut command = self.transport();
        command
            .arg("--")
            .arg(&self.destination)
            .arg(format!("exec sh -c {}", quote(script)));
        command.stdin(Stdio::null());
        command
    }
    async fn output(&self, script: &str) -> Result<Vec<u8>> {
        let output = tokio::time::timeout(Duration::from_secs(20), self.command(script).output())
            .await
            .context("SSH check timed out")??;
        ensure!(
            output.status.success(),
            "SSH failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            output.stdout.len() <= 2 * 1024 * 1024,
            "SSH metadata exceeds 2 MiB"
        );
        Ok(output.stdout)
    }
    async fn probe_directory(&self, cwd: &str) -> Result<Value> {
        crate::targets::validate_cwd(cwd)?;
        let script = format!(
            "cd {} || {{ printf '%s' 'Remote working directory does not exist' >&2; exit 1; }}; printf '%s\\0%s\\0%s\\0' \"$PWD\" \"$HOME\" \"${{SHELL:-/bin/sh}}\"; cat /proc/sys/kernel/random/boot_id",
            quote(cwd)
        );
        let bytes = self.output(&script).await?;
        let parts = bytes
            .split(|b| *b == 0)
            .map(std::str::from_utf8)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            parts.len() == 4 && !parts[3].trim().is_empty(),
            "SSH target needs a POSIX shell and Linux boot identity"
        );
        let mut files = tokio::time::timeout(Duration::from_secs(20), sftp::Sftp::connect(self))
            .await
            .context("SFTP connection timed out")??;
        let meta = tokio::time::timeout(Duration::from_secs(20), files.stat(cwd))
            .await
            .context("SFTP check timed out")??;
        ensure!(
            meta["isDirectory"] == true,
            "Remote working directory is not a directory"
        );
        Ok(
            json!({"boot":parts[3].trim(),"info":{"shell":{"name":parts[2].rsplit('/').next().unwrap_or("sh"),"path":parts[2]},"cwd":sftp::path_uri(parts[0]),"userHomeDir":sftp::path_uri(parts[1]),"platformOs":"linux","tempDir":"file:///tmp","temporaryDirectories":["file:///tmp"],"capabilities":{}}}),
        )
    }
}

pub struct Engine {
    pub target: crate::store::Target,
    backend: Arc<Backend>,
    task: JoinHandle<()>,
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Backend {
    config: Config,
    boot: Value,
    info: Value,
}
#[derive(Default)]
struct Session {
    processes: Mutex<HashMap<String, Arc<Process>>>,
}
struct Process {
    params: String,
    state: Mutex<ProcessState>,
    cancel: mpsc::Sender<()>,
}
#[derive(Default)]
struct ProcessState {
    chunks: VecDeque<Value>,
    bytes: usize,
    next: u64,
    closed: bool,
    code: Value,
    failure: Value,
    discarded: u64,
}

fn sandbox(p: &Value) -> Result<()> {
    if !p["sandbox"].is_null() {
        ensure!(
            p["sandbox"]["permissions"]["type"] == "disabled",
            "SSH targets cannot enforce a sandbox; select danger-full-access explicitly"
        );
    }
    ensure!(
        p["enforceManagedNetwork"] != true
            && p["managedNetwork"].is_null()
            && p["networkProxy"].is_null(),
        "SSH adapter does not support managed networking"
    );
    ensure!(
        p["shellSnapshot"].is_null(),
        "SSH adapter does not support shell snapshots"
    );
    Ok(())
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}

impl Engine {
    pub async fn check_directory(&self, cwd: &str) -> Result<()> {
        let probe = self.backend.config.probe_directory(cwd).await?;
        ensure!(
            probe["boot"] == self.backend.boot,
            "SSH host rebooted; replace the SSH executor and reconnect attached sessions"
        );
        Ok(())
    }
    pub async fn upload(&self, cwd: &str, bytes: &[u8], extension: &str) -> Result<String> {
        self.check_directory(cwd).await?;
        tokio::time::timeout(Duration::from_secs(45), async {
            let mut files = sftp::Sftp::connect(&self.backend.config).await?;
            let directory = format!(
                "{}/.demodex-upload-{}",
                cwd.trim_end_matches('/'),
                uuid::Uuid::new_v4()
            );
            files.mkdir(&directory, 0o700).await?;
            let path = format!("{directory}/image.{extension}");
            files.write(&path, bytes, true).await?;
            Ok::<_, anyhow::Error>(path)
        })
        .await
        .context("SSH upload timed out; outcome uncertain, do not replay automatically")?
    }
    pub async fn start(id: &str, config: Config) -> Result<Self> {
        config.validate()?;
        let probe = config.probe().await?;
        let cwd = config.cwd.clone();
        let backend = Arc::new(Backend {
            config,
            boot: probe["boot"].clone(),
            info: probe["info"].clone(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let server_backend = backend.clone();
        let task = tokio::spawn(async move {
            let backend = server_backend;
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>match accepted {
                        Ok((socket,_))=> {
                            let backend=backend.clone();
                            connections.spawn(async move {
                                // A loopback port must not grant the SSH account to another local user.
                                if !same_user(&socket).unwrap_or(false) { return; }
                                let ws=tokio_tungstenite::accept_hdr_async(socket,reject_browser_origin).await;
                                if let Ok(ws)=ws { serve(backend,ws).await; }
                            });
                        }, Err(_)=>break,
                    },
                    _=connections.join_next(),if !connections.is_empty()=>{}
                }
            }
        });
        Ok(Self {
            target: crate::store::Target {
                id: format!("{id}-{}", uuid::Uuid::new_v4()),
                url,
                cwd,
            },
            backend,
            task,
        })
    }
}
fn same_user(socket: &tokio::net::TcpStream) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")?.uid();
    let peer = socket.peer_addr()?;
    let local = socket.local_addr()?;
    fn address(a: std::net::SocketAddr) -> String {
        let std::net::IpAddr::V4(ip) = a.ip() else {
            return String::new();
        };
        format!("{:08X}:{:04X}", u32::from_le_bytes(ip.octets()), a.port())
    }
    for line in std::fs::read_to_string("/proc/net/tcp")?.lines().skip(1) {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.len() > 7 && f[1] == address(peer) && f[2] == address(local) {
            let owner: u32 = f[7].parse()?;
            return Ok(owner == uid || owner == 0);
        }
    }
    Ok(false)
}
async fn serve(
    backend: Arc<Backend>,
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) {
    let (mut sink, mut source) = ws.split();
    let (out, mut receiver) = mpsc::channel::<Value>(128);
    let session = Arc::new(Session::default());
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut initialized = false;
    let mut requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            message=source.next()=>{
                let text=match message {
                    Some(Ok(Message::Text(text)))=>text,
                    Some(Ok(Message::Ping(_) | Message::Pong(_)))=>continue,
                    _=>break,
                };
                let Ok(request)=serde_json::from_str::<Value>(&text) else { break };
                let Some(id)=request.get("id").cloned() else { continue };
                let method=request["method"].as_str().unwrap_or("").to_owned();
                let params=request.get("params").cloned().unwrap_or(json!({}));
                if method=="initialize" {
                    let response=if initialized || !params["resumeSessionId"].is_null() { json!({"error":{"code":-32600,"message":"Executor session unavailable; reconnect explicitly. Old handles cannot be resumed."}}) } else { initialized=true;json!({"result":{"sessionId":session_id,"environmentInfo":backend.info}}) };
                    let mut response=response; response["id"]=id; response["jsonrpc"]=json!("2.0");
                    if sink.send(Message::Text(response.to_string().into())).await.is_err() {break}
                    continue;
                }
                let out=out.clone();let b=backend.clone();let s=session.clone();
                requests.spawn(async move {
                    let result=if initialized { b.call(&s,&out,&method,params).await } else { Err(anyhow::anyhow!("Initialize first")) };
                    let response=match result { Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":value}), Err(error)=>json!({"jsonrpc":"2.0","id":id,"error":{"code":error.downcast_ref::<RpcFailure>().map(|e|e.code).unwrap_or(-32603),"message":format!("{error:#}")}}) };
                    let _=out.send(response).await;
                });
            },
            Some(value)=receiver.recv()=>if sink.send(Message::Text(value.to_string().into())).await.is_err(){break},
            _=requests.join_next(),if !requests.is_empty()=>{}
        }
    }
    requests.abort_all();
    // Dropping pending requests closes local SSH. Remote termination is best effort.
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn unsupported(method: &str) -> Result<Value> {
    Err(RpcFailure{code:-32601,message:format!("SSH foreground executor does not support {method}. Use non-interactive foreground commands. For background work, check whether tmux is available remotely and manage tmux jobs with ordinary commands; they are not executor-managed.")}.into())
}
impl Backend {
    async fn call(
        &self,
        session: &Session,
        out: &mpsc::Sender<Value>,
        method: &str,
        p: Value,
    ) -> Result<Value> {
        ensure!(p.is_object(), "Expected object parameters");
        sandbox(&p)?;
        match method {
            "environment/info" => Ok(self.info.clone()),
            "environment/status" => Ok(json!({"status":"ready"})),
            "process/start" => self.run(session, out, p).await,
            "process/read" => {
                let process = session
                    .processes
                    .lock()
                    .await
                    .get(string(&p, "processId")?)
                    .cloned()
                    .context("Unknown process receipt")?;
                let state = process.state.lock().await;
                ensure!(
                    state.closed,
                    "SSH commands run synchronously; no background process handles are available"
                );
                let after = p["afterSeq"].as_u64().unwrap_or(0);
                ensure!(
                    after >= state.discarded,
                    "Output cursor expired; retained output exceeded 4 MiB"
                );
                let mut chunks = vec![];
                let mut size = 0;
                let mut next = state.next;
                let max = p["maxBytes"].as_u64().unwrap_or(4 * 1024 * 1024) as usize;
                for chunk in state
                    .chunks
                    .iter()
                    .filter(|c| c["seq"].as_u64().unwrap_or(0) > after)
                {
                    let len = chunk["chunk"].as_str().map(str::len).unwrap_or(0) * 3 / 4;
                    if !chunks.is_empty() && size + len > max {
                        next = chunk["seq"].as_u64().unwrap();
                        break;
                    }
                    size += len;
                    chunks.push(chunk.clone());
                }
                Ok(
                    json!({"chunks":chunks,"nextSeq":next,"exited":state.code.is_number(),"exitCode":state.code,"closed":true,"failure":state.failure,"sandboxDenied":false}),
                )
            }
            "process/terminate" => {
                let process = session
                    .processes
                    .lock()
                    .await
                    .get(string(&p, "processId")?)
                    .cloned()
                    .context("Unknown process receipt")?;
                if !process.state.lock().await.closed {
                    let _ = process.cancel.try_send(());
                }
                Ok(json!({"running":!process.state.lock().await.closed}))
            }
            "fs/readFile" | "fs/writeFile" | "fs/getMetadata" | "fs/canonicalize"
            | "fs/readDirectory" | "fs/createDirectory" | "fs/remove" => {
                tokio::time::timeout(Duration::from_secs(45), async {
                    let mut files = sftp::Sftp::connect(&self.config).await?;
                    files.call(method, &p).await
                })
                .await
                .context(
                    "SFTP operation timed out; outcome uncertain, never replay automatically",
                )?
            }
            "capabilityRoots/discoverV1" => Ok(
                json!({"roots":p["roots"].as_array().context("Missing roots")?.iter().map(|root|json!({"id":root["id"],"path":root["path"],"skills":[],"namespaceManifests":[],"warnings":[],"error":"SSH capability discovery is unsupported"})).collect::<Vec<_>>()}),
            ),
            _ => unsupported(method),
        }
    }
    async fn run(&self, session: &Session, out: &mpsc::Sender<Value>, p: Value) -> Result<Value> {
        if p["tty"] == true || p["pipeStdin"] == true || !p["arg0"].is_null() {
            return unsupported("interactive PTYs, stdin pipes or argv0 overrides");
        }
        let id = string(&p, "processId")?.to_owned();
        ensure!(id.len() <= 256, "Process ID too long");
        let argv = p["argv"].as_array().context("Missing argv")?;
        ensure!(
            !argv.is_empty()
                && argv
                    .iter()
                    .all(|a| a.as_str().is_some_and(|a| !a.contains('\0'))),
            "argv must contain non-NUL strings"
        );
        let cwd = sftp::uri_path(string(&p, "cwd")?)?;
        let fingerprint = fingerprint(&p)?;
        let mut processes = session.processes.lock().await;
        if let Some(old) = processes.get(&id) {
            ensure!(
                old.params == fingerprint,
                "Process ID already used with different parameters"
            );
            let state = old.state.lock().await;
            ensure!(
                state.closed && state.failure.is_null(),
                "Previous command pending or uncertain; it will not be replayed"
            );
            return Ok(json!({"processId":id,"sandboxType":"none"}));
        }
        ensure!(
            processes.len() < 1024,
            "Receipt limit reached; replace the executor explicitly"
        );
        let mut active = 0;
        let mut completed = 0;
        for old in processes.values() {
            let mut state = old.state.lock().await;
            if !state.closed {
                active += 1
            } else {
                completed += 1;
                if completed > 16 {
                    state.discarded = state
                        .chunks
                        .back()
                        .and_then(|c| c["seq"].as_u64())
                        .unwrap_or(state.discarded);
                    state.chunks.clear();
                    state.bytes = 0;
                }
            }
        }
        ensure!(active < 32, "At most 32 concurrent SSH commands");
        let (cancel, mut cancelled) = mpsc::channel(1);
        let process = Arc::new(Process {
            params: fingerprint,
            state: Mutex::new(ProcessState {
                next: 1,
                ..Default::default()
            }),
            cancel,
        });
        processes.insert(id.clone(), process.clone());
        drop(processes);
        let result:Result<()>=async {
            let env=self.environment(&p).await?;
            let mut script=format!("test \"$(cat /proc/sys/kernel/random/boot_id)\" = {} || {{ printf '%s' 'SSH host rebooted; replace the executor' >&2; exit 255; }}; cd {} || exit 255; exec env {}-- ",quote(self.boot.as_str().context("Missing boot identity")?),quote(&cwd),if !p["envPolicy"].is_null(){"-i "}else{""});
            for (key,value) in env {ensure!(!key.is_empty() && !key.contains(['=','\0']) && !value.contains('\0'),"Invalid environment variable");script.push_str(&quote(&format!("{key}={value}")));script.push(' ');}
            for arg in argv{script.push_str(&quote(arg.as_str().unwrap()));script.push(' ');}
            let mut child=self.config.command(&script).spawn()?;
            let mut stdout=child.stdout.take().context("Missing stdout")?;let mut stderr=child.stderr.take().context("Missing stderr")?;
            let mut a=[0;16384];let mut b=[0;16384];let mut open_a=true;let mut open_b=true;
            while open_a || open_b {tokio::select!{
                read=stdout.read(&mut a),if open_a=>{let n=read?;if n==0{open_a=false}else{self.output_chunk(&process,out,&id,"stdout",&a[..n]).await?;}},
                read=stderr.read(&mut b),if open_b=>{let n=read?;if n==0{open_b=false}else{self.output_chunk(&process,out,&id,"stderr",&b[..n]).await?;}},
                _=cancelled.recv()=>{child.kill().await?;bail!("Local SSH cancelled; remote command outcome unknown. Remote termination is best effort");},
            }}
            let status=tokio::select!{status=child.wait()=>status?,_=cancelled.recv()=>{child.kill().await?;bail!("Local SSH cancelled; remote outcome unknown");}};
            ensure!(status.code().is_some() && status.code()!=Some(255),"SSH transport failed or returned reserved status 255; remote outcome unknown");
            let mut state=process.state.lock().await;state.code=json!(status.code());let seq=state.next;state.next+=1;drop(state);
            out.send(json!({"jsonrpc":"2.0","method":"process/exited","params":{"processId":id,"seq":seq,"exitCode":status.code(),"sandboxDenied":false}})).await?;Ok(())
        }.await;
        let mut state = process.state.lock().await;
        state.closed = true;
        if let Err(e) = &result {
            state.failure = json!(format!("{e:#}"));
        }
        let seq = state.next;
        state.next += 1;
        drop(state);
        let _=out.send(json!({"jsonrpc":"2.0","method":"process/closed","params":{"processId":id,"seq":seq}})).await;
        result?;
        Ok(json!({"processId":id,"sandboxType":"none"}))
    }
    async fn output_chunk(
        &self,
        process: &Process,
        out: &mpsc::Sender<Value>,
        id: &str,
        stream: &str,
        bytes: &[u8],
    ) -> Result<()> {
        use base64::Engine;
        let chunk = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut state = process.state.lock().await;
        let seq = state.next;
        state.next += 1;
        state.bytes += chunk.len();
        state
            .chunks
            .push_back(json!({"seq":seq,"stream":stream,"chunk":chunk}));
        while state.bytes > 4 * 1024 * 1024 {
            if let Some(old) = state.chunks.pop_front() {
                state.bytes -= old["chunk"].as_str().unwrap().len();
                state.discarded = old["seq"].as_u64().unwrap();
            }
        }
        drop(state);
        out.send(json!({"jsonrpc":"2.0","method":"process/output","params":{"processId":id,"seq":seq,"stream":stream,"chunk":chunk}})).await?;
        Ok(())
    }
    async fn environment(&self, p: &Value) -> Result<std::collections::BTreeMap<String, String>> {
        let mut env = std::collections::BTreeMap::<String, String>::new();
        if let Some(policy) = p["envPolicy"].as_object() {
            let inherit = policy
                .get("inherit")
                .and_then(Value::as_str)
                .context("Missing environment inheritance policy")?;
            ensure!(
                matches!(inherit, "all" | "core" | "none"),
                "Unsupported environment inheritance policy"
            );
            if inherit != "none" {
                for field in self
                    .config
                    .output("env -0")
                    .await?
                    .split(|b| *b == 0)
                    .filter(|b| !b.is_empty())
                {
                    let text = std::str::from_utf8(field)?;
                    let (key, value) =
                        text.split_once('=').context("Invalid remote environment")?;
                    if inherit == "all"
                        || [
                            "HOME",
                            "LOGNAME",
                            "PATH",
                            "SHELL",
                            "USER",
                            "USERNAME",
                            "SYSTEMROOT",
                            "TEMP",
                            "TMP",
                            "TMPDIR",
                        ]
                        .contains(&key)
                    {
                        env.insert(key.into(), value.into());
                    }
                }
            }
            let patterns = |key: &str| -> Vec<String> {
                policy
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_uppercase)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let mut exclude = patterns("exclude");
            if policy.get("ignoreDefaultExcludes") != Some(&json!(true)) {
                exclude.extend(["*KEY*", "*SECRET*", "*TOKEN*"].map(str::to_owned));
            }
            let include = patterns("includeOnly");
            env.retain(|key, _| {
                !exclude.iter().any(|p| glob(p, &key.to_uppercase()))
                    && (include.is_empty() || include.iter().any(|p| glob(p, &key.to_uppercase())))
            });
            if let Some(set) = policy.get("set").and_then(Value::as_object) {
                for (key, value) in set {
                    env.insert(
                        key.clone(),
                        value
                            .as_str()
                            .context("Environment values must be strings")?
                            .into(),
                    );
                }
            }
        }
        if let Some(set) = p["env"].as_object() {
            for (key, value) in set {
                env.insert(
                    key.clone(),
                    value
                        .as_str()
                        .context("Environment values must be strings")?
                        .into(),
                );
            }
        }
        Ok(env)
    }
}
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut i, mut j, mut star, mut saved) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            saved = j;
        } else if let Some(s) = star {
            saved += 1;
            j = saved;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

/// Local adapter endpoints are only meaningful in the daemon's network namespace.
pub fn require_local_app_server(
    endpoint: &str,
    selection: &[crate::targets::Selection],
) -> Result<()> {
    if !selection.iter().any(|target| target.id.starts_with("ssh-")) {
        return Ok(());
    }
    let local = endpoint.starts_with("unix://")
        || endpoint
            .parse::<axum::http::Uri>()
            .ok()
            .and_then(|uri| uri.host().map(str::to_owned))
            .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]"));
    ensure!(
        local,
        "SSH targets require an app-server on this Demodex host; their local adapter cannot be reached by a remote app-server"
    );
    Ok(())
}

// The callback signature is fixed by tungstenite.
#[allow(clippy::result_large_err)]
fn reject_browser_origin(
    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
    response: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> std::result::Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    if request.headers().contains_key("origin") {
        Err(tokio_tungstenite::tungstenite::http::Response::builder()
            .status(403)
            .body(Some("Browser access forbidden".into()))
            .unwrap())
    } else {
        Ok(response)
    }
}

fn fingerprint(value: &Value) -> Result<String> {
    use sha2::Digest;
    Ok(sha2::Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_survives_restart_and_rejects_unsafe_configuration() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sessions.db");
        let config = Config {
            name: "Builder".into(),
            destination: "user@builder".into(),
            cwd: "/workspace".into(),
            port: None,
            identity_file: None,
            known_hosts_file: None,
        };
        let id = {
            let store = crate::store::Store::open(&path)?;
            store.register_ssh_target(&config)?
        };
        let store = crate::store::Store::open(&path)?;
        assert_eq!(store.ssh_targets()?[0].0, id);
        assert_eq!(store.ssh_targets()?[0].1.destination, config.destination);
        assert!(
            Config {
                destination: "-oProxyCommand=invalid".into(),
                ..config.clone()
            }
            .validate()
            .is_err()
        );
        assert!(sandbox(&json!({"sandbox":{"permissions":{"type":"managed"}}})).is_err());
        assert!(sandbox(&json!({"sandbox":{"permissions":{"type":"external"}}})).is_err());
        assert!(sandbox(&json!({"sandbox":{"permissions":{"type":"disabled"}}})).is_ok());
        assert!(sandbox(&json!({"enforceManagedNetwork":true})).is_err());
        store.forget_target(&id)?;
        assert!(store.ssh_targets()?.is_empty());
        Ok(())
    }
}
