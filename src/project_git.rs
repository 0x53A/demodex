//! Cached, bounded Git metadata through the project's exact executor.
use anyhow::{Context, Result, ensure};
use base64::Engine;
use demodex_protocol::{GitStatus, ProjectGit, Session, Target};
use futures_util::{SinkExt, StreamExt, stream};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

const LIMIT: usize = 256 * 1024;
// No network, hooks, fsmonitor commands, index refresh or submodule recursion.
// timeout is intentionally required on the executor, bounding remote work even
// when the connection is lost. No project paths are interpolated into this code.
const SCRIPT: &str = r#"
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CEILING_DIRECTORIES
export LC_ALL=C GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_NO_LAZY_FETCH=1
ro_git() { git --no-optional-locks -c core.fsmonitor=false -c core.untrackedCache=false -c core.hooksPath=/dev/null "$@"; }
root=$(ro_git rev-parse --show-toplevel) || exit $?
prefix=$(ro_git rev-parse --show-prefix) || exit $?
printf '%s\0%s\0' "$root" "$prefix"
ro_git status --porcelain=v2 --branch -z --untracked-files=normal --ignore-submodules=all
"#;

type Key = (String, String, String);
#[derive(Default)]
pub struct Cache(Mutex<BTreeMap<Key, (Instant, ProjectGit)>>);

pub fn project(session: &Session) -> Option<(Target, String)> {
    let context = session
        .presentation
        .context
        .as_ref()
        .filter(|c| c.path.starts_with('/'));
    let id = demodex_protocol::location::preferred(
        session.targets.iter().map(|t| t.id.as_str()),
        context.map(|c| c.environment_id.as_str()),
    )?;
    let target = session.targets.iter().find(|t| t.id == id)?.clone();
    let path = context
        .filter(|c| c.environment_id == id)
        .map(|c| c.path.clone())
        .unwrap_or_else(|| target.cwd.clone());
    Some((target, path))
}

fn empty(target: &Target, path: &str, state: &str, error: Option<String>) -> ProjectGit {
    ProjectGit {
        target: demodex_protocol::location::target_id(&target.id).into(),
        executor: target.id.clone(),
        path: format!(
            "/{}",
            path.split('/')
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        ),
        state: state.into(),
        checked_at_ms: crate::message_files::now_ms(),
        status: None,
        error,
    }
}

impl Cache {
    pub async fn snapshot(&self, manager: &crate::manager::Manager) -> Result<Vec<ProjectGit>> {
        let sessions = manager.store.list()?;
        let live = manager.live.lock().await;
        let mut projects = BTreeMap::new();
        for session in &sessions {
            if let Some((target, path)) = project(session) {
                let key = (
                    demodex_protocol::location::target_id(&target.id).to_owned(),
                    path.clone(),
                );
                let connected = live.contains_key(&session.id);
                let entry =
                    projects
                        .entry(key)
                        .or_insert((target.clone(), path.clone(), connected));
                if connected {
                    *entry = (target, path, true);
                }
            }
        }
        drop(live);
        let mut cache = self.0.lock().await;
        cache.retain(|(id, url, path), _| {
            projects
                .values()
                .any(|(t, p, connected)| *connected && &t.id == id && &t.url == url && p == path)
        });
        let mut output = Vec::new();
        let mut pending = Vec::new();
        for (target, path, connected) in projects.into_values() {
            let key = (target.id.clone(), target.url.clone(), path.clone());
            if !connected {
                output.push(empty(
                    &target,
                    &path,
                    "unavailable",
                    Some("Reconnect a session in this project to check Git".into()),
                ));
            } else if let Some((_, value)) = cache
                .get(&key)
                .filter(|(at, _)| at.elapsed() < Duration::from_secs(30))
            {
                output.push(value.clone());
            } else {
                pending.push((key, target, path));
            }
        }
        // Four foreground commands globally; callers share the cache lock.
        // Cap one refresh at 64 projects and 12 seconds, including queued checks.
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut checks = stream::iter(pending.into_iter().enumerate().map(
            |(i, (key, target, path))| async move {
                let result = if i >= 64 {
                    Err(anyhow::anyhow!("Git check limit reached (64 projects)"))
                } else {
                    tokio::time::timeout_at(
                        deadline.min(Instant::now() + Duration::from_secs(5)),
                        probe(&target, &path),
                    )
                    .await
                    .context("Git check timed out")
                    .and_then(|r| r)
                };
                let value = match result {
                    Ok(value) => value,
                    Err(error) => empty(&target, &path, "unavailable", Some(format!("{error:#}"))),
                };
                (key, value)
            },
        ))
        .buffer_unordered(4);
        while let Some((key, value)) = checks.next().await {
            cache.insert(key, (Instant::now(), value.clone()));
            output.push(value);
        }
        // Do not attribute results to a replacement or detached executor.
        let current = manager.store.list()?;
        let live = manager.live.lock().await;
        output.retain(|row| {
            current.iter().any(|s| {
                project(s).is_some_and(|(t, p)| {
                    t.id == row.executor && empty(&t, &p, "", None).path == row.path
                }) && (row.state == "unavailable" || live.contains_key(&s.id))
            })
        });
        Ok(output)
    }
}

async fn probe(target: &Target, path: &str) -> Result<ProjectGit> {
    ensure!(
        path.starts_with('/') && !path.contains('\0'),
        "Invalid project directory"
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(512 * 1024))
        .max_frame_size(Some(512 * 1024));
    let (mut socket, _) =
        tokio_tungstenite::connect_async_with_config(&target.url, Some(config), false)
            .await
            .context("Cannot connect to project executor")?;
    socket
        .send(Message::Text(
            json!({"id":1,"method":"initialize","params":{"clientName":"demodex-project-git"}})
                .to_string()
                .into(),
        ))
        .await?;
    // Native exec-server replaces rather than inherits the supplied environment.
    // Only the daemon's own host can use its PATH; never send host store paths
    // to a remote executor. Remote checks use standard Linux/NixOS locations.
    let system_path = "/run/current-system/sw/bin:/usr/local/bin:/usr/bin:/bin";
    let command_path = if demodex_protocol::location::target_id(&target.id) == "host" {
        std::env::var("PATH").unwrap_or_else(|_| system_path.into())
    } else {
        system_path.into()
    };
    let process = uuid::Uuid::new_v4().to_string();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut started = false;
    let mut exited = None;
    while let Some(message) = socket.next().await {
        let Message::Text(raw) = message? else {
            continue;
        };
        let value: Value = serde_json::from_str(&raw)?;
        if value["id"] == 1 {
            ensure!(
                value["error"].is_null(),
                "Executor initialization failed: {}",
                value["error"]
            );
            socket
                .send(Message::Text(
                    json!({"method":"initialized","params":{}})
                        .to_string()
                        .into(),
                ))
                .await?;
            socket.send(Message::Text(json!({"id":2,"method":"process/start","params":{"processId":process,"argv":["timeout","-k","1s","4s","sh","-c",SCRIPT],"cwd":crate::ssh::sftp::path_uri(path),"env":{"PATH":command_path},"tty":false,"pipeStdin":false}}).to_string().into())).await?;
        } else if value["id"] == 2 {
            ensure!(
                value["error"].is_null(),
                "Git command failed: {}",
                value["error"]
            );
            started = true;
        } else if value["params"]["processId"] == process {
            match value["method"].as_str() {
                Some("process/output") => {
                    let chunk = base64::engine::general_purpose::STANDARD.decode(
                        value["params"]["chunk"]
                            .as_str()
                            .context("Missing executor output")?,
                    )?;
                    ensure!(
                        stdout.len() + stderr.len() + chunk.len() <= LIMIT,
                        "Git output exceeds 256 KiB"
                    );
                    if value["params"]["stream"] == "stdout" {
                        stdout.extend(chunk);
                    } else {
                        stderr.extend(chunk);
                    }
                }
                Some("process/exited") => {
                    exited = Some(
                        value["params"]["exitCode"]
                            .as_i64()
                            .context("Git exit status is unknown")?,
                    );
                }
                _ => {}
            }
        }
        if started && let Some(code) = exited {
            let _ = socket.close(None).await;
            return interpret(target, path, code, &stdout, &stderr);
        }
    }
    anyhow::bail!("Project executor disconnected before Git completed")
}

fn interpret(
    target: &Target,
    path: &str,
    code: i64,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<ProjectGit> {
    let error = String::from_utf8_lossy(stderr);
    if code == 128 && stdout.is_empty() && error.starts_with("fatal: not a git repository") {
        return Ok(empty(target, path, "not_repository", None));
    }
    ensure!(
        code == 0,
        "Git exited with {code}: {}",
        error.chars().take(512).collect::<String>()
    );
    let mut parts = stdout.split(|b| *b == 0);
    let root = std::str::from_utf8(parts.next().context("Missing Git root")?)?.to_owned();
    ensure!(root.starts_with('/'), "Invalid Git root");
    let prefix = parts.next().context("Missing Git prefix")?;
    let mut status = GitStatus {
        root,
        parent_levels: prefix.iter().filter(|b| **b == b'/').count() as u32,
        branch: String::new(),
        oid: String::new(),
        ahead: None,
        behind: None,
        staged: 0,
        unstaged: 0,
        untracked: 0,
        conflicts: 0,
    };
    while let Some(part) = parts.next() {
        if part.is_empty() {
            continue;
        }
        if let Some(branch) = part.strip_prefix(b"# branch.head ") {
            status.branch = String::from_utf8_lossy(branch).into_owned();
        } else if let Some(oid) = part.strip_prefix(b"# branch.oid ") {
            status.oid = String::from_utf8_lossy(oid).into_owned();
        } else if let Some(ab) = part.strip_prefix(b"# branch.ab ") {
            let ab = std::str::from_utf8(ab)?;
            let (a, b) = ab.split_once(' ').context("Invalid Git tracking counts")?;
            status.ahead = Some(
                a.strip_prefix('+')
                    .context("Invalid ahead count")?
                    .parse()?,
            );
            status.behind = Some(
                b.strip_prefix('-')
                    .context("Invalid behind count")?
                    .parse()?,
            );
        } else if matches!(part[0], b'1' | b'2' | b'u') {
            ensure!(
                part.len() > 4 && part[1] == b' ',
                "Invalid Git change record"
            );
            if part[0] == b'u' {
                status.conflicts += 1;
            } else {
                if part[2] != b'.' {
                    status.staged += 1;
                }
                if part[3] != b'.' {
                    status.unstaged += 1;
                }
            }
            if part[0] == b'2' {
                parts.next().context("Missing Git rename source")?;
            }
        } else if part.starts_with(b"? ") {
            status.untracked += 1;
        } else {
            ensure!(part.starts_with(b"# "), "Unexpected Git status record");
        }
    }
    ensure!(
        !status.branch.is_empty() && !status.oid.is_empty(),
        "Missing Git branch metadata"
    );
    let mut result = empty(target, path, "repository", None);
    result.status = Some(status);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: &str) -> Target {
        Target {
            id: "host-test".into(),
            url: "ws://127.0.0.1:1".into(),
            cwd: path.into(),
        }
    }
    fn git(path: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    async fn local(path: &std::path::Path) -> Result<ProjectGit> {
        let output = tokio::process::Command::new("sh")
            .args(["-c", SCRIPT])
            .current_dir(path)
            .output()
            .await?;
        interpret(
            &target(path.to_str().unwrap()),
            path.to_str().unwrap(),
            output.status.code().unwrap().into(),
            &output.stdout,
            &output.stderr,
        )
    }

    #[tokio::test]
    async fn partial_clone_status_never_fetches_missing_rename_objects() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("source");
        let clone = dir.path().join("clone");
        std::fs::create_dir(&source)?;
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "uploadpack.allowFilter", "true"]);
        std::fs::write(source.join("original"), "original content\n".repeat(200))?;
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "Initial"]);
        // A file transport exercises real partial-clone fetching without network I/O.
        let url = crate::ssh::sftp::path_uri(source.to_str().unwrap());
        git(
            dir.path(),
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                "clone",
            ],
        );
        git(&clone, &["read-tree", "HEAD"]);
        std::fs::write(
            clone.join("renamed"),
            "original content\n".repeat(199) + "changed\n",
        )?;
        git(&clone, &["add", "renamed"]);
        git(&clone, &["rm", "--cached", "original"]);
        // Rename similarity normally fetches the original blob, writes a pack,
        // and may launch maintenance even with optional locks disabled.
        let packs = || -> Result<Vec<std::ffi::OsString>> {
            let mut files = std::fs::read_dir(clone.join(".git/objects/pack"))?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?;
            files.sort();
            Ok(files)
        };
        let before = packs()?;
        let trace = dir.path().join("git-trace");
        let output = tokio::process::Command::new("sh")
            .args(["-c", SCRIPT])
            .env("GIT_TRACE", &trace)
            .current_dir(&clone)
            .output()
            .await?;
        assert!(
            !output.status.success(),
            "Missing objects must remain unavailable"
        );
        assert!(
            interpret(
                &target(clone.to_str().unwrap()),
                clone.to_str().unwrap(),
                output.status.code().unwrap().into(),
                &output.stdout,
                &output.stderr
            )
            .is_err()
        );
        assert_eq!(
            packs()?,
            before,
            "Status must not write fetched object packs"
        );
        let trace = std::fs::read_to_string(trace)?;
        assert!(
            !trace.contains(" fetch "),
            "Status attempted a fetch: {trace}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn repository_status_tracks_parents_changes_detached_heads_and_worktrees() -> Result<()> {
        let dir = tempfile::tempdir()?;
        assert_eq!(local(dir.path()).await?.state, "not_repository");
        git(dir.path(), &["init", "-b", "main"]);
        let initial = local(dir.path()).await?.status.unwrap();
        assert_eq!(initial.oid, "(initial)");
        assert_eq!(initial.branch, "main");
        std::fs::write(dir.path().join("tracked"), "original")?;
        git(dir.path(), &["add", "tracked"]);
        git(dir.path(), &["commit", "-m", "Initial"]);
        std::fs::create_dir_all(dir.path().join("a/b/c"))?;
        let clean = local(&dir.path().join("a/b/c")).await?.status.unwrap();
        assert_eq!(clean.parent_levels, 3);
        assert_eq!((clean.staged, clean.unstaged, clean.untracked), (0, 0, 0));
        std::fs::write(dir.path().join("tracked"), "modified")?;
        std::fs::write(dir.path().join("odd\nname"), "new")?;
        let dirty = local(&dir.path().join("a/b")).await?.status.unwrap();
        assert_eq!(dirty.parent_levels, 2);
        assert_eq!((dirty.staged, dirty.unstaged, dirty.untracked), (0, 1, 1));
        git(dir.path(), &["add", "tracked"]);
        assert_eq!(local(dir.path()).await?.status.unwrap().staged, 1);
        git(dir.path(), &["checkout", "--detach"]);
        assert_eq!(
            local(dir.path()).await?.status.unwrap().branch,
            "(detached)"
        );
        let worktree = dir.path().join("linked");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                worktree.to_str().unwrap(),
            ],
        );
        assert!(worktree.join(".git").is_file());
        assert_eq!(local(&worktree).await?.status.unwrap().branch, "linked");
        Ok(())
    }

    #[test]
    fn porcelain_counts_tracking_conflicts_and_rename_sources_without_parsing_filenames()
    -> Result<()> {
        let output=b"/repo\0a/b/\0# branch.oid deadbeef\0# branch.head main\0# branch.ab +2 -3\x002 R. metadata filename\0? fake source\0u UU conflict data\0? untracked\0";
        let status = interpret(&target("/repo/a/b"), "/repo/a/b", 0, output, b"")?
            .status
            .unwrap();
        assert_eq!((status.ahead, status.behind), (Some(2), Some(3)));
        assert_eq!(
            (
                status.staged,
                status.unstaged,
                status.untracked,
                status.conflicts
            ),
            (1, 0, 1, 1)
        );
        assert!(
            interpret(
                &target("/repo"),
                "/repo",
                128,
                b"",
                b"fatal: detected dubious ownership"
            )
            .is_err()
        );
        assert!(interpret(&target("/repo"), "/repo", 124, b"", b"").is_err());
        assert!(interpret(&target("/repo"), "/repo", 0, b"/repo\0\0", b"").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn executor_protocol_accepts_both_exit_and_start_reply_orders_and_never_falls_back()
    -> Result<()> {
        for exit_first in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let mut target = target("/not/a/host/path");
            target.url = format!("ws://{}", listener.local_addr()?);
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let init = socket.next().await.unwrap().unwrap();
                let init: Value = serde_json::from_str(init.to_text().unwrap()).unwrap();
                assert_eq!(init["method"], "initialize");
                socket
                    .send(Message::Text(
                        json!({"id":1,"result":{}}).to_string().into(),
                    ))
                    .await
                    .unwrap();
                let _initialized = socket.next().await.unwrap().unwrap();
                let start = socket.next().await.unwrap().unwrap();
                let start: Value = serde_json::from_str(start.to_text().unwrap()).unwrap();
                assert_eq!(start["params"]["cwd"], "file:///not/a/host/path");
                assert_eq!(start["params"]["argv"][0], "timeout");
                assert_eq!(start["params"]["argv"][6], SCRIPT);
                let id = &start["params"]["processId"];
                let reply = json!({"id":2,"result":{"processId":id}});
                if !exit_first {
                    socket
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .unwrap();
                }
                let bytes = b"/not\0a/host/path/\0# branch.oid abcdef\0# branch.head main\0";
                let chunk = base64::engine::general_purpose::STANDARD.encode(bytes);
                socket.send(Message::Text(json!({"method":"process/output","params":{"processId":id,"stream":"stdout","chunk":chunk}}).to_string().into())).await.unwrap();
                socket
                    .send(Message::Text(
                        json!({"method":"process/exited","params":{"processId":id,"exitCode":0}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                if exit_first {
                    socket
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .unwrap();
                }
                let _ = socket.next().await;
            });
            let result = probe(&target, &target.cwd).await?;
            assert_eq!(result.status.unwrap().parent_levels, 3);
            server.await?;
        }
        assert!(probe(&target("/"), "/").await.is_err());
        Ok(())
    }
}
