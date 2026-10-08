//! Explicit takeover pins the verified CLI daemon; never stop by profile or PID.
use crate::{orchestrator::Orchestrator, rpc::Rpc, store::Session};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

fn conflict(session: &Session) -> bool {
    session.status == "disconnected"
        && !session.archived
        && session.thread_id.as_deref().is_some_and(|thread| {
            demodex_protocol::active_writer_conflict(session.error.as_deref().unwrap_or(""), thread)
        })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonRecord {
    pid: i32,
    process_identity: ProcessIdentity,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessIdentity {
    boot_id: String,
    start_ticks: u64,
}

async fn identity(profile: &Path) -> Result<(String, DaemonRecord)> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    tokio::fs::File::open(profile.join("app-server-daemon/daemon.pid"))
        .await
        .context("Codex CLI daemon identity is unavailable")?
        .take(16_385)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= 16_384, "Invalid Codex daemon identity");
    let record: DaemonRecord =
        serde_json::from_slice(&bytes).context("Unsupported Codex daemon identity")?;
    ensure!(record.pid > 1, "Invalid Codex daemon PID");
    let digest = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((digest, record))
}

#[cfg(target_os = "linux")]
struct PinnedDaemon(tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>);

#[cfg(target_os = "linux")]
impl PinnedDaemon {
    async fn open(record: &DaemonRecord) -> Result<Self> {
        use rustix::process::{Pid, PidfdFlags, pidfd_open};
        ensure!(
            record.pid > 1 && record.pid as u32 != std::process::id(),
            "Refusing to stop this process"
        );
        let pid = Pid::from_raw(record.pid).context("Invalid Codex daemon PID")?;
        // Pin BEFORE checking /proc. Reuse or replacement after validation can
        // never redirect the signal to another process. No numeric-PID fallback.
        let fd = pidfd_open(pid, PidfdFlags::empty())
            .context("Cannot pin the CLI daemon; safe takeover requires Linux pidfd support")?;
        let boot = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id").await?;
        let stat = tokio::fs::read_to_string(format!("/proc/{}/stat", record.pid)).await?;
        ensure!(
            boot.trim() == record.process_identity.boot_id
                && start_ticks(&stat)? == record.process_identity.start_ticks,
            "CLI daemon process identity changed; nothing was stopped"
        );
        Ok(Self(tokio::io::unix::AsyncFd::new(fd)?))
    }

    async fn stop(self) -> Result<()> {
        use rustix::process::{Signal, pidfd_send_signal};
        pidfd_send_signal(self.0.get_ref(), Signal::TERM)
            .context("The reviewed CLI daemon could not be stopped; refresh before takeover")?;
        // Wait for this exact process to exit, not for a replaceable PID file or
        // socket path to disappear. Never escalate to SIGKILL or signal a group.
        let _exited = tokio::time::timeout(Duration::from_secs(30), self.0.readable())
            .await
            .context(
                "CLI stop timed out; outcome is uncertain. Inspect the CLI server before retrying.",
            )??;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn start_ticks(stat: &str) -> Result<u64> {
    // comm (field 2) can contain spaces and parentheses; field 22 follows it.
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .context("Invalid CLI daemon process stat")?
        .parse()
        .context("Invalid CLI daemon process start time")
}

#[cfg(not(target_os = "linux"))]
struct PinnedDaemon;

#[cfg(not(target_os = "linux"))]
impl PinnedDaemon {
    async fn open(_: &DaemonRecord) -> Result<Self> {
        anyhow::bail!(
            "Safe CLI takeover requires Linux pidfd support; stop the original controller and reconnect"
        )
    }
    async fn stop(self) -> Result<()> {
        anyhow::bail!("Safe CLI takeover is unavailable")
    }
}

async fn inspect(
    app: &Orchestrator,
    session: &Session,
) -> Result<(PinnedDaemon, String, Vec<String>)> {
    ensure!(
        conflict(session),
        "This session has no active-writer conflict; reconnect normally"
    );
    ensure!(
        app.is_host_mode() && app.manager.store.host_sessions()?.contains(&session.id),
        "Takeover is only available for imported local host sessions"
    );
    let profile = app
        .codex_home
        .clone()
        .context("Takeover requires an explicitly shared Codex profile")?;
    let (_, endpoint) = app.runtime_rpc().await?;
    ensure!(
        session.endpoint == endpoint,
        "Session does not belong to this host runtime"
    );
    let control = profile.join("app-server-control/app-server-control.sock");
    let owned = endpoint
        .strip_prefix("unix://")
        .context("Takeover requires a local managed runtime")?;
    ensure!(
        tokio::fs::canonicalize(&control).await? != tokio::fs::canonicalize(owned).await?,
        "Refusing to stop Demodex's own runtime"
    );
    let (daemon, record) = identity(&profile).await?;
    let stream = tokio::net::UnixStream::connect(&control)
        .await
        .context("Cannot reach the shared Codex CLI server")?;
    let peer = stream.peer_cred()?;
    ensure!(
        peer.pid() == Some(record.pid),
        "CLI socket belongs to another process; nothing was stopped"
    );
    #[cfg(target_os = "linux")]
    ensure!(
        peer.uid() == rustix::process::geteuid().as_raw(),
        "CLI daemon belongs to another user"
    );
    let pinned = PinnedDaemon::open(&record).await?;
    let (rpc, _events) = Rpc::connect_unix(stream).await?;
    let mut threads = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let page = rpc
            .call("thread/loaded/list", json!({"cursor":cursor,"limit":100}))
            .await?;
        for thread in page["data"]
            .as_array()
            .context("Invalid loaded thread list")?
        {
            threads.push(
                thread
                    .as_str()
                    .context("Invalid loaded thread ID")?
                    .to_owned(),
            );
        }
        cursor = match &page["nextCursor"] {
            Value::Null => None,
            Value::String(value) => Some(value.clone()),
            _ => anyhow::bail!("Invalid loaded thread cursor"),
        };
        if cursor.is_none() {
            break;
        }
    }
    ensure!(cursor.is_none(), "Too many CLI threads to verify takeover");
    threads.sort();
    threads.dedup();
    ensure!(
        threads
            .iter()
            .any(|id| Some(id) == session.thread_id.as_ref()),
        "The shared CLI server does not own this thread. Stop its original Codex controller and reconnect."
    );
    let live = app.manager.live.lock().await;
    ensure!(
        !live
            .values()
            .any(|session| threads.contains(&session.thread)),
        "The CLI server also owns a connected Demodex session; refusing to stop it"
    );
    drop(live);
    ensure!(
        identity(&profile).await?.0 == daemon,
        "CLI server changed; refresh before takeover"
    );
    Ok((pinned, daemon, threads))
}

impl Orchestrator {
    pub(crate) async fn takeover_view(&self, session: &Session) -> Value {
        if !conflict(session) {
            return Value::Null;
        }
        match tokio::time::timeout(Duration::from_secs(5), inspect(self, session)).await {
            Ok(Ok((_, daemon, threads))) => json!({"daemon":daemon,"threads":threads}),
            Ok(Err(error)) => json!({"error":format!("{error:#}")}),
            Err(_) => {
                json!({"error":"Checking the Codex CLI server timed out. Reconnect or refresh to check again."})
            }
        }
    }
}

pub(crate) async fn takeover(
    app: &Orchestrator,
    id: &str,
    expected_daemon: &str,
    expected_threads: &[String],
) -> Result<()> {
    let session = app.manager.store.get(id)?;
    let (pinned, daemon, threads) =
        tokio::time::timeout(Duration::from_secs(10), inspect(app, &session))
            .await
            .context("Checking the CLI server timed out; nothing was stopped")??;
    ensure!(
        daemon == expected_daemon && threads == expected_threads,
        "The CLI server or its loaded sessions changed. Refresh and review takeover again; nothing was stopped."
    );
    pinned.stop().await
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn child() -> tokio::process::Child {
        tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn record(child: &tokio::process::Child) -> DaemonRecord {
        let pid = child.id().unwrap() as i32;
        DaemonRecord {
            pid,
            process_identity: ProcessIdentity {
                boot_id: tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                    .await
                    .unwrap()
                    .trim()
                    .into(),
                start_ticks: start_ticks(
                    &tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .await
                        .unwrap(),
                )
                .unwrap(),
            },
        }
    }

    async fn publish(profile: &Path, record: &DaemonRecord) {
        tokio::fs::create_dir_all(profile.join("app-server-daemon"))
            .await
            .unwrap();
        tokio::fs::write(profile.join("app-server-daemon/daemon.pid"), json!({
            "pid":record.pid,
            "processIdentity":{"bootId":record.process_identity.boot_id,"startTicks":record.process_identity.start_ticks}
        }).to_string()).await.unwrap();
    }

    #[tokio::test]
    async fn replacement_after_validation_is_never_signalled() {
        // The old stop command resolved CODEX_HOME again here and would have
        // stopped replacement. A pinned handle must keep targeting reviewed.
        let profile = tempfile::tempdir().unwrap();
        let mut reviewed = child();
        publish(profile.path(), &record(&reviewed).await).await;
        let (_, reviewed_record) = identity(profile.path()).await.unwrap();
        let pinned = PinnedDaemon::open(&reviewed_record).await.unwrap();
        let mut replacement = child();
        publish(profile.path(), &record(&replacement).await).await;
        pinned.stop().await.unwrap();
        reviewed.wait().await.unwrap();
        assert!(replacement.try_wait().unwrap().is_none());
        assert_eq!(
            identity(profile.path()).await.unwrap().1.pid as u32,
            replacement.id().unwrap()
        );
        replacement.kill().await.unwrap();
    }

    #[tokio::test]
    async fn exited_reviewed_process_never_falls_back_to_replacement() {
        let profile = tempfile::tempdir().unwrap();
        let mut reviewed = child();
        let reviewed_record = record(&reviewed).await;
        let pinned = PinnedDaemon::open(&reviewed_record).await.unwrap();
        reviewed.kill().await.unwrap();
        let mut replacement = child();
        publish(profile.path(), &record(&replacement).await).await;
        assert!(pinned.stop().await.is_err());
        assert!(replacement.try_wait().unwrap().is_none());
        replacement.kill().await.unwrap();
    }

    #[tokio::test]
    async fn stale_process_identity_is_rejected_without_signalling() {
        let mut process = child();
        let mut saved = record(&process).await;
        saved.process_identity.start_ticks += 1;
        assert!(PinnedDaemon::open(&saved).await.is_err());
        saved = record(&process).await;
        saved.process_identity.boot_id = "another-boot".into();
        assert!(PinnedDaemon::open(&saved).await.is_err());
        assert!(process.try_wait().unwrap().is_none());
        process.kill().await.unwrap();
    }
}
