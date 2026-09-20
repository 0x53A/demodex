use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, sync::Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub id: String,
    pub url: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub name: String,
    pub memory_mib: u32,
    pub cpus: u16,
    pub internet: bool,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub thread_id: Option<String>,
    pub targets: Vec<Target>,
    pub status: String,
    pub error: Option<String>,
    pub sandbox: Option<Sandbox>,
    pub effective_sandbox: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox { ReadOnly, WorkspaceWrite, DangerFullAccess }

#[derive(Debug, Serialize)]
pub struct Event {
    pub seq: i64,
    pub at: String,
    pub message: Value,
}

#[derive(Debug, Serialize)]
pub struct Pending {
    pub key: String,
    pub method: String,
    pub params: Value,
    pub state: String,
}

pub struct Store(Mutex<Connection>);

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, endpoint TEXT NOT NULL,
               thread_id TEXT, targets TEXT NOT NULL, status TEXT NOT NULL,
               error TEXT, created INTEGER NOT NULL DEFAULT (unixepoch()));
             CREATE TABLE IF NOT EXISTS events (
               seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id),
               at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')), message TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS events_session ON events(session_id,seq);
             CREATE TABLE IF NOT EXISTS pending (
               key TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
               generation TEXT NOT NULL, rpc_id TEXT NOT NULL, method TEXT NOT NULL,
               params TEXT NOT NULL, state TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS environments (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, memory_mib INTEGER NOT NULL,
               cpus INTEGER NOT NULL, internet INTEGER NOT NULL, status TEXT NOT NULL, error TEXT);
             CREATE TABLE IF NOT EXISTS session_environment (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id),
               environment_id TEXT NOT NULL REFERENCES environments(id));
             CREATE TABLE IF NOT EXISTS host_sessions (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id));
             CREATE TABLE IF NOT EXISTS session_settings (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), sandbox TEXT, effective_sandbox TEXT);
             CREATE TABLE IF NOT EXISTS command_receipts (
               id TEXT PRIMARY KEY, operation TEXT NOT NULL, response TEXT);
             UPDATE environments SET status='stopped', error=NULL;
             UPDATE sessions SET status='disconnected', error=NULL;
             UPDATE pending SET state='unavailable' WHERE state IN ('pending','responding','delivered');",
        )?;
        Ok(Self(Mutex::new(connection)))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))
    }

    pub fn receipt(&self, id: &str) -> Result<Option<(String, Option<String>)>> {
        Ok(self.lock()?.query_row("SELECT operation,response FROM command_receipts WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    }

    pub fn begin_command(&self, id: &str, operation: &str) -> Result<()> {
        self.lock()?.execute("INSERT INTO command_receipts(id,operation) VALUES(?1,?2)", params![id, operation])?;
        Ok(())
    }

    pub fn finish_command(&self, id: &str, response: &str) -> Result<()> {
        self.lock()?.execute("UPDATE command_receipts SET response=?2 WHERE id=?1", params![id, response])?;
        Ok(())
    }

    pub fn environment_create(&self,name:&str,memory_mib:u32,cpus:u16,internet:bool)->Result<Environment> {
        let id=uuid::Uuid::new_v4().to_string();
        self.lock()?.execute("INSERT INTO environments VALUES(?1,?2,?3,?4,?5,'stopped',NULL)",params![id,name,memory_mib,cpus,internet])?;
        self.environment(&id)
    }
    pub fn environments(&self)->Result<Vec<Environment>> {
        let db=self.lock()?;
        let mut query=db.prepare("SELECT id,name,memory_mib,cpus,internet,status,error FROM environments ORDER BY rowid")?;
        Ok(query.query_map([],|r|Ok(Environment{id:r.get(0)?,name:r.get(1)?,memory_mib:r.get(2)?,cpus:r.get(3)?,internet:r.get(4)?,status:r.get(5)?,error:r.get(6)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn environment(&self,id:&str)->Result<Environment> {
        self.environments()?.into_iter().find(|e|e.id==id).context("environment not found")
    }
    pub fn environment_status(&self,id:&str,status:&str,error:Option<&str>)->Result<()> {
        self.lock()?.execute("UPDATE environments SET status=?2,error=?3 WHERE id=?1",params![id,status,error])?;Ok(())
    }
    pub fn bind_environment(&self,session:&str,environment:&str)->Result<()> {
        self.lock()?.execute("INSERT INTO session_environment VALUES(?1,?2)",params![session,environment])?;Ok(())
    }
    pub fn session_environment(&self,session:&str)->Result<Option<String>> {
        Ok(self.lock()?.query_row("SELECT environment_id FROM session_environment WHERE session_id=?1",[session],|r|r.get(0)).optional()?)
    }
    pub fn environment_sessions(&self,environment:&str)->Result<Vec<String>> {
        let db=self.lock()?;let mut q=db.prepare("SELECT session_id FROM session_environment WHERE environment_id=?1")?;
        Ok(q.query_map([environment],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn retarget(&self,session:&str,endpoint:&str,targets:&[Target])->Result<()> {
        self.lock()?.execute("UPDATE sessions SET endpoint=?2,targets=?3 WHERE id=?1",params![session,endpoint,serde_json::to_string(targets)?])?;Ok(())
    }
    pub fn bind_host(&self, session: &str) -> Result<()> {
        self.lock()?.execute("INSERT INTO host_sessions VALUES(?1)", [session])?;
        Ok(())
    }
    pub fn host_sessions(&self) -> Result<Vec<String>> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT session_id FROM host_sessions")?;
        Ok(query.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create(
        &self,
        name: &str,
        endpoint: &str,
        targets: &[Target],
        thread_id: Option<&str>,
    ) -> Result<Session> {
        let id = uuid::Uuid::new_v4().to_string();
        self.lock()?.execute("INSERT INTO sessions(id,name,endpoint,targets,thread_id,status) VALUES(?1,?2,?3,?4,?5,'disconnected')",
            params![id, name, endpoint, serde_json::to_string(targets)?, thread_id])?;
        self.get(&id)
    }

    pub fn sandbox(&self, id: &str, sandbox: Option<Sandbox>) -> Result<()> {
        self.lock()?.execute("INSERT INTO session_settings(session_id,sandbox) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET sandbox=excluded.sandbox,effective_sandbox=NULL",
            params![id,sandbox.map(|s|serde_json::to_string(&s)).transpose()?])?;
        Ok(())
    }
    pub fn effective_sandbox(&self, id: &str, sandbox: &Value) -> Result<()> {
        self.lock()?.execute("INSERT INTO session_settings(session_id,effective_sandbox) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET effective_sandbox=excluded.effective_sandbox",
            params![id, sandbox.to_string()])?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT id,name,endpoint,thread_id,targets,status,error,sandbox,effective_sandbox FROM sessions LEFT JOIN session_settings ON sessions.id=session_settings.session_id ORDER BY created,id")?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        rows.map(|row| {
            let (id, name, endpoint, thread_id, targets, status, error, sandbox, effective_sandbox) = row?;
            Ok(Session {
                id,
                name,
                endpoint,
                thread_id,
                targets: serde_json::from_str(&targets)?,
                status,
                error,
                sandbox: sandbox.as_deref().map(serde_json::from_str).transpose()?,
                effective_sandbox: effective_sandbox.as_deref().map(serde_json::from_str).transpose()?,
            })
        })
        .collect()
    }

    pub fn get(&self, id: &str) -> Result<Session> {
        self.list()?
            .into_iter()
            .find(|s| s.id == id)
            .context("session not found")
    }

    pub fn thread(&self, id: &str, thread: &str) -> Result<()> {
        self.lock()?.execute(
            "UPDATE sessions SET thread_id=?2 WHERE id=?1",
            params![id, thread],
        )?;
        Ok(())
    }

    pub fn status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.lock()?.execute(
            "UPDATE sessions SET status=?2,error=?3 WHERE id=?1",
            params![id, status, error],
        )?;
        Ok(())
    }

    pub fn event(&self, id: &str, message: &Value) -> Result<i64> {
        let db = self.lock()?;
        db.execute(
            "INSERT INTO events(session_id,message) VALUES(?1,?2)",
            params![id, serde_json::to_string(message)?],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn events(&self, id: &str, after: i64) -> Result<Vec<Event>> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT seq,at,message FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT 500")?;
        let rows = query.query_map(params![id, after], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
        })?;
        rows.map(|r| {
            let (seq, at, message) = r?;
            Ok(Event {
                seq,
                at,
                message: serde_json::from_str(&message)?,
            })
        })
        .collect()
    }

    pub fn pending(&self, id: &str) -> Result<Vec<Pending>> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT key,method,params,state FROM pending WHERE session_id=?1 AND state != 'resolved' ORDER BY rowid")?;
        let rows = query.query_map([id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?))
        })?;
        rows.map(|r| {
            let (key, method, p, state) = r?;
            Ok(Pending {
                key,
                method,
                params: serde_json::from_str(&p)?,
                state,
            })
        })
        .collect()
    }

    pub fn request(&self, id: &str, generation: &str, message: &Value) -> Result<()> {
        self.lock()?.execute("INSERT OR IGNORE INTO pending(key,session_id,generation,rpc_id,method,params,state) VALUES(?1,?2,?3,?4,?5,?6,'pending')",
            params![format!("{generation}:{}",message["id"]),id,generation,message["id"].to_string(),message["method"].as_str().unwrap_or("unknown"),message["params"].to_string()])?;
        Ok(())
    }

    pub fn claim(&self, id: &str, key: &str, generation: &str) -> Result<Value> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let rpc_id: String = tx.query_row("SELECT rpc_id FROM pending WHERE key=?1 AND session_id=?2 AND generation=?3 AND state='pending'", params![key,id,generation], |r| r.get(0)).optional()?.context("request is no longer answerable on this connection")?;
        tx.execute("UPDATE pending SET state='responding' WHERE key=?1", [key])?;
        tx.commit()?;
        Ok(serde_json::from_str(&rpc_id)?)
    }

    pub fn request_state(&self, key: &str, state: &str) -> Result<()> {
        self.lock()?.execute(
            "UPDATE pending SET state=?2 WHERE key=?1 AND state != 'resolved'",
            params![key, state],
        )?;
        Ok(())
    }

    pub fn resolve(&self, generation: &str, rpc_id: &Value) -> Result<()> {
        self.lock()?.execute(
            "UPDATE pending SET state='resolved' WHERE generation=?1 AND rpc_id=?2",
            params![generation, rpc_id.to_string()],
        )?;
        Ok(())
    }

    pub fn disconnected(&self, id: &str, generation: &str) -> Result<()> {
        self.lock()?.execute("UPDATE pending SET state='unavailable' WHERE session_id=?1 AND generation=?2 AND state!='resolved'", params![id,generation])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn approval_cannot_be_answered_twice_or_on_replacement_connection() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let session = store.create("test", "ws://localhost:1", &[], None)?;
        store.request(
            &session.id,
            "first",
            &json!({"id":7,"method":"approval","params":{}}),
        )?;
        assert!(store.claim(&session.id, "first:7", "replacement").is_err());
        assert_eq!(store.claim(&session.id, "first:7", "first")?, json!(7));
        assert!(store.claim(&session.id, "first:7", "first").is_err());
        Ok(())
    }
    #[test]
    fn restart_keeps_identity_and_history_but_not_live_approvals() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        let id;
        {
            let store = Store::open(&path)?;
            let s = store.create("test", "ws://localhost:1", &[], None)?;
            id = s.id;
            store.thread(&id, "thread-original")?;
            store.event(&id, &json!({"hello":"world"}))?;
            store.request(&id, "old", &json!({"id":1,"method":"approval","params":{}}))?;
        }
        let store = Store::open(&path)?;
        assert_eq!(
            store.get(&id)?.thread_id.as_deref(),
            Some("thread-original")
        );
        assert_eq!(store.events(&id, 0)?.len(), 1);
        assert_eq!(store.pending(&id)?[0].state, "unavailable");
        assert!(store.claim(&id, "old:1", "old").is_err());
        Ok(())
    }
}
