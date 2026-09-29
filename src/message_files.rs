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

async fn metadata(target: &Target, path: &str) -> Result<Value> {
    let mut socket = connect(target).await?;
    request(
        &mut socket,
        2,
        "fs/getMetadata",
        json!({"path":crate::ssh::sftp::path_uri(path)}),
    )
    .await
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
                        metadata(target, &check.path).await
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

pub async fn listing(manager: &crate::manager::Manager, id: &str, item: &str) -> Result<Value> {
    let Some((targets, mut files)) = manager.store.message_files(id, item)? else {
        return Ok(json!({"files":[],"note":"No metadata snapshot for this message"}));
    };
    let session = manager.store.get(id)?;
    let connected = manager.live.lock().await.contains_key(id);
    for file in &mut files {
        for check in &mut file.checks {
            check.available = connected
                && targets.iter().any(|old| {
                    old.id == check.executor
                        && session
                            .targets
                            .iter()
                            .any(|current| current.id == old.id && current.url == old.url)
                });
        }
    }
    Ok(json!({"files":files}))
}

pub async fn read(
    manager: &crate::manager::Manager,
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
    let _live = manager.runtime(id).await?;
    let current = manager.store.get(id)?;
    ensure!(
        current
            .targets
            .iter()
            .any(|t| t.id == target.id && t.url == target.url),
        "The original executor is no longer attached or has been replaced"
    );
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let _permit = manager.file_slots.acquire().await?;
        let mut socket = connect(target).await?;
        let params = json!({"path":crate::ssh::sftp::path_uri(&check.path)});
        let metadata = request(&mut socket, 2, "fs/getMetadata", params.clone()).await?;
        ensure!(metadata["isFile"] == true, "Only regular files can be read");
        ensure!(metadata["size"].as_u64().is_some_and(|n| n <= LIMIT as u64), "File exceeds the 4 MiB download limit or has unknown size");
        let result = request(&mut socket, 3, "fs/readFile", params).await?;
        let encoded = result["dataBase64"].as_str().context("Executor returned no file bytes")?;
        ensure!(encoded.len() <= LIMIT.div_ceil(3) * 4, "File grew beyond the 4 MiB limit");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        ensure!(bytes.len() <= LIMIT, "File grew beyond the 4 MiB limit");
        Ok::<_, anyhow::Error>(json!({"dataBase64":encoded,"path":check.path,"read_at_ms":now_ms(),"size":bytes.len()}))
    }).await.context("File read timed out")??;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
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
