//! Startup recovery reads Codex's durable goal state, never a shutdown marker.
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl crate::orchestrator::Orchestrator {
    fn managed_session(&self, id: &str) -> Result<bool> {
        Ok(self.manager.store.uses_runtime(id)?
            || self.manager.store.host_sessions()?.iter().any(|s| s == id)
            || self.manager.store.session_environment(id)?.is_some())
    }

    pub(crate) async fn reconnect_active_goals(&self, stop: &CancellationToken) {
        let sessions = match self.manager.store.list() {
            Ok(sessions) => sessions,
            Err(error) => { tracing::warn!(%error, "cannot inspect saved goals after restart"); return; }
        };
        let sessions: Vec<_> = sessions.into_iter().filter(|s| !s.archived && s.thread_id.is_some()).collect();
        // Both providers share durable goal storage. Read through the native
        // runtime; start Claude only if an active Claude goal needs reconnecting.
        let needs_runtime = sessions.iter().any(|s| self.managed_session(&s.id).unwrap_or(false));
        let runtime_error = if needs_runtime && !stop.is_cancelled() {
            let _lifecycle = self.jobs.lock().await;
            let _settings = self.manager.connecting.lock().await;
            if stop.is_cancelled() { return; }
            self.start_runtime().await.err().map(|e| format!("{e:#}"))
        } else { None };
        for session in sessions {
            if stop.is_cancelled() { break; }
            if let Err(error) = self.reconnect_active_goal(&session.id, runtime_error.as_deref(), stop).await {
                let message = format!("Automatic goal reconnect failed; reconnect manually: {error:#}");
                let live = self.manager.live.lock().await;
                if !live.contains_key(&session.id) {
                    let _ = self.manager.store.status(&session.id, "disconnected", Some(&message));
                }
                drop(live);
                self.manager.changed();
                tracing::warn!(session=%session.id, %error, "startup goal reconnect failed");
            }
        }
    }

    async fn reconnect_active_goal(&self, id: &str, runtime_error: Option<&str>, stop: &CancellationToken) -> Result<()> {
        let _lifecycle = self.jobs.lock().await;
        let settings = self.manager.connecting.lock().await;
        if stop.is_cancelled() || self.manager.live.lock().await.contains_key(id) { return Ok(()); }
        let session = self.manager.store.get(id)?;
        if session.archived { return Ok(()); }
        let Some(thread) = &session.thread_id else { return Ok(()); };
        let managed = self.managed_session(id)?;
        let goal = tokio::time::timeout(Duration::from_secs(3), async {
            let params = json!({"threadId":thread});
            if managed {
                ensure!(runtime_error.is_none(), "Runtime unavailable: {}", runtime_error.unwrap_or_default());
                self.runtime_rpc().await?.0.call("thread/goal/get", params).await
            } else {
                let (rpc, _events) = crate::rpc::Rpc::connect(&session.endpoint).await?;
                let result = rpc.call("thread/goal/get", params).await;
                rpc.close();
                result
            }
        }).await.context("Saved goal lookup timed out")??;
        ensure!(goal.get("goal").is_some(), "Codex did not return saved goal state");
        if goal["goal"]["status"] != "active" || stop.is_cancelled() { return Ok(()); }
        ensure!(!self.manager.store.targets_pending(id)?, "Apply the pending execution-target change before resuming the goal");
        // One attempt per startup. No turn/start, goal/set, queue dispatch or
        // decision response: Codex owns continuation of the existing goal.
        self.connect_session_locked(id, &settings, None).await
    }
}
