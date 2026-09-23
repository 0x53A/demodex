//! Stable user selections are distinct from ephemeral executor identities.
use crate::store::{Store, Target};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub use demodex_protocol::Selection;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisteredTarget {
    pub id: String,
    pub name: String,
    pub url: String,
    pub cwd: String,
}

pub fn validate_selection(selection: &[Selection]) -> Result<()> {
    ensure!(selection.len() <= 32, "Select at most 32 targets");
    let mut ids = std::collections::HashSet::new();
    for target in selection {
        ensure!(
            !target.id.is_empty() && ids.insert(&target.id),
            "Target IDs must be unique"
        );
        validate_cwd(&target.cwd)?;
    }
    Ok(())
}

pub fn validate_cwd(cwd: &str) -> Result<()> {
    ensure!(
        cwd.starts_with('/') && !cwd.contains('\0'),
        "Working directory must be an absolute path"
    );
    Ok(())
}

impl Store {
    /// Idempotent migration, also used after legacy creation endpoints.
    pub fn ensure_target_selection(&self, id: &str) -> Result<()> {
        if self.target_selection(id)?.is_some() {
            return Ok(());
        }
        let session = self.get(id)?;
        let host = self.host_sessions()?.iter().any(|s| s == id);
        let vm = self.session_environment(id)?;
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let mut selection = Vec::new();
        if host {
            selection.push(Selection {
                id: "host".into(),
                cwd: session
                    .targets
                    .first()
                    .map(|t| t.cwd.clone())
                    .unwrap_or_else(|| "/workspace".into()),
            });
        } else if let Some(vm) = vm {
            selection.push(Selection {
                id: format!("vm-{vm}"),
                cwd: session
                    .targets
                    .first()
                    .map(|t| t.cwd.clone())
                    .unwrap_or_else(|| "/workspace".into()),
            });
        } else {
            for target in &session.targets {
                // Never merge unrelated endpoints just because a caller reused an executor ID.
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT id FROM execution_targets WHERE url=?1 AND cwd=?2 AND name=?3",
                        params![target.url, target.cwd, target.id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let key = existing.unwrap_or_else(|| format!("external-{}", uuid::Uuid::new_v4()));
                tx.execute(
                    "INSERT OR IGNORE INTO execution_targets VALUES(?1,?2,?3,?4)",
                    params![key, target.id, target.url, target.cwd],
                )?;
                selection.push(Selection {
                    id: key,
                    cwd: target.cwd.clone(),
                });
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO session_targets VALUES(?1,?2,0)",
            params![id, serde_json::to_string(&selection)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn target_selection(&self, id: &str) -> Result<Option<Vec<Selection>>> {
        let value: Option<String> = self
            .lock()?
            .query_row(
                "SELECT selection FROM session_targets WHERE session_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    pub fn targets_pending(&self, id: &str) -> Result<bool> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT pending FROM session_targets WHERE session_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn targets_applied(&self, id: &str) -> Result<()> {
        self.lock()?.execute(
            "UPDATE session_targets SET pending=0 WHERE session_id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn save_target_selection(
        &self,
        id: &str,
        selection: &[Selection],
        targets: &[Target],
    ) -> Result<()> {
        validate_selection(selection)?;
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        tx.execute("INSERT INTO session_targets VALUES(?1,?2,1) ON CONFLICT(session_id) DO UPDATE SET selection=excluded.selection,pending=1", params![id,serde_json::to_string(selection)?])?;
        tx.execute(
            "UPDATE sessions SET targets=?2 WHERE id=?1",
            params![id, serde_json::to_string(targets)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn registered_targets(&self) -> Result<Vec<RegisteredTarget>> {
        let db = self.lock()?;
        let mut q = db.prepare("SELECT id,name,url,cwd FROM execution_targets ORDER BY name,id")?;
        Ok(q.query_map([], |r| {
            Ok(RegisteredTarget {
                id: r.get(0)?,
                name: r.get(1)?,
                url: r.get(2)?,
                cwd: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn register_target(&self, name: &str, url: &str, cwd: &str) -> Result<RegisteredTarget> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 120,
            "Target name must be 1–120 characters"
        );
        validate_cwd(cwd)?;
        ensure!(
            url.starts_with("ws://") && !url.contains(['\n', '\r', '\0']),
            "Executor URL must use ws://"
        );
        let parsed: http::Uri = url.parse().context("Invalid executor URL")?;
        ensure!(
            parsed.host().is_some() && !url.contains('@'),
            "Executor URL must have a host and no credentials"
        );
        let target = RegisteredTarget {
            id: format!("external-{}", uuid::Uuid::new_v4()),
            name: name.trim().into(),
            url: url.into(),
            cwd: cwd.into(),
        };
        self.lock()?.execute(
            "INSERT INTO execution_targets VALUES(?1,?2,?3,?4)",
            params![target.id, target.name, target.url, target.cwd],
        )?;
        Ok(target)
    }

    pub fn ssh_targets(&self) -> Result<Vec<(String, crate::ssh::Config)>> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT id,config FROM ssh_targets ORDER BY rowid")?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, config)| Ok((id, serde_json::from_str(&config)?)))
            .collect()
    }
    pub fn register_ssh_target(&self, config: &crate::ssh::Config) -> Result<String> {
        config.validate()?;
        let id = format!("ssh-{}", uuid::Uuid::new_v4());
        self.lock()?.execute(
            "INSERT INTO ssh_targets VALUES(?1,?2)",
            params![id, serde_json::to_string(config)?],
        )?;
        Ok(id)
    }

    pub fn target_users(&self, id: &str) -> Result<Vec<String>> {
        let db = self.lock()?;
        let mut q = db.prepare("SELECT session_id,selection FROM session_targets")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .filter_map(
                |(session, value)| match serde_json::from_str::<Vec<Selection>>(&value) {
                    Ok(selection) if selection.iter().any(|t| t.id == id) => Some(Ok(session)),
                    Ok(_) => None,
                    Err(error) => Some(Err(error.into())),
                },
            )
            .collect()
    }

    pub fn forget_target(&self, id: &str) -> Result<()> {
        ensure!(
            self.target_users(id)?.is_empty(),
            "Detach this target from every session before forgetting it"
        );
        let db = self.lock()?;
        let removed = db.execute("DELETE FROM execution_targets WHERE id=?1", [id])?
            + db.execute("DELETE FROM ssh_targets WHERE id=?1", [id])?;
        ensure!(
            removed == 1,
            "Only registered external or SSH targets can be forgotten"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        manager::{Live, Manager},
        orchestrator::Orchestrator,
        rpc::Rpc,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn selected_session_keeps_runtime_ownership_separate_from_targets() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("db");
        let store = Store::open(&path)?;
        let selection = vec![Selection {
            id: "ssh-build".into(),
            cwd: "/project".into(),
        }];
        let session = store.create_selected(
            "SSH only",
            "unix:///runtime/app.sock",
            &[],
            &selection,
            Some(crate::store::Sandbox::DangerFullAccess),
        )?;
        assert!(store.uses_runtime(&session.id)?);
        assert!(store.host_sessions()?.is_empty());
        assert!(store.session_environment(&session.id)?.is_none());
        assert!(!store.targets_pending(&session.id)?);
        drop(store);
        let store = Store::open(&path)?;
        store.ensure_target_selection(&session.id)?;
        assert_eq!(store.target_selection(&session.id)?, Some(selection));
        assert!(store.uses_runtime(&session.id)?);
        assert!(matches!(
            store.get(&session.id)?.sandbox,
            Some(crate::store::Sandbox::DangerFullAccess)
        ));
        Ok(())
    }

    #[test]
    fn migration_preserves_history_and_shared_membership() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("db");
        let store = Store::open(&path)?;
        let vm = store.environment_create("Shared build VM", 1024, 1, false)?;
        let a = store.create("First", "ws://127.0.0.1:1", &[], None)?;
        store.bind_environment(&a.id, &vm.id)?;
        store.ensure_target_selection(&a.id)?;
        store.event(&a.id, &json!({"history":"keep"}))?;
        let host = Selection {
            id: "host".into(),
            cwd: "/projects".into(),
        };
        let b = store.create_attached("Second", "ws://127.0.0.1:1", &[], None, Some(&host))?;
        assert_eq!(store.target_selection(&b.id)?, Some(vec![host]));
        store.ensure_target_selection(&b.id)?;
        let selection = vec![Selection {
            id: format!("vm-{}", vm.id),
            cwd: "/workspace/second".into(),
        }];
        store.save_target_selection(&b.id, &selection, &[])?;
        assert_eq!(store.target_users(&selection[0].id)?.len(), 2);
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(store.target_selection(&b.id)?, Some(selection.clone()));
        assert_eq!(store.target_users(&selection[0].id)?.len(), 2);
        assert!(store.targets_pending(&b.id)?);
        assert!(
            store
                .events(&a.id, 0)?
                .iter()
                .any(|e| e.message["history"] == "keep")
        );
        assert!(
            store
                .register_target("bad", "ws://user:secret@localhost:1", "/tmp")
                .is_err()
        );
        assert!(validate_selection(&[selection[0].clone(), selection[0].clone()]).is_err());
        let aliases = [
            Target {
                id: "alias-a".into(),
                url: "ws://localhost:9".into(),
                cwd: "/work".into(),
            },
            Target {
                id: "alias-b".into(),
                url: "ws://localhost:9".into(),
                cwd: "/work".into(),
            },
        ];
        let external = store.create("Aliases", "ws://localhost:1", &aliases, None)?;
        store.ensure_target_selection(&external.id)?;
        let migrated = store.target_selection(&external.id)?.unwrap();
        assert_eq!(migrated.len(), 2);
        validate_selection(&migrated)?;
        Ok(())
    }

    #[tokio::test]
    async fn stopping_a_shared_vm_disconnects_all_users_but_keeps_selection() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Store::open(&root.path().join("db"))?;
        let vm = store.environment_create("Shared", 1024, 1, false)?;
        let selection = vec![Selection {
            id: format!("vm-{}", vm.id),
            cwd: "/workspace".into(),
        }];
        let mut users = Vec::new();
        for name in ["Owner", "Guest"] {
            let session = store.create(name, "ws://localhost:1", &[], None)?;
            // The guest originally belonged to the host runtime, not this VM.
            if name == "Owner" {
                store.bind_environment(&session.id, &vm.id)?;
            } else {
                store.bind_host(&session.id)?;
            }
            store.save_target_selection(&session.id, &selection, &[])?;
            store.status(&session.id, "idle", None)?;
            users.push(session.id);
        }
        let other = store.create("Other", "ws://localhost:1", &[], None)?;
        store.ensure_target_selection(&other.id)?;
        store.status(&other.id, "idle", None)?;
        let manager = Manager::new(store);
        let orchestrator = Orchestrator::new(manager.clone(), root.path().into(), None, None, None);
        orchestrator.stop(&vm.id).await?;
        for user in users {
            assert_eq!(manager.store.get(&user)?.status, "disconnected");
            assert_eq!(
                manager.store.target_selection(&user)?,
                Some(selection.clone())
            );
        }
        assert_eq!(manager.store.get(&other.id)?.status, "idle");
        assert_eq!(manager.store.environment(&vm.id)?.status, "stopped");
        Ok(())
    }

    #[tokio::test]
    async fn paused_selection_is_bound_on_next_turn_and_rejects_active_work() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let state = Arc::new(Mutex::new(
            json!({"active":false,"queue":[],"goal":null,"fail":false}),
        ));
        let log = calls.clone();
        let remote = state.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let req: Value = serde_json::from_str(&text).unwrap();
                if req.get("id").is_none() {
                    continue;
                }
                log.lock().await.push(req.clone());
                let state = remote.lock().await;
                let result = match req["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "thread/read" => {
                        json!({"thread":{"status":{"type":if state["active"]==true{"active"}else{"idle"}}}})
                    }
                    "thread/queue/list" => json!({"data":state["queue"],"nextCursor":null}),
                    "thread/goal/get" => json!({"goal":state["goal"]}),
                    "environment/add" if state["fail"] == true => {
                        ws.send(Message::Text(json!({"id":req["id"],"error":{"code":-1,"message":"executor unavailable"}}).to_string().into())).await.unwrap();
                        continue;
                    }
                    "environment/add" => json!({}),
                    "turn/start" => json!({"turn":{"id":"new-turn"}}),
                    other => panic!("unexpected {other}"),
                };
                ws.send(Message::Text(
                    json!({"id":req["id"],"result":result}).to_string().into(),
                ))
                .await
                .unwrap();
            }
        });
        let root = tempfile::tempdir()?;
        let store = Store::open(&root.path().join("db"))?;
        let session = store.create("Fixture", &endpoint, &[], None)?;
        store.thread(&session.id, "thread")?;
        store.ensure_target_selection(&session.id)?;
        let a = store.register_target("A", "ws://localhost:2", "/a")?;
        let b = store.register_target("B", "ws://localhost:3", "/b")?;
        let manager = Manager::new(store);
        let (rpc, _events) = Rpc::connect(&endpoint).await?;
        manager.live.lock().await.insert(
            session.id.clone(),
            Arc::new(Live {
                rpc,
                generation: "fixture".into(),
                thread: "thread".into(),
                turn: Mutex::new(None),
            }),
        );
        let orchestrator = Orchestrator::new(manager.clone(), root.path().into(), None, None, None);
        let selection = vec![
            Selection {
                id: a.id.clone(),
                cwd: "/a".into(),
            },
            Selection {
                id: b.id.clone(),
                cwd: "/b".into(),
            },
        ];
        for (key, value) in [
            ("active", json!(true)),
            ("queue", json!([{"id":"queued"}])),
            ("goal", json!({"status":"active"})),
            ("fail", json!(true)),
        ] {
            state.lock().await[key] = value;
            assert!(
                orchestrator
                    .select_targets(&session.id, &selection)
                    .await
                    .is_err()
            );
            assert!(manager.store.get(&session.id)?.targets.is_empty());
            *state.lock().await = json!({"active":false,"queue":[],"goal":null,"fail":false});
        }
        manager.store.request(
            &session.id,
            "fixture",
            &json!({"id":99,"method":"item/tool/requestUserInput","params":{}}),
        )?;
        for status in ["pending", "responding", "delivered"] {
            manager.store.request_state("fixture:99", status)?;
            assert!(
                orchestrator
                    .select_targets(&session.id, &selection)
                    .await
                    .is_err()
            );
            assert_eq!(manager.store.pending(&session.id)?[0].state, status);
            assert!(manager.store.get(&session.id)?.targets.is_empty());
        }
        manager.store.resolve("fixture", &json!(99))?;
        orchestrator.select_targets(&session.id, &selection).await?;
        assert!(manager.store.targets_pending(&session.id)?);
        assert!(manager.resume_queue(&session.id).await.is_err());
        assert!(
            manager
                .change_goal(
                    &session.id,
                    crate::controls::GoalAction {
                        action: "resume".into(),
                        objective: None,
                        token_budget: None
                    }
                )
                .await
                .is_err()
        );
        assert!(manager.store.forget_target(&a.id).is_err());
        manager.prompt(&session.id, "Use both targets").await?;
        let log = calls.lock().await;
        let turn = log.iter().find(|r| r["method"] == "turn/start").unwrap();
        assert_eq!(turn["params"]["threadId"], "thread");
        assert_eq!(turn["params"]["environments"].as_array().unwrap().len(), 2);
        assert_eq!(turn["params"]["environments"][1]["cwd"], "/b");
        assert!(!manager.store.targets_pending(&session.id)?);
        server.abort();
        Ok(())
    }
}
