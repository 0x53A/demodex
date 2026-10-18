//! Bounded, durable metadata checks for links in completed assistant messages.
use anyhow::{Context, Result, ensure};
use base64::Engine;
use demodex_protocol::{FileCheck, LinkDestination, MessageFile, Target};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

const LIMIT: usize = 4 * 1024 * 1024;
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn resolved(cwd: &str, path: &str) -> String {
    // Preserve .. and symlink semantics; the executor, not the daemon, resolves it.
    if path.starts_with('/') {
        path.into()
    } else {
        format!("{}/{path}", cwd.trim_end_matches('/'))
    }
}

pub fn prepare(source: &str, targets: &[Target]) -> Vec<MessageFile> {
    demodex_protocol::links::extract(source)
        .into_iter()
        .filter_map(|link| {
            if let LinkDestination::File { path } = link.kind {
                Some((link.destination, path))
            } else {
                None
            }
        })
        .enumerate()
        .map(|(index, (destination, path))| MessageFile {
            destination,
            checks: if index < 10 {
                targets
                    .iter()
                    .map(|target| FileCheck {
                        executor: target.id.clone(),
                        executor_name: target.id.clone(),
                        host: None,
                        unavailable_reason: None,
                        path: resolved(&target.cwd, &path),
                        state: "pending".into(),
                        checked_at_ms: None,
                        metadata: None,
                        error: None,
                        available: false,
                    })
                    .collect()
            } else {
                vec![]
            },
            note: if index >= 10 {
                Some("Not checked: message limit of 10 files".into())
            } else if targets.is_empty() {
                Some("No executors were attached when this message completed".into())
            } else {
                None
            },
        })
        .collect()
}

pub fn name_executors(store: &crate::store::Store, files: &mut [MessageFile]) -> Result<()> {
    let mut names = vec![("host".to_owned(), "This host".to_owned())];
    names.extend(
        store
            .registered_targets()?
            .into_iter()
            .map(|t| (t.id, t.name)),
    );
    names.extend(store.ssh_targets()?.into_iter().map(|(id, c)| (id, c.name)));
    names.extend(
        store
            .environments()?
            .into_iter()
            .map(|e| (format!("vm-{}", e.id), e.name)),
    );
    names.extend(
        store
            .containers()?
            .into_iter()
            .map(|e| (format!("container-{}", e.id), e.name)),
    );
    for check in files.iter_mut().flat_map(|file| file.checks.iter_mut()) {
        if let Some((_, name)) = names
            .iter()
            .find(|(id, _)| check.executor == *id || check.executor.starts_with(&format!("{id}-")))
        {
            check.executor_name = name.clone();
        }
    }
    Ok(())
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn request(socket: &mut Socket, id: u64, method: &str, params: Value) -> Result<Value> {
    socket
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await?;
    while let Some(message) = socket.next().await {
        if let Message::Text(raw) = message? {
            let response: Value = serde_json::from_str(&raw)?;
            if response["id"] != id {
                continue;
            }
            if !response["error"].is_null() {
                // Do not turn permission, transport or unsupported errors into absence.
                if response["error"]["code"] == -32004
                    || response["error"]["data"]["code"] == "ENOENT"
                {
                    return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
                }
                anyhow::bail!("Executor operation failed: {}", response["error"]);
            }
            return Ok(response["result"].clone());
        }
    }
    anyhow::bail!("Executor connection closed")
}

async fn connect(target: &Target) -> Result<Socket> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(6 * 1024 * 1024))
        .max_frame_size(Some(6 * 1024 * 1024));
    let (mut socket, _) =
        tokio_tungstenite::connect_async_with_config(&target.url, Some(config), false).await?;
    request(
        &mut socket,
        1,
        "initialize",
        json!({"clientName":"demodex-message-files"}),
    )
    .await?;
    socket
        .send(Message::Text(
            json!({"method":"initialized","params":{}})
                .to_string()
                .into(),
        ))
        .await?;
    Ok(socket)
}

pub(crate) enum FileRoute {
    Executor(Target),
    Ssh(crate::ssh::Config),
}
fn file_error(error: anyhow::Error) -> anyhow::Error {
    if crate::ssh::is_missing_file(&error) {
        std::io::Error::from(std::io::ErrorKind::NotFound).into()
    } else { error }
}
enum FileConnection {
    Executor(Box<Socket>),
    Ssh(Box<crate::ssh::sftp::Sftp>),
}
impl FileRoute {
    async fn connect(&self) -> Result<FileConnection> {
        match self {
            Self::Executor(target) => Ok(FileConnection::Executor(Box::new(connect(target).await?))),
            Self::Ssh(config) => Ok(FileConnection::Ssh(Box::new(crate::ssh::sftp::Sftp::connect(config).await?))),
        }
    }
}
impl FileConnection {
    async fn call(&mut self, method: &str, path: &str) -> Result<Value> {
        let params = json!({"path":crate::ssh::sftp::path_uri(path)});
        match self {
            Self::Executor(socket) => request(socket, 10, method, params).await,
            Self::Ssh(sftp) if method == "fs/readFile" => Ok(json!({"dataBase64":base64::engine::general_purpose::STANDARD.encode(sftp.read_limited(path, LIMIT).await?)})),
            Self::Ssh(sftp) => sftp.call(method, &params).await.map_err(file_error),
        }
    }
    async fn identity(&mut self) -> Result<String> {
        use sha2::{Digest, Sha256};
        async fn small(conn: &mut FileConnection, path: &str) -> Result<String> {
            let value = conn.call("fs/getMetadata", path).await?;
            ensure!(value["isFile"] == true && value["size"].as_u64().is_some_and(|n| n <= 65536), "Host identity file unavailable or too large");
            let value = conn.call("fs/readFile", path).await?;
            let encoded = value["dataBase64"].as_str().context("Missing host identity bytes")?;
            ensure!(encoded.len() <= 65536*2, "Host identity file too large");
            Ok(String::from_utf8(base64::engine::general_purpose::STANDARD.decode(encoded)?)?)
        }
        let machine = small(self, "/etc/machine-id").await?;
        let machine = machine.trim();
        ensure!(machine.len() == 32 && machine.bytes().all(|b| b.is_ascii_hexdigit()) && machine.bytes().any(|b| b != b'0'), "Invalid machine ID");
        let status = small(self, "/proc/self/status").await?;
        let uid = status.lines().find_map(|line| line.strip_prefix("Uid:")).and_then(|v|v.split_whitespace().nth(3)).context("Filesystem account identity unavailable")?;
        let uid: u32 = uid.parse()?;
        Ok(Sha256::digest(format!("demodex-file-host-v1:{machine}:{uid}")).iter().map(|b| format!("{b:02x}")).collect())
    }
    async fn verify(&mut self, host: &demodex_protocol::FileHost) -> Result<()> {
        ensure!(self.identity().await? == host.fingerprint, "File host identity changed; refusing access to a different machine or account");
        Ok(())
    }
}

fn captured_route(store: &crate::store::Store, target: &Target) -> Result<(FileRoute, Option<String>)> {
    let stable = store.file_executor_host(target)?;
    if let Some(id) = stable.as_ref().filter(|id|id.starts_with("ssh-")) {
        let config = store.ssh_targets()?.into_iter().find(|(key,_)|key == id).context("Original SSH target was removed")?.1;
        return Ok((FileRoute::Ssh(config), stable));
    }
    Ok((FileRoute::Executor(target.clone()), stable))
}

async fn file_route(manager: &crate::manager::Manager, orchestrator: &crate::orchestrator::Orchestrator, id: &str, check: &FileCheck, original: &Target) -> Result<FileRoute> {
    if let Some(host) = &check.host {
        return orchestrator.file_host(&host.target).await;
    }
    let _live = manager.runtime(id).await.context("No durable host identity was recorded. The original session/executor must still be connected")?;
    ensure!(manager.store.get(id)?.targets.iter().any(|t|t.id == original.id && t.url == original.url), "No durable host identity was recorded and the original executor is no longer attached");
    Ok(captured_route(&manager.store, original)?.0)
}

pub async fn check(
    manager: Arc<crate::manager::Manager>,
    id: String,
    item: String,
    targets: Vec<Target>,
    mut files: Vec<MessageFile>,
    deadline: Instant,
) {
    for file in &mut files {
        if file.checks.is_empty() {
            continue;
        }
        if Instant::now() >= deadline {
            for check in &mut file.checks {
                check.state = "not-checked".into();
                check.error = Some("Overall deadline reached".into());
            }
            continue;
        }
        let file_deadline = deadline.min(Instant::now() + Duration::from_secs(3));
        // All executors for a file share its deadline. The daemon semaphore
        // bounds outstanding executor requests across all messages and reads.
        futures_util::stream::iter(file.checks.iter_mut())
            .for_each_concurrent(4, |check| {
                let manager = &manager;
                let targets = &targets;
                async move {
                    let target = targets
                        .iter()
                        .find(|target| target.id == check.executor)
                        .unwrap();
                    let result = tokio::time::timeout_at(file_deadline, async {
                        let _permit = manager.file_slots.acquire().await?;
                        let (route, stable) = captured_route(&manager.store, target)?;
                        let mut connection = route.connect().await?;
                        if let Some(stable) = stable {
                            match connection.identity().await {
                                Ok(fingerprint) => check.host = Some(demodex_protocol::FileHost { target: stable, fingerprint }),
                                Err(error) => check.unavailable_reason = Some(format!("Durable host identity unavailable: {error:#}")),
                            }
                        }
                        connection.call("fs/getMetadata", &check.path).await
                    })
                    .await;
                    check.checked_at_ms = Some(now_ms());
                    match result {
                        Ok(Ok(value)) => {
                            check.state = "found".into();
                            check.metadata = Some(value);
                        }
                        Ok(Err(error)) => {
                            check.state = if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                            {
                                "not-found"
                            } else {
                                "error"
                            }
                            .into();
                            check.error = Some(format!("{error:#}"));
                        }
                        Err(_) => {
                            check.state = "timed-out".into();
                            check.error = Some("Metadata check timed out".into());
                        }
                    }
                }
            })
            .await;
    }
    if let Err(error) = manager.store.finish_message_files(&id, &item, &files) {
        tracing::warn!(%error, "Could not persist file metadata checks");
    }
    manager.session_changed(&id, false);
}

/// Explicit, receipted refresh of one recorded destination on its original executors.
pub async fn refresh(manager: &crate::manager::Manager, orchestrator: &crate::orchestrator::Orchestrator, id: &str, item: &str, destination: &str) -> Result<Value> {
    let (targets, files) = manager.store.message_files(id, item)?.context("No metadata snapshot for this message")?;
    ensure!(!files.iter().flat_map(|f| &f.checks).any(|c| c.state == "pending"), "Initial metadata checks are still running");
    let mut file = files.into_iter().find(|f| f.destination == destination).context("No recorded file link")?;
    let LinkDestination::File { path } = demodex_protocol::links::classify(destination) else {
        anyhow::bail!("Destination is not a local file path");
    };
    let mut routes = std::collections::HashMap::new();
    for check in &file.checks {
        if let Some(original) = targets.iter().find(|t| t.id == check.executor)
            && let Ok(route) = file_route(manager, orchestrator, id, check, original).await
        {
            routes.insert(check.executor.clone(), route);
        }
    }
    ensure!(!routes.is_empty(), "No original file host is available; cannot refresh this file");
    let deadline = Instant::now() + Duration::from_secs(3);
    futures_util::stream::iter(file.checks.iter_mut()).for_each_concurrent(4, |check| {
        let route = routes.get(&check.executor);
        let original = targets.iter().find(|t| t.id == check.executor);
        let path = &path;
        async move {
            let (Some(route), Some(original)) = (route, original) else { return; };
            check.path = resolved(&original.cwd, path);
            check.metadata = None;
            check.error = None;
            let result = tokio::time::timeout_at(deadline, async {
                let _permit = manager.file_slots.acquire().await?;
                let mut connection = route.connect().await?;
                if let Some(host) = &check.host {
                    connection.verify(host).await?;
                } else if let Some(stable) = manager.store.file_executor_host(original)? {
                    check.host = Some(demodex_protocol::FileHost { target: stable, fingerprint: connection.identity().await? });
                }
                connection.call("fs/getMetadata", &check.path).await
            }).await;
            check.checked_at_ms = Some(now_ms());
            match result {
                Ok(Ok(value)) => { check.state = "found".into(); check.metadata = Some(value); }
                Ok(Err(error)) => {
                    check.state = if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) { "not-found" } else { "error" }.into();
                    check.error = Some(format!("{error:#}"));
                }
                Err(_) => { check.state = "timed-out".into(); check.error = Some("Metadata check timed out".into()); }
            }
        }
    }).await;
    manager.store.refresh_message_file(id, item, &file)?;
    manager.session_changed(id, false);
    listing(manager, orchestrator, id, item).await
}

/// Explicit pairing for legacy snapshots only; never replace a captured identity.
pub async fn bind_host(manager: &crate::manager::Manager, orchestrator: &crate::orchestrator::Orchestrator, id: &str, item: &str, destination: &str, executor: &str, target: &str) -> Result<Value> {
    let (targets, files) = manager.store.message_files(id, item)?.context("No metadata snapshot for this message")?;
    ensure!(!files.iter().flat_map(|f| &f.checks).any(|c|c.state == "pending"), "Initial metadata checks are still running");
    let mut file = files.into_iter().find(|f|f.destination == destination).context("No recorded file link")?;
    let check = file.checks.iter_mut().find(|c|c.executor == executor).context("No recorded executor for this file")?;
    ensure!(check.host.is_none(), "This file already has a pinned host identity");
    let original = targets.iter().find(|t|t.id == executor).context("Missing captured executor")?;
    let LinkDestination::File { path } = demodex_protocol::links::classify(destination) else { anyhow::bail!("Destination is not a local file path"); };
    let route = orchestrator.file_host(target).await?;
    let (fingerprint, metadata) = tokio::time::timeout(Duration::from_secs(10), async {
        let _permit = manager.file_slots.acquire().await?;
        let mut connection = route.connect().await?;
        let fingerprint = connection.identity().await?;
        let metadata = connection.call("fs/getMetadata", &resolved(&original.cwd, &path)).await;
        Ok::<_,anyhow::Error>((fingerprint,metadata))
    }).await.context("File host pairing timed out")??;
    check.host = Some(demodex_protocol::FileHost { target: target.into(), fingerprint });
    check.path = resolved(&original.cwd, &path);
    check.executor_name = target.into();
    check.checked_at_ms = Some(now_ms());
    check.metadata = None;
    check.error = None;
    match metadata {
        Ok(value) => { check.state = "found".into(); check.metadata = Some(value); }
        Err(error) => { check.state = if error.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind() == std::io::ErrorKind::NotFound) { "not-found" } else { "error" }.into(); check.error = Some(format!("{error:#}")); }
    }
    // Store refuses concurrent attempts to replace an established identity.
    manager.store.bind_message_file_host(id, item, &file, executor)?;
    manager.session_changed(id, false);
    listing(manager, orchestrator, id, item).await
}

pub async fn listing(manager: &crate::manager::Manager, orchestrator: &crate::orchestrator::Orchestrator, id: &str, item: &str) -> Result<Value> {
    let Some((targets, mut files)) = manager.store.message_files(id, item)? else {
        return Ok(json!({"files":[],"note":"No metadata snapshot for this message"}));
    };
    for file in &mut files {
        for check in &mut file.checks {
            let route = if let Some(original) = targets.iter().find(|t|t.id == check.executor) {
                file_route(manager, orchestrator, id, check, original).await
            } else { Err(anyhow::anyhow!("Missing captured executor")) };
            check.available = route.is_ok();
            check.unavailable_reason = route.err().map(|e|format!("{e:#}"));
        }
    }
    Ok(json!({"files":files}))
}

pub async fn read(
    manager: &crate::manager::Manager,
    orchestrator: &crate::orchestrator::Orchestrator,
    id: &str,
    item: &str,
    destination: &str,
    executor: &str,
) -> Result<Value> {
    let (targets, files) = manager
        .store
        .message_files(id, item)?
        .context("No metadata snapshot for this message")?;
    let check = files
        .iter()
        .find(|f| f.destination == destination)
        .and_then(|f| f.checks.iter().find(|c| c.executor == executor))
        .context("File was not checked on this executor")?;
    ensure!(
        check.state == "found",
        "File was not found in the recorded check"
    );
    let target = targets
        .iter()
        .find(|t| t.id == executor)
        .context("Missing captured executor")?;
    let route = file_route(manager, orchestrator, id, check, target).await?;
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let _permit = manager.file_slots.acquire().await?;
        let mut connection = route.connect().await?;
        if let Some(host) = &check.host { connection.verify(host).await?; }
        let metadata = connection.call("fs/getMetadata", &check.path).await?;
        ensure!(metadata["isFile"] == true, "Only regular files can be read");
        ensure!(metadata["size"].as_u64().is_some_and(|n| n <= LIMIT as u64), "File exceeds the 4 MiB download limit or has unknown size");
        let result = connection.call("fs/readFile", &check.path).await?;
        let encoded = result["dataBase64"].as_str().context("Executor returned no file bytes")?;
        ensure!(encoded.len() <= LIMIT.div_ceil(3) * 4, "File grew beyond the 4 MiB limit");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        ensure!(bytes.len() <= LIMIT, "File grew beyond the 4 MiB limit");
        Ok::<_, anyhow::Error>(json!({"dataBase64":encoded,"path":check.path,"read_at_ms":now_ms(),"size":bytes.len()}))
    }).await.context("File read timed out")??;
    Ok(result)
}

/// Only recorded uploads may load automatically; never read arbitrary Markdown paths.
pub async fn read_uploaded_image(manager: &crate::manager::Manager, id: &str, path: &str) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let (target, digest) = manager.store.uploaded_image(id, path)?.context("No recorded image upload for this path")?;
    let _live = manager.runtime(id).await?;
    let current = manager.store.get(id)?;
    // Uploads use the next-turn selection, which may not be active yet.
    let staged = manager.store.staged_targets(id)?.unwrap_or_default();
    ensure!(current.targets.iter().chain(&staged).any(|t| t.id == target.id && t.url == target.url), "The original image executor is unavailable or replaced");
    tokio::time::timeout(Duration::from_secs(10), async {
        let _permit = manager.file_slots.acquire().await?;
        let mut socket = connect(&target).await?;
        let params = json!({"path":crate::ssh::sftp::path_uri(path)});
        let metadata = request(&mut socket, 2, "fs/getMetadata", params.clone()).await?;
        ensure!(metadata["isFile"] == true && metadata["size"].as_u64().is_some_and(|n| n <= LIMIT as u64), "Image must be a regular file of at most 4 MiB");
        let result = request(&mut socket, 3, "fs/readFile", params).await?;
        let encoded = result["dataBase64"].as_str().context("Executor returned no image bytes")?;
        ensure!(encoded.len() <= LIMIT.div_ceil(3)*4, "Image exceeds 4 MiB");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        let extension = crate::uploads::extension(&bytes)?;
        ensure!(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>() == digest, "Uploaded image has changed");
        let mime = if extension == "jpg" { "image/jpeg".to_owned() } else { format!("image/{extension}") };
        Ok::<_,anyhow::Error>(json!({"dataBase64":encoded,"mime":mime}))
    }).await.context("Image read timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairing_and_refresh_cannot_replace_a_concurrently_pinned_identity() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = crate::store::Store::open(&temp.path().join("state.sqlite"))?;
        let target = Target { id:"original".into(), url:"ws://127.0.0.1:1".into(), cwd:"/".into() };
        let session = store.create("files", "ws://127.0.0.1:2", std::slice::from_ref(&target), None)?;
        let mut files = prepare("[A](/a) [B](/b)", &[target]);
        for file in &mut files { file.checks[0].state="found".into(); }
        store.claim_message_files(&session.id, "item", &[], &files)?;
        let stale = files[0].clone();
        files[0].checks[0].host = Some(demodex_protocol::FileHost {target:"host".into(),fingerprint:"first".into()});
        store.bind_message_file_host(&session.id, "item", &files[0], "original")?;
        assert!(store.refresh_message_file(&session.id, "item", &stale).is_err());
        files[0].checks[0].host.as_mut().unwrap().fingerprint="other".into();
        assert!(store.bind_message_file_host(&session.id, "item", &files[0], "original").is_err());
        let (_, saved) = store.message_files(&session.id, "item")?.unwrap();
        assert_eq!(saved[0].checks[0].host.as_ref().unwrap().fingerprint, "first");
        assert_eq!(saved[1], files[1]);
        Ok(())
    }

    #[tokio::test]
    async fn initial_checks_capture_only_registered_host_provenance() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(Message::Text(raw))) = socket.next().await {
                        let request: Value = serde_json::from_str(&raw).unwrap();
                        if request["id"].is_null() { continue; }
                        let result = match request["method"].as_str() {
                            Some("fs/getMetadata") => json!({"isFile":true,"size":32}),
                            Some("fs/readFile") => {
                                let bytes = if request["params"]["path"] == "file:///etc/machine-id" {
                                    "11111111111111111111111111111111\n"
                                } else { "Uid:\t1000\t1000\t1000\t1000\n" };
                                json!({"dataBase64":base64::engine::general_purpose::STANDARD.encode(bytes)})
                            }
                            _ => json!({}),
                        };
                        if socket.send(Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).await.is_err() { break; }
                    }
                });
            }
        });
        let temp = tempfile::tempdir()?;
        let store = crate::store::Store::open(&temp.path().join("state.sqlite"))?;
        let genuine = Target { id:"host-real-generation".into(), url:format!("ws://{address}"), cwd:"/".into() };
        let lookalike = Target { id:"host-fake-generation".into(), ..genuine.clone() };
        store.record_file_executor(&genuine, "host")?;
        let targets = vec![genuine, lookalike];
        let session = store.create("files", "ws://127.0.0.1:2", &targets, None)?;
        let files = prepare("[File](/report)", &targets);
        store.claim_message_files(&session.id, "item", &targets, &files)?;
        let manager = crate::manager::Manager::new(store);
        check(manager.clone(), session.id.clone(), "item".into(), targets, files, Instant::now()+Duration::from_secs(10)).await;
        let (_, files) = manager.store.message_files(&session.id, "item")?.unwrap();
        assert_eq!(files[0].checks[0].host.as_ref().unwrap().target, "host");
        assert_eq!(files[0].checks[0].state, "found");
        assert!(files[0].checks[1].host.is_none(), "A display prefix must not authorize rebinding");
        assert_eq!(files[0].checks[1].state, "found");
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn uploaded_images_are_durable_session_scoped_and_require_live_executors() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("state.sqlite");
        let store = crate::store::Store::open(&db)?;
        let target = Target { id: "original".into(), url: "ws://127.0.0.1:1".into(), cwd: "/remote".into() };
        let session = store.create("image", "ws://127.0.0.1:2", std::slice::from_ref(&target), None)?;
        let other = store.create("other", "ws://127.0.0.1:2", &[], None)?;
        store.record_uploaded_image(&session.id, "/remote/image.png", &target, "digest")?;
        drop(store);
        let store = crate::store::Store::open(&db)?;
        assert_eq!(store.uploaded_image(&session.id, "/remote/image.png")?, Some((target, "digest".into())));
        assert!(store.uploaded_image(&other.id, "/remote/image.png")?.is_none());
        let manager = crate::manager::Manager::new(store);
        assert!(read_uploaded_image(&manager, &session.id, "/etc/passwd").await.unwrap_err().to_string().contains("No recorded"));
        assert!(read_uploaded_image(&manager, &session.id, "/remote/image.png").await.is_err());
        Ok(())
    }
    #[test]
    fn location_links_capture_underlying_executor_path() {
        let targets = [Target {
            id: "original-executor".into(),
            url: "ws://127.0.0.1:1".into(),
            cwd: "/remote/work".into(),
        }];
        let files = prepare("[Design](/project/DESIGN.md:1) [Source](./src/main.rs:12:4)", &targets);
        assert_eq!(files[0].destination, "/project/DESIGN.md:1");
        assert_eq!(files[0].checks[0].path, "/project/DESIGN.md");
        assert_eq!(files[1].destination, "./src/main.rs:12:4");
        assert_eq!(files[1].checks[0].path, "/remote/work/./src/main.rs");
        assert_eq!(files[1].checks[0].executor, "original-executor");
    }

    #[test]
    fn snapshots_bound_files_and_keep_executor_paths() {
        let targets = vec![Target {
            id: "remote-generation".into(),
            url: "ws://127.0.0.1:1".into(),
            cwd: "/remote/work".into(),
        }];
        let mut source =
            "[Report](report.json) [Again](report.json) [Web](https://example.org)".to_owned();
        for n in 0..11 {
            source.push_str(&format!(" [File](/tmp/{n})"));
        }
        let files = prepare(&source, &targets);
        assert_eq!(files.len(), 12);
        assert_eq!(files[0].checks[0].path, "/remote/work/report.json");
        assert_eq!(files.iter().filter(|f| !f.checks.is_empty()).count(), 10);
        assert!(files[10].note.as_ref().unwrap().contains("limit"));
    }

    #[test]
    fn snapshots_survive_restart_without_replaying_interrupted_checks() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("state.sqlite");
        let store = crate::store::Store::open(&db)?;
        let targets = vec![Target {
            id: "generation-a".into(),
            url: "ws://127.0.0.1:1".into(),
            cwd: "/remote".into(),
        }];
        let session = store.create("files", "ws://127.0.0.1:2", &targets, None)?;
        let mut files = prepare("[A](a) [B](b)", &targets);
        assert!(store.claim_message_files(&session.id, "item", &targets, &files)?);
        assert!(!store.claim_message_files(&session.id, "item", &targets, &files)?);
        files[0].checks[0].state = "found".into();
        files[0].checks[0].checked_at_ms = Some(123);
        store.finish_message_files(&session.id, "item", &files)?;
        drop(store);
        let store = crate::store::Store::open(&db)?;
        let (saved, files) = store.message_files(&session.id, "item")?.unwrap();
        assert_eq!(saved, targets);
        assert_eq!(files[0].checks[0].state, "found");
        assert_eq!(files[0].checks[0].checked_at_ms, Some(123));
        assert_eq!(files[1].checks[0].state, "not-checked");
        assert!(
            files[1].checks[0]
                .error
                .as_ref()
                .unwrap()
                .contains("interrupted")
        );
        Ok(())
    }

    #[tokio::test]
    async fn slow_file_does_not_prevent_the_next_file_check() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(raw))) = socket.next().await {
                        let request: Value = serde_json::from_str(&raw).unwrap();
                        if request["id"].is_null() {
                            continue;
                        }
                        if request["params"]["path"] == "file:///slow" {
                            tokio::time::sleep(Duration::from_secs(4)).await;
                        }
                        let result = if request["method"] == "fs/getMetadata" {
                            json!({"isFile":true,"size":12})
                        } else {
                            json!({})
                        };
                        if socket
                            .send(Message::Text(
                                json!({"id":request["id"],"result":result})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        let temp = tempfile::tempdir()?;
        let store = crate::store::Store::open(&temp.path().join("state.sqlite"))?;
        let targets = vec![Target {
            id: "fixture".into(),
            url: format!("ws://{address}"),
            cwd: "/".into(),
        }];
        let session = store.create("files", "ws://127.0.0.1:2", &targets, None)?;
        let files = prepare("[Slow](/slow) [Fast](/fast)", &targets);
        store.claim_message_files(&session.id, "item", &targets, &files)?;
        let manager = crate::manager::Manager::new(store);
        let started = Instant::now();
        check(
            manager.clone(),
            session.id.clone(),
            "item".into(),
            targets,
            files,
            started + Duration::from_secs(10),
        )
        .await;
        let (_, files) = manager.store.message_files(&session.id, "item")?.unwrap();
        assert_eq!(files[0].checks[0].state, "timed-out");
        assert_eq!(files[1].checks[0].state, "found");
        assert!(started.elapsed() < Duration::from_secs(6));
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn overall_deadline_keeps_results_and_marks_unstarted_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = crate::store::Store::open(&temp.path().join("state.sqlite"))?;
        let targets = vec![Target {
            id: "slow".into(),
            url: "ws://127.0.0.1:1".into(),
            cwd: "/".into(),
        }];
        let session = store.create("files", "ws://127.0.0.1:2", &targets, None)?;
        let files = prepare("[A](a) [B](b)", &targets);
        store.claim_message_files(&session.id, "item", &targets, &files)?;
        let manager = crate::manager::Manager::new(store);
        // Saturate the global budget so no executor request can start.
        let _permits = manager.file_slots.acquire_many(8).await?;
        check(
            manager.clone(),
            session.id.clone(),
            "item".into(),
            targets,
            files,
            Instant::now() + Duration::from_millis(25),
        )
        .await;
        let (_, files) = manager.store.message_files(&session.id, "item")?.unwrap();
        assert_eq!(files[0].checks[0].state, "timed-out");
        assert_eq!(files[1].checks[0].state, "not-checked");
        Ok(())
    }
}
