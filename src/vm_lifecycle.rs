//! Durable VM attachment cleanup. Callers serialize lifecycle and session settings.
use crate::{store::{Store, Target}, targets::Selection};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub(crate) fn environment_users(&self, id: &str) -> Result<Vec<String>> {
        self.environment(id)?;
        for session in self.list()? {
            self.ensure_target_selection(&session.id)?;
        }
        self.target_users(&format!("vm-{id}"))
    }

    pub(crate) fn detach_environment_selections(&self, id: &str) -> Result<()> {
        let users = self.environment_users(id)?;
        let target = format!("vm-{id}");
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        for session in users {
            let (selection, targets): (String, String) = tx.query_row(
                "SELECT selection,targets FROM session_targets JOIN sessions ON sessions.id=session_id WHERE session_id=?1",
                [&session], |r| Ok((r.get(0)?,r.get(1)?)))?;
            let (selection, targets) = remove_selection(&target, &selection, &targets)?;
            tx.execute("UPDATE session_targets SET selection=?2,pending=1 WHERE session_id=?1", params![session,selection])?;
            tx.execute("UPDATE sessions SET targets=?2 WHERE id=?1", params![session,targets])?;
            let staged: Option<(String,String)> = tx.query_row(
                "SELECT selection,targets FROM staged_session_targets WHERE session_id=?1",
                [&session], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((selection, targets)) = staged {
                let (selection, targets) = remove_selection(&target, &selection, &targets)?;
                tx.execute("UPDATE staged_session_targets SET selection=?2,targets=?3 WHERE session_id=?1", params![session,selection,targets])?;
            }
        }
        // Legacy VM creation used this binding to identify runtime ownership.
        // Preserve ownership without letting future migration restore the VM.
        tx.execute("INSERT OR IGNORE INTO runtime_sessions SELECT session_id FROM session_environment WHERE environment_id=?1", [id])?;
        tx.execute("DELETE FROM session_environment WHERE environment_id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn delete_environment_record(&self, id: &str) -> Result<()> {
        ensure!(self.environment_users(id)?.is_empty(), "VM still has attached executors");
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO runtime_sessions SELECT session_id FROM session_environment WHERE environment_id=?1", [id])?;
        tx.execute("DELETE FROM session_environment WHERE environment_id=?1", [id])?;
        ensure!(tx.execute("DELETE FROM environments WHERE id=?1", [id])? == 1, "VM not found");
        tx.commit()?;
        Ok(())
    }
}

fn remove_selection(id: &str, selection: &str, targets: &str) -> Result<(String,String)> {
    let selection: Vec<Selection> = serde_json::from_str(selection)?;
    let targets: Vec<Target> = serde_json::from_str(targets)?;
    ensure!(selection.len() == targets.len(), "Cannot safely detach inconsistent executor selections");
    let (selection, targets): (Vec<_>,Vec<_>) = selection.into_iter().zip(targets)
        .filter(|(selection,_)| selection.id != id).unzip();
    Ok((serde_json::to_string(&selection).context("Encode selection")?, serde_json::to_string(&targets)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{manager::Manager, orchestrator::Orchestrator};

    #[tokio::test]
    async fn live_detach_refuses_work_and_requires_unsubscribe_confirmation() -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::{Value, json};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        use tokio_tungstenite::tungstenite::Message;
        let mode = Arc::new(AtomicUsize::new(1));
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}",listener.local_addr()?);
        let state = mode.clone();
        let recorded = calls.clone();
        let server = tokio::spawn(async move {
            let (stream,_) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(raw))) = ws.next().await {
                let req: Value = serde_json::from_str(&raw).unwrap();
                let method = req["method"].as_str().unwrap_or("");
                recorded.lock().await.push(method.to_owned());
                if req.get("id").is_none() { continue; }
                let mode = state.load(Ordering::SeqCst);
                let result = match method {
                    "initialize" | "environment/add" => json!({}),
                    "config/read" => json!({"config":{}}),
                    "thread/start" | "thread/resume" => json!({"thread":{"id":"thread","turns":[]}}),
                    "thread/read" => json!({"thread":{"status":{"type":if mode == 1 {"active"}else{"idle"}}}}),
                    "thread/queue/list" => json!({"data":if mode == 2 {json!([{}])}else{json!([])},"nextCursor":null}),
                    "thread/goal/get" => json!({"goal":if mode == 3 {json!({"status":"active"})}else{Value::Null}}),
                    "thread/backgroundTerminals/list" => json!({"data":if mode == 4 {json!([{"processId":"p","itemId":"i","command":"sleep","cwd":"/workspace"}])}else{json!([])},"nextCursor":null}),
                    "thread/unsubscribe" => json!({"status":if mode == 5 {"unknown"}else{"unsubscribed"}}),
                    other => panic!("Unexpected method {other}"),
                };
                ws.send(Message::Text(json!({"id":req["id"],"result":result}).to_string().into())).await.unwrap();
            }
        });
        let root = tempfile::tempdir()?;
        let store = Store::open(&root.path().join("db"))?;
        let vm = store.environment_create("live",512,1,false)?;
        let session = store.create("live",&endpoint,&[],None)?;
        let manager = Manager::new(store);
        manager.connect(&session.id).await?;
        manager.store.save_target_selection(&session.id,
            &[Selection {id:format!("vm-{}",vm.id),cwd:"/workspace".into()}],
            &[Target {id:"vm-generation".into(),url:"ws://localhost:2".into(),cwd:"/workspace".into()}])?;
        let owner = Orchestrator::new(manager.clone(),root.path().into(),None,None,None);
        for blocked in 1..=5 {
            mode.store(blocked,Ordering::SeqCst);
            assert!(owner.detach_environment_executors(&vm.id).await.is_err(), "mode {blocked}");
            assert_eq!(manager.store.environment_users(&vm.id)?, vec![session.id.clone()]);
            assert!(manager.runtime(&session.id).await.is_ok());
        }
        mode.store(0,Ordering::SeqCst);
        owner.detach_environment_executors(&vm.id).await?;
        assert!(manager.store.environment_users(&vm.id)?.is_empty());
        assert!(manager.runtime(&session.id).await.is_err());
        let calls = calls.lock().await;
        assert!(!calls.iter().any(|call| call == "turn/start" || call == "turn/interrupt"));
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn deletion_requires_detach_and_preserves_other_targets_and_history() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Store::open(&root.path().join("db"))?;
        let vm = store.environment_create("test", 512, 1, false)?;
        let session = store.create("conversation", "ws://localhost:1", &[], None)?;
        store.bind_environment(&session.id, &vm.id)?;
        let selection = vec![Selection { id:format!("vm-{}", vm.id),cwd:"/workspace".into() },Selection {id:"host".into(),cwd:"/other".into()}];
        let targets = vec![Target {id:"vm-old-generation".into(),url:"ws://localhost:2".into(),cwd:"/workspace".into()},Target {id:"host-generation".into(),url:"ws://localhost:3".into(),cwd:"/other".into()}];
        store.save_target_selection(&session.id, &selection, &targets)?;
        store.targets_applied(&session.id)?;
        store.stage_target_selection(&session.id, &selection[1..], &targets[1..])?;
        store.event(&session.id, &serde_json::json!({"method":"test/history"}))?;
        let manager = Manager::new(store);
        let owner = Orchestrator::new(manager.clone(), root.path().into(), None, None, None);
        let directory = root.path().join("environments").join(&vm.id);
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join("disk.qcow2"), "private guest data")?;
        let base = root.path().join("base.qcow2");
        std::fs::write(&base, "shared base")?;
        assert!(owner.delete_environment(&vm.id).await.is_err());
        assert!(directory.exists());
        owner.detach_environment_executors(&vm.id).await?;
        assert!(manager.store.environment_users(&vm.id)?.is_empty());
        assert_eq!(manager.store.get(&session.id)?.targets, targets[1..]);
        assert_eq!(manager.store.staged_targets(&session.id)?, Some(targets[1..].to_vec()));
        assert!(manager.store.targets_pending(&session.id)?);
        assert!(manager.store.uses_runtime(&session.id)?);
        assert!(manager.store.session_environment(&session.id)?.is_none());
        owner.delete_environment(&vm.id).await?;
        assert!(!directory.exists());
        assert!(base.exists());
        assert!(manager.store.environment(&vm.id).is_err());
        assert!(manager.store.get(&session.id).is_ok());
        assert!(!manager.store.events(&session.id, 0)?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn staged_only_attachment_blocks_delete_and_running_vm_is_rejected() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Store::open(&root.path().join("db"))?;
        let vm = store.environment_create("test",512,1,false)?;
        let session = store.create("staged", "ws://localhost:1", &[], None)?;
        store.ensure_target_selection(&session.id)?;
        store.stage_target_selection(&session.id, &[Selection {id:format!("vm-{}",vm.id),cwd:"/workspace".into()}],
            &[Target {id:"vm-generation".into(),url:"ws://localhost:2".into(),cwd:"/workspace".into()}])?;
        let manager = Manager::new(store);
        let owner = Orchestrator::new(manager.clone(),root.path().into(),None,None,None);
        assert!(owner.delete_environment(&vm.id).await.is_err());
        owner.detach_environment_executors(&vm.id).await?;
        assert!(manager.store.staged_targets(&session.id)?.unwrap().is_empty());
        manager.store.environment_status(&vm.id,"running",None)?;
        assert!(owner.delete_environment(&vm.id).await.is_err());
        manager.store.environment_status(&vm.id,"stopped",None)?;
        // An unprovisioned VM has no directory and can still be deleted.
        owner.delete_environment(&vm.id).await?;
        Ok(())
    }

    #[tokio::test]
    async fn delete_rejects_symlink_to_unrelated_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::write(outside.path().join("keep"),"keep")?;
        let store = Store::open(&root.path().join("db"))?;
        let vm = store.environment_create("test",512,1,false)?;
        std::fs::create_dir(root.path().join("environments"))?;
        std::os::unix::fs::symlink(outside.path(),root.path().join("environments").join(&vm.id))?;
        let manager = Manager::new(store);
        let owner = Orchestrator::new(manager.clone(),root.path().into(),None,None,None);
        assert!(owner.delete_environment(&vm.id).await.is_err());
        assert!(outside.path().join("keep").exists());
        assert!(manager.store.environment(&vm.id).is_ok());
        Ok(())
    }
}
