use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value,json};
use std::{path::Path, sync::Mutex};

pub use demodex_protocol::Target;

pub use demodex_protocol::Environment;

pub use demodex_protocol::Session;

pub use demodex_protocol::Sandbox;

pub use demodex_protocol::Event;

pub use demodex_protocol::Pending;

pub struct Store(Mutex<Connection>);

// Keep manual ordering inside the same target/project group as the overview.
fn session_group(session: &Session) -> (String, String) {
    let context = session.presentation.context.as_ref().filter(|c| c.path.starts_with('/'));
    let environment = demodex_protocol::location::preferred(session.targets.iter().map(|t|t.id.as_str()), context.map(|c|c.environment_id.as_str())).unwrap_or("");
    let path = context.filter(|c|c.environment_id == environment).map(|c|c.path.as_str())
        .or_else(||session.targets.iter().find(|t|t.id == environment).map(|t|t.cwd.as_str())).unwrap_or("");
    (demodex_protocol::location::target_id(environment).into(), path.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("/"))
}

impl Store {
    pub fn record_uploaded_image(&self, id: &str, path: &str, target: &Target, digest: &str) -> Result<()> {
        self.0.lock().unwrap().execute("INSERT INTO uploaded_images VALUES(?1,?2,?3,?4)", params![id,path,serde_json::to_string(target)?,digest])?;
        Ok(())
    }

    pub fn uploaded_image(&self, id: &str, path: &str) -> Result<Option<(Target, String)>> {
        let row: Option<(String,String)> = self.0.lock().unwrap().query_row("SELECT target,digest FROM uploaded_images WHERE session_id=?1 AND path=?2", params![id,path], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(target,digest)| Ok((serde_json::from_str(&target)?,digest))).transpose()
    }

    pub fn runtime_features(&self) -> Result<std::collections::BTreeMap<String, bool>> {
        let connection = self.0.lock().unwrap();
        let mut statement = connection.prepare("SELECT name, enabled FROM runtime_features ORDER BY name")?;
        Ok(statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_runtime_feature(&self, name: &str, enabled: Option<bool>) -> Result<()> {
        let connection = self.0.lock().unwrap();
        if let Some(enabled) = enabled {
            connection.execute("INSERT INTO runtime_features VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET enabled=excluded.enabled", params![name, enabled])?;
        } else {
            connection.execute("DELETE FROM runtime_features WHERE name=?1", [name])?;
        }
        Ok(())
    }

    pub fn save_prompt(&self, id: &str, text: &str) -> Result<()> {
        self.0.lock().unwrap().execute("INSERT INTO session_prompt VALUES(?1,?2)", params![id, text])?;
        Ok(())
    }

    pub fn prompt(&self, id: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().query_row("SELECT text FROM session_prompt WHERE session_id=?1", [id], |row| row.get(0)).optional()?)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, endpoint TEXT NOT NULL,
               thread_id TEXT, targets TEXT NOT NULL, status TEXT NOT NULL,
               error TEXT, created INTEGER NOT NULL DEFAULT (unixepoch()));
             CREATE TABLE IF NOT EXISTS ssh_targets (id TEXT PRIMARY KEY, config TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS runtime_features (name TEXT PRIMARY KEY, enabled INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS session_ssh_targets (target_id TEXT PRIMARY KEY REFERENCES ssh_targets(id) ON DELETE CASCADE, session_id TEXT NOT NULL REFERENCES sessions(id));
             CREATE TABLE IF NOT EXISTS containers (id TEXT PRIMARY KEY, name TEXT NOT NULL, image TEXT NOT NULL, memory_mib INTEGER NOT NULL, cpus INTEGER NOT NULL, status TEXT NOT NULL, error TEXT, engine TEXT NOT NULL DEFAULT 'docker');
             CREATE TABLE IF NOT EXISTS execution_targets (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, cwd TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS session_targets (session_id TEXT PRIMARY KEY REFERENCES sessions(id), selection TEXT NOT NULL, pending INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS staged_session_targets (session_id TEXT PRIMARY KEY REFERENCES sessions(id), selection TEXT NOT NULL, targets TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS uploaded_images (session_id TEXT NOT NULL REFERENCES sessions(id), path TEXT NOT NULL, target TEXT NOT NULL, digest TEXT NOT NULL, PRIMARY KEY(session_id,path));
             CREATE TABLE IF NOT EXISTS message_files (session_id TEXT NOT NULL REFERENCES sessions(id), item_id TEXT NOT NULL, targets TEXT NOT NULL, files TEXT NOT NULL, PRIMARY KEY(session_id,item_id));
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
             CREATE TABLE IF NOT EXISTS runtime_sessions (session_id TEXT PRIMARY KEY REFERENCES sessions(id));
             CREATE TABLE IF NOT EXISTS host_sessions (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id));
             CREATE TABLE IF NOT EXISTS session_settings (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), sandbox TEXT, effective_sandbox TEXT);
             CREATE TABLE IF NOT EXISTS command_receipts (
               id TEXT PRIMARY KEY, operation TEXT NOT NULL, response TEXT);
             CREATE TABLE IF NOT EXISTS session_presentation (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS session_usage (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS session_order (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), starred INTEGER NOT NULL DEFAULT 0, position INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS session_archive (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id));
             CREATE TABLE IF NOT EXISTS session_model (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), selection TEXT, effective TEXT);
             CREATE TABLE IF NOT EXISTS prompt_settings (id TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS session_prompt (
               session_id TEXT PRIMARY KEY REFERENCES sessions(id), text TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS context_tool_receipts (
               id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
               thread_id TEXT NOT NULL, call_id TEXT NOT NULL, request TEXT NOT NULL,
               response TEXT NOT NULL, UNIQUE(session_id,thread_id,call_id));
             UPDATE environments SET status='stopped', error=NULL;
             UPDATE containers SET status='stopped', error=NULL;
             UPDATE sessions SET status='disconnected', error=NULL;
             UPDATE pending SET state='unavailable' WHERE state IN ('pending','responding','delivered');",
        )?;
        crate::notifications::initialize(&connection)?;
        connection.execute("INSERT INTO session_order(session_id,position) SELECT id, (SELECT COALESCE(MAX(position),0) FROM session_order) + ROW_NUMBER() OVER (ORDER BY created,id) FROM sessions WHERE id NOT IN (SELECT session_id FROM session_order)", [])?;
        let has_engine = connection.prepare("PRAGMA table_info(containers)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|column| column == "engine");
        if !has_engine {
            connection.execute("ALTER TABLE containers ADD COLUMN engine TEXT NOT NULL DEFAULT 'docker'", [])?;
        }
        // Add identities to existing data without replacing sessions or history.
        let ids = connection.prepare("SELECT id FROM sessions WHERE id NOT IN (SELECT session_id FROM session_presentation)")?
            .query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            connection.execute(
                "INSERT INTO session_presentation VALUES(?1,?2)",
                params![
                    id,
                    serde_json::to_string(&crate::session_context::generate_presentation())?
                ],
            )?;
        }
        // Recover the last report already in persisted history on first upgrade.
        let usage_ids = connection
            .prepare(
                "SELECT id FROM sessions WHERE id NOT IN (SELECT session_id FROM session_usage) OR id IN (SELECT session_id FROM session_usage WHERE json_type(value,'$.input_tokens') IS NULL)",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in usage_ids {
            let report: Option<(String,Option<i64>)> = connection.query_row(
                "SELECT message,unixepoch(at) FROM events WHERE session_id=?1 AND json_extract(message,'$.method')='thread/tokenUsage/updated' ORDER BY seq DESC LIMIT 1",
                [&id], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((message, at)) = report {
                let message: Value = serde_json::from_str(&message)?;
                let mut usage = crate::usage::context(&message["params"]["tokenUsage"]);
                usage["reported_at"] = serde_json::json!(at);
                connection.execute(
                    "INSERT INTO session_usage VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET value=json_set(excluded.value,'$.reported_at',json_extract(session_usage.value,'$.reported_at'))",
                    params![id, usage.to_string()],
                )?;
            }
        }
        let store = Self(Mutex::new(connection));
        // Interrupted checks are durable unknown outcomes, never replayed.
        let interrupted = store.lock()?.prepare("SELECT session_id,item_id,files FROM message_files WHERE files LIKE '%\"pending\"%'")?
            .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id,item,encoded) in interrupted {
            let mut files: Vec<demodex_protocol::MessageFile> = serde_json::from_str(&encoded)?;
            for file in &mut files { for check in &mut file.checks {
                if check.state == "pending" { check.state="not-checked".into(); check.error=Some("Check interrupted by daemon restart; not retried".into()); }
            }}
            store.finish_message_files(&id,&item,&files)?;
        }
        for session in store.list()? {
            store.ensure_target_selection(&session.id)?;
        }
        Ok(store)
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))
    }

    pub fn receipt(&self, id: &str) -> Result<Option<(String, Option<String>)>> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT operation,response FROM command_receipts WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn begin_command(&self, id: &str, operation: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO command_receipts(id,operation) VALUES(?1,?2)",
            params![id, operation],
        )?;
        Ok(())
    }

    pub fn finish_command(&self, id: &str, response: &str) -> Result<()> {
        self.lock()?.execute(
            "UPDATE command_receipts SET response=?2 WHERE id=?1",
            params![id, response],
        )?;
        Ok(())
    }

    pub fn environment_create(
        &self,
        name: &str,
        memory_mib: u32,
        cpus: u16,
        internet: bool,
    ) -> Result<Environment> {
        let id = uuid::Uuid::new_v4().to_string();
        self.lock()?.execute(
            "INSERT INTO environments VALUES(?1,?2,?3,?4,?5,'stopped',NULL)",
            params![id, name, memory_mib, cpus, internet],
        )?;
        self.environment(&id)
    }
    pub fn environments(&self) -> Result<Vec<Environment>> {
        let db = self.lock()?;
        let mut query = db.prepare(
            "SELECT id,name,memory_mib,cpus,internet,status,error FROM environments ORDER BY rowid",
        )?;
        Ok(query
            .query_map([], |r| {
                Ok(Environment {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    memory_mib: r.get(2)?,
                    cpus: r.get(3)?,
                    internet: r.get(4)?,
                    status: r.get(5)?,
                    error: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn environment(&self, id: &str) -> Result<Environment> {
        self.environments()?
            .into_iter()
            .find(|e| e.id == id)
            .context("environment not found")
    }
    pub fn environment_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.lock()?.execute(
            "UPDATE environments SET status=?2,error=?3 WHERE id=?1",
            params![id, status, error],
        )?;
        Ok(())
    }
    #[cfg(test)]
    pub fn bind_environment(&self, session: &str, environment: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO session_environment VALUES(?1,?2)",
            params![session, environment],
        )?;
        Ok(())
    }
    pub fn session_environment(&self, session: &str) -> Result<Option<String>> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT environment_id FROM session_environment WHERE session_id=?1",
                [session],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn retarget(&self, session: &str, endpoint: &str, targets: &[Target]) -> Result<()> {
        self.lock()?.execute(
            "UPDATE sessions SET endpoint=?2,targets=?3 WHERE id=?1",
            params![session, endpoint, serde_json::to_string(targets)?],
        )?;
        Ok(())
    }
    #[cfg(test)]
    pub fn bind_host(&self, session: &str) -> Result<()> {
        self.lock()?
            .execute("INSERT INTO host_sessions VALUES(?1)", [session])?;
        Ok(())
    }
    pub fn host_sessions(&self) -> Result<Vec<String>> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT session_id FROM host_sessions")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create(
        &self,
        name: &str,
        endpoint: &str,
        targets: &[Target],
        thread_id: Option<&str>,
    ) -> Result<Session> {
        self.create_attached(name, endpoint, targets, thread_id, None)
    }

    pub fn create_attached(
        &self,
        name: &str,
        endpoint: &str,
        targets: &[Target],
        thread_id: Option<&str>,
        attachment: Option<&crate::targets::Selection>,
    ) -> Result<Session> {
        self.create_initial(name, endpoint, targets, thread_id, attachment, None)
    }

    pub fn create_selected(
        &self,
        name: &str,
        endpoint: &str,
        targets: &[Target],
        selection: &[crate::targets::Selection],
        sandbox: Option<Sandbox>,
    ) -> Result<Session> {
        self.create_initial(
            name,
            endpoint,
            targets,
            None,
            None,
            Some((selection, sandbox)),
        )
    }

    pub fn uses_runtime(&self, id: &str) -> Result<bool> {
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }

    fn create_initial(
        &self,
        name: &str,
        endpoint: &str,
        targets: &[Target],
        thread_id: Option<&str>,
        attachment: Option<&crate::targets::Selection>,
        selected: Option<(&[crate::targets::Selection], Option<Sandbox>)>,
    ) -> Result<Session> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        tx.execute("INSERT INTO sessions(id,name,endpoint,targets,thread_id,status) VALUES(?1,?2,?3,?4,?5,'disconnected')",
            params![id, name, endpoint, serde_json::to_string(targets)?, thread_id])?;
        tx.execute("INSERT INTO session_order(session_id,position) VALUES(?1,(SELECT COALESCE(MAX(position),0)+1 FROM session_order))", [&id])?;
        tx.execute(
            "INSERT INTO session_presentation VALUES(?1,?2)",
            params![
                id,
                serde_json::to_string(&crate::session_context::generate_presentation())?
            ],
        )?;
        if let Some(attachment) = attachment {
            // Publish the session and its initial managed selection atomically.
            // Concurrent snapshots must never migrate a half-created host/VM
            // session as an external executor or an empty selection.
            if attachment.id == "host" {
                tx.execute("INSERT INTO host_sessions VALUES(?1)", [&id])?;
            } else {
                let vm = attachment
                    .id
                    .strip_prefix("vm-")
                    .context("Invalid managed attachment")?;
                tx.execute(
                    "INSERT INTO session_environment VALUES(?1,?2)",
                    params![id, vm],
                )?;
            }
            tx.execute(
                "INSERT INTO session_targets VALUES(?1,?2,0)",
                params![id, serde_json::to_string(&[attachment])?],
            )?;
        }
        if let Some((selection, sandbox)) = selected {
            tx.execute("INSERT INTO runtime_sessions VALUES(?1)", [&id])?;
            tx.execute(
                "INSERT INTO session_targets VALUES(?1,?2,0)",
                params![id, serde_json::to_string(selection)?],
            )?;
            tx.execute(
                "INSERT INTO session_settings(session_id,sandbox) VALUES(?1,?2)",
                params![id, sandbox.map(|v| serde_json::to_string(&v)).transpose()?],
            )?;
        }
        tx.commit()?;
        drop(db);
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
        Self::list_from(&db)
    }

    fn list_from(db: &Connection) -> Result<Vec<Session>> {
        let mut query = db.prepare("SELECT id,name,endpoint,thread_id,targets,status,error,sandbox,effective_sandbox,session_presentation.value, EXISTS(SELECT 1 FROM session_archive WHERE session_id=sessions.id), (SELECT value FROM session_usage WHERE session_id=sessions.id), session_order.starred, session_order.position FROM sessions LEFT JOIN session_settings ON sessions.id=session_settings.session_id JOIN session_presentation ON sessions.id=session_presentation.session_id JOIN session_order ON sessions.id=session_order.session_id ORDER BY session_order.position,created,id")?;
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
                row.get::<_, String>(9)?,
                row.get::<_, bool>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, bool>(12)?,
                row.get::<_, i64>(13)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                name,
                endpoint,
                thread_id,
                targets,
                status,
                error,
                sandbox,
                effective_sandbox,
                presentation,
                archived,
                context_usage,
                starred,
                sort_order,
            ) = row?;
            Ok(Session {
                archived,
                starred,
                sort_order,
                context_usage: context_usage
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or(Value::Null),
                id,
                name,
                endpoint,
                thread_id,
                targets: serde_json::from_str(&targets)?,
                status,
                error,
                sandbox: sandbox.as_deref().map(serde_json::from_str).transpose()?,
                effective_sandbox: effective_sandbox
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?,
                presentation: serde_json::from_str(&presentation)?,
            })
        })
        .collect()
    }

    pub fn context_usage(&self, id: &str, usage: &Value) -> Result<()> {
        // Resume can repeat an old report without running inference. Do not make
        // a possibly cold prompt appear fresh just because it was reattached.
        self.lock()?.execute("INSERT INTO session_usage VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET value=excluded.value WHERE json_extract(session_usage.value,'$.report_fingerprint') IS NOT json_extract(excluded.value,'$.report_fingerprint')",
            params![id, crate::usage::context(usage).to_string()])?;
        Ok(())
    }

    pub fn star(&self, id: &str, starred: bool) -> Result<()> {
        anyhow::ensure!(self.lock()?.execute("UPDATE session_order SET starred=?2 WHERE session_id=?1", params![id,starred])? == 1, "session not found");
        Ok(())
    }

    pub fn reorder_sessions(&self, expected: &[String], ids: &[String]) -> Result<()> {
        use std::collections::BTreeSet;
        anyhow::ensure!(!expected.is_empty() && expected.len() == ids.len(), "invalid session order");
        let unique: BTreeSet<_> = ids.iter().collect();
        anyhow::ensure!(unique.len() == ids.len() && unique == expected.iter().collect(), "session order must contain each session exactly once");
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let sessions = Self::list_from(&tx)?;
        let first = sessions.iter().find(|s| s.id == expected[0]).context("session not found")?;
        let group = session_group(first);
        let siblings: Vec<_> = sessions.iter().filter(|s| s.archived == first.archived && s.starred == first.starred && session_group(s) == group).collect();
        anyhow::ensure!(siblings.iter().map(|s| &s.id).eq(expected.iter()), "session group or order changed; refresh before reordering");
        for (id, previous) in ids.iter().zip(siblings) {
            tx.execute("UPDATE session_order SET position=?2 WHERE session_id=?1", params![id,previous.sort_order])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn move_session(&self, id: &str, neighbor: &str) -> Result<()> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let sessions = Self::list_from(&tx)?;
        let session = sessions.iter().find(|s| s.id == id).context("session not found")?;
        let group = session_group(session);
        let siblings: Vec<_> = sessions.iter().filter(|s| s.archived == session.archived && s.starred == session.starred && session_group(s) == group).collect();
        let index = siblings.iter().position(|s| s.id == id).unwrap();
        let other = siblings.iter().position(|s| s.id == neighbor).context("sessions must be in the same project group and have the same star state")?;
        anyhow::ensure!(index.abs_diff(other) == 1, "session order changed; refresh before moving again");
        tx.execute("UPDATE session_order SET position=?2 WHERE session_id=?1",params![id,siblings[other].sort_order])?;
        tx.execute("UPDATE session_order SET position=?2 WHERE session_id=?1",params![neighbor,session.sort_order])?;
        tx.commit()?;
        Ok(())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty() && name.chars().count() <= 120, "session name must be 1–120 characters");
        self.get(id)?;
        self.lock()?.execute("UPDATE sessions SET name=?2 WHERE id=?1", params![id, name])?;
        Ok(())
    }

    pub fn archive(&self, id: &str, archived: bool) -> Result<()> {
        self.get(id)?;
        let db = self.lock()?;
        if archived {
            db.execute(
                "INSERT OR IGNORE INTO session_archive(session_id) VALUES(?1)",
                [id],
            )?;
        } else {
            db.execute("DELETE FROM session_archive WHERE session_id=?1", [id])?;
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Session> {
        self.list()?
            .into_iter()
            .find(|s| s.id == id)
            .context("session not found")
    }

    pub fn enable_context_reporting(&self, id: &str) -> Result<()> {
        let mut presentation = self.get(id)?.presentation;
        presentation.context_reporting = true;
        self.lock()?.execute(
            "UPDATE session_presentation SET value=?2 WHERE session_id=?1",
            params![id, serde_json::to_string(&presentation)?],
        )?;
        Ok(())
    }

    pub fn model_settings(&self, id: &str) -> Result<Value> {
        let values: Option<(Option<String>, Option<String>)> = self
            .lock()?
            .query_row(
                "SELECT selection,effective FROM session_model WHERE session_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (selection, effective) = values.unwrap_or_default();
        Ok(
            serde_json::json!({"selection":selection.map(|v|serde_json::from_str::<Value>(&v)).transpose()?,"effective":effective.map(|v|serde_json::from_str::<Value>(&v)).transpose()?}),
        )
    }
    pub fn model_selection(&self, id: &str, value: &Value) -> Result<()> {
        self.lock()?.execute("INSERT INTO session_model(session_id,selection) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET selection=excluded.selection",params![id,value.to_string()])?;
        Ok(())
    }
    pub fn model_effective(&self, id: &str, value: &Value) -> Result<()> {
        self.lock()?.execute("INSERT INTO session_model(session_id,effective) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET effective=excluded.effective",params![id,value.to_string()])?;
        Ok(())
    }

    /// Metadata and its UUID receipt commit together before replying to Codex.
    /// A retried call returns its original result, never overwrites newer context.
    pub fn context_tool(&self, id: &str, thread: &str, request: &Value) -> Result<Value> {
        use crate::session_context::{self, UserVisibleContext};
        let mut session = self.get(id)?;
        anyhow::ensure!(
            request["threadId"] == thread && session.thread_id.as_deref() == Some(thread),
            "tool call does not belong to this session"
        );
        let call_id = request["callId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("missing tool call ID")?;
        let request_text = request.to_string();
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let previous: Option<(String, String)> = tx.query_row("SELECT request,response FROM context_tool_receipts WHERE session_id=?1 AND thread_id=?2 AND call_id=?3", params![id,thread,call_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((original, response)) = previous {
            anyhow::ensure!(
                original == request_text,
                "tool call ID reused with different arguments"
            );
            return Ok(serde_json::from_str(&response)?);
        }
        // Read display fields under the same transaction that writes them, so
        // concurrent identity/context updates cannot overwrite each other.
        let (title, presentation): (String, String) = tx.query_row(
            "SELECT sessions.name,session_presentation.value FROM sessions JOIN session_presentation ON sessions.id=session_presentation.session_id WHERE sessions.id=?1",
            [id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        session.name = title;
        session.presentation = serde_json::from_str(&presentation)?;
        let result = (|| -> Result<Value> {
            anyhow::ensure!(
                session.presentation.context_reporting,
                "Demodex context tools were not registered for this thread"
            );
            anyhow::ensure!(
                request["namespace"] == "demodex",
                "unsupported tool namespace"
            );
            match request["tool"].as_str().unwrap_or("") {
                "set_user_visible_session_context" => {
                    let mut context: UserVisibleContext =
                        serde_json::from_value(request["arguments"].clone())?;
                    session_context::validate(&mut context, &session.targets)?;
                    session.presentation.context = Some(context);
                    Ok(serde_json::to_value(&session.presentation)?)
                }
                "notify" => {
                    let input: crate::notifications::NotificationInput = serde_json::from_value(request["arguments"].clone())?;
                    crate::notifications::record(&tx, &session, input)
                }
                "set_session_identity" => {
                    let mut update: session_context::IdentityUpdate =
                        serde_json::from_value(request["arguments"].clone())?;
                    update.validate()?;
                    if let Some(title) = update.title { session.name = title; }
                    if let Some(name) = update.name { session.presentation.name = name; }
                    if let Some(icon) = update.icon { session.presentation.icon = icon; }
                    Ok(serde_json::json!({"session_id":session.id,"title":session.name,"presentation":session.presentation}))
                }
                "get_session_context" => {
                    anyhow::ensure!(
                        request["arguments"]
                            .as_object()
                            .is_some_and(|v| v.is_empty()),
                        "get_session_context takes no arguments"
                    );
                    Ok(
                        serde_json::json!({"session_id":session.id,"thread_id":session.thread_id,"title":session.name,"presentation":session.presentation,"environments":session.targets.iter().map(|t| serde_json::json!({"environment_id":t.id,"execution_directory":t.cwd,"executor_guidance":if t.id.starts_with("ssh-"){Some(crate::ssh::AGENT_INSTRUCTIONS)}else{None}})).collect::<Vec<_>>()}),
                    )
                }
                _ => anyhow::bail!("unsupported Demodex tool"),
            }
        })();
        let response = session_context::response(result);
        tx.execute(
            "INSERT INTO context_tool_receipts VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                id,
                thread,
                call_id,
                request_text,
                response.to_string()
            ],
        )?;
        if response["success"] == true && matches!(request["tool"].as_str(), Some("set_user_visible_session_context" | "set_session_identity")) {
            if request["tool"] == "set_session_identity" {
                tx.execute("UPDATE sessions SET name=?2 WHERE id=?1", params![id, session.name])?;
            }
            tx.execute(
                "UPDATE session_presentation SET value=?2 WHERE session_id=?1",
                params![id, serde_json::to_string(&session.presentation)?],
            )?;
        }
        tx.commit()?;
        Ok(response)
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

    #[cfg(test)]
    pub fn claim_message_files(&self, id: &str, item: &str, targets: &[Target], files: &[demodex_protocol::MessageFile]) -> Result<bool> {
        let db = self.lock()?;
        Ok(db.execute("INSERT OR IGNORE INTO message_files VALUES(?1,?2,?3,?4)", params![id,item,serde_json::to_string(targets)?,serde_json::to_string(files)?])? == 1)
    }
    pub fn completed_message_files(&self, id: &str, item: &str, targets: &[Target], files: &[demodex_protocol::MessageFile], message: &Value) -> Result<bool> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        tx.execute("INSERT INTO events(session_id,message) VALUES(?1,?2)", params![id,serde_json::to_string(message)?])?;
        let claimed = tx.execute("INSERT OR IGNORE INTO message_files VALUES(?1,?2,?3,?4)", params![id,item,serde_json::to_string(targets)?,serde_json::to_string(files)?])? == 1;
        tx.commit()?;
        Ok(claimed)
    }
    pub fn message_files(&self, id: &str, item: &str) -> Result<Option<(Vec<Target>, Vec<demodex_protocol::MessageFile>)>> {
        let db = self.lock()?;
        let row: Option<(String,String)> = db.query_row("SELECT targets,files FROM message_files WHERE session_id=?1 AND item_id=?2", params![id,item], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(targets,files)| Ok((serde_json::from_str(&targets)?,serde_json::from_str(&files)?))).transpose()
    }
    pub fn finish_message_files(&self, id: &str, item: &str, files: &[demodex_protocol::MessageFile]) -> Result<()> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        tx.execute("UPDATE message_files SET files=?3 WHERE session_id=?1 AND item_id=?2", params![id,item,serde_json::to_string(files)?])?;
        let message=serde_json::json!({"method":"demodex/messageFiles","params":{"itemId":item,"files":files}});
        tx.execute("INSERT INTO events(session_id,message) VALUES(?1,?2)", params![id,serde_json::to_string(&message)?])?;
        tx.commit()?;
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

    pub fn conversation(&self, id: &str) -> Result<demodex_protocol::ConversationSnapshot> {
        // Capture a finite high-water mark; later events are fetched by cursor.
        let end: i64 = self.lock()?.query_row("SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?1", [id], |r| r.get(0))?;
        let mut transcript = demodex_protocol::transcript::Transcript::default();
        let mut count = 0usize;
        let mut after = 0;
        while after < end {
            let batch: Vec<_> = self.events(id, after)?.into_iter().take_while(|event| event.seq <= end).collect();
            if batch.is_empty() { break; }
            after = batch.last().unwrap().seq;
            count += batch.len();
            transcript.append(&batch.into_iter().map(|event| json!(event)).collect::<Vec<_>>());
        }
        let items: Vec<_> = transcript.chunks.iter().flat_map(|chunk| chunk.iter().map(|item| item.as_ref().clone())).collect();
        Ok(demodex_protocol::ConversationSnapshot { items, cursor:end, event_count:count as u64 })
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
            "UPDATE pending SET state=?2 WHERE key=?1 AND state NOT IN ('resolved','unavailable')",
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


impl crate::store::Store {
    pub(crate) fn prompt_record(&self, id: &str) -> Result<Value> {
        let text: Option<String> = self.0.lock().unwrap().query_row("SELECT value FROM prompt_settings WHERE id=?1", [id], |r| r.get(0)).optional()?;
        Ok(text.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or(Value::Null))
    }
    pub(crate) fn put_prompt_record(&self, id: &str, value: &Value) -> Result<()> {
        self.0.lock().unwrap().execute("INSERT INTO prompt_settings VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value", params![id, value.to_string()])?;
        Ok(())
    }
    pub(crate) fn prompt_settings(&self) -> Result<(u64, demodex_protocol::PromptSettings)> {
        let value = self.prompt_record("server")?;
        Ok((value["revision"].as_u64().unwrap_or(0), if value.is_null() { demodex_protocol::PromptSettings::default() } else { serde_json::from_value(value["settings"].clone())? }))
    }
    pub(crate) fn set_prompt_settings(&self, expected: u64, settings: &demodex_protocol::PromptSettings) -> Result<u64> {
        crate::prompts::validate(settings)?;
        let mut connection = self.0.lock().unwrap();
        let tx = connection.transaction()?;
        let old: Option<String> = tx.query_row("SELECT value FROM prompt_settings WHERE id='server'", [], |r|r.get(0)).optional()?;
        let revision = old.map(|s|serde_json::from_str::<Value>(&s)).transpose()?.and_then(|v|v["revision"].as_u64()).unwrap_or(0);
        ensure!(expected == revision, "Prompt settings changed in another window. Reload before saving.");
        let revision=revision.checked_add(1).context("Prompt revision exhausted")?;
        tx.execute("INSERT INTO prompt_settings VALUES('server',?1) ON CONFLICT(id) DO UPDATE SET value=excluded.value", [json!({"revision":revision,"settings":settings}).to_string()])?;
        tx.commit()?;
        Ok(revision)
    }
}
impl Store { pub(crate) fn remove_legacy_prompt(&self,id:&str)->Result<()> { self.0.lock().unwrap().execute("DELETE FROM session_prompt WHERE session_id=?1",[id])?;Ok(()) } }


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn stars_and_order_stay_within_groups_and_survive_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("order.db");
        let store = Store::open(&path)?;
        let a = store.create("A","ws://localhost:1",&[],Some("a"))?;
        let b = store.create("B","ws://localhost:1",&[],Some("b"))?;
        let c = store.create("C","ws://localhost:1",&[],Some("c"))?;
        let d = store.create("D","ws://localhost:1",&[Target{id:"host".into(),url:"ws://localhost:2".into(),cwd:"/other".into()}],Some("d"))?;
        assert!(store.move_session(&a.id,&c.id).is_err());
        store.move_session(&b.id,&a.id)?;
        assert_eq!(store.list()?[0].id,b.id);
        store.star(&a.id,true)?;
        assert!(store.move_session(&b.id,&a.id).is_err());
        assert!(store.move_session(&b.id,&d.id).is_err());
        store.star(&c.id,true)?;
        store.move_session(&c.id,&a.id)?;
        assert!(store.get(&c.id)?.sort_order < store.get(&a.id)?.sort_order);
        store.archive(&c.id,true)?;
        assert!(store.move_session(&c.id,&a.id).is_err());
        store.archive(&c.id,false)?;
        drop(store);
        let store = Store::open(&path)?;
        assert!(store.get(&a.id)?.starred);
        assert!(store.get(&c.id)?.starred);
        assert!(!store.get(&b.id)?.starred);
        assert!(store.get(&c.id)?.sort_order < store.get(&a.id)?.sort_order);
        assert_eq!(store.get(&b.id)?.thread_id.as_deref(),Some("b"));
        Ok(())
    }

    #[test]
    fn conversation_snapshot_preserves_projection_cursor_and_raw_events() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let id = store.create("history", "ws://localhost:1", &[], None)?.id;
        store.event(&id, &json!({"method":"item/started","params":{"item":{"id":"message","type":"agentMessage","text":""}}}))?;
        for _ in 0..1100 {
            store.event(&id, &json!({"method":"item/agentMessage/delta","params":{"itemId":"message","delta":"x"}}))?;
        }
        store.event(&id, &json!({"method":"future/unknown","params":{"keep":"original"}}))?;
        let snapshot = store.conversation(&id)?;
        assert_eq!(snapshot.event_count, 1102);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0]["text"].as_str().unwrap().len(), 1100);
        let at = snapshot.items[0]["_demodexAt"].clone();
        assert!(at.is_string());
        let mut restored = demodex_protocol::transcript::Transcript::restore(&snapshot.items, snapshot.cursor);
        assert!(store.events(&id, snapshot.cursor)?.is_empty());
        store.event(&id, &json!({"method":"item/completed","params":{"item":{"id":"message","type":"agentMessage","text":"done"}}}))?;
        restored.append(&store.events(&id, snapshot.cursor)?.into_iter().map(|e|json!(e)).collect::<Vec<_>>());
        let fresh = store.conversation(&id)?;
        assert_eq!(*restored.chunks[0][0], fresh.items[0]);
        assert_eq!(fresh.items[0]["_demodexAt"], at);
        assert_eq!(store.events(&id, snapshot.cursor - 1)?[0].message["method"], "future/unknown");
        Ok(())
    }

    #[test]
    fn ordering_separates_targets_at_the_same_path() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let host = Target { id:"host-old".into(), url:"ws://localhost:1".into(), cwd:"/project".into() };
        let a = store.create("host", "ws://localhost:1", std::slice::from_ref(&host), None)?;
        let mut other = host.clone(); other.id = "ssh-other".into();
        let b = store.create("ssh", "ws://localhost:1", &[other], None)?;
        let mut host = host; host.id = "host-new".into();
        let c = store.create("host again", "ws://localhost:1", &[host], None)?;
        assert_eq!(session_group(&a), session_group(&c));
        assert_ne!(session_group(&a), session_group(&b));
        store.reorder_sessions(&[a.id.clone(),c.id.clone()], &[c.id,a.id])?;
        Ok(())
    }

    #[test]
    fn atomic_reorder_rejects_stale_partial_and_cross_group_lists() -> Result<()> {
        let store = Store::open(std::path::Path::new(":memory:"))?;
        let a=store.create("A","ws://localhost:1",&[],Some("a"))?.id;
        let b=store.create("B","ws://localhost:1",&[],Some("b"))?.id;
        let c=store.create("C","ws://localhost:1",&[],Some("c"))?.id;
        let original=vec![a.clone(),b.clone(),c.clone()];
        let reordered=vec![c.clone(),a.clone(),b.clone()];
        store.reorder_sessions(&original,&reordered)?;
        assert_eq!(store.list()?.iter().map(|s|s.id.clone()).collect::<Vec<_>>(),reordered);
        assert!(store.reorder_sessions(&original,&reordered).is_err());
        assert!(store.reorder_sessions(&reordered,&[a.clone(),a.clone(),b.clone()]).is_err());
        assert!(store.reorder_sessions(&[a.clone(),b.clone()],&[b.clone(),a.clone()]).is_err());
        store.star(&a,true)?;
        assert!(store.reorder_sessions(&reordered,&original).is_err());
        store.reorder_sessions(&[c.clone(),b.clone()],&[b.clone(),c.clone()])?;
        store.archive(&c,true)?;
        assert!(store.reorder_sessions(&[b.clone(),c.clone()],&[c,b]).is_err());
        Ok(())
    }

    #[test]
    fn rename_validates_and_preserves_identity_across_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("rename.db");
        let id;
        {
            let store = Store::open(&path)?;
            let session = store.create("Before", "ws://localhost:1", &[], Some("thread"))?;
            id = session.id;
            assert!(store.rename(&id, "   ").is_err());
            assert!(store.rename(&id, &"a".repeat(121)).is_err());
            store.rename(&id, "  After 🦆  ")?;
        }
        let session = Store::open(&path)?.get(&id)?;
        assert_eq!(session.name,"After 🦆");
        assert_eq!(session.thread_id.as_deref(),Some("thread"));
        Ok(())
    }

    #[tokio::test]
    async fn archive_requires_stopped_state_and_survives_restart_without_losing_history()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive.db");
        let id;
        {
            let manager = crate::manager::Manager::new(Store::open(&path)?);
            let session = manager
                .store
                .create("keep", "ws://localhost:1", &[], Some("thread"))?;
            id = session.id;
            manager.store.event(&id, &json!({"history":"preserved"}))?;
            for state in [
                "working",
                "active",
                "waiting",
                "connecting",
                "running",
                "error",
            ] {
                manager.store.status(&id, state, None)?;
                assert!(manager.archive(&id, true).await.is_err());
                assert!(!manager.store.get(&id)?.archived);
            }
            manager.store.status(&id, "disconnected", None)?;
            manager.archive(&id, true).await?;
            assert!(
                manager
                    .prompt(&id, "must not run")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Restore")
            );
            assert!(
                manager
                    .connect(&id)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Restore")
            );
        }
        let manager = crate::manager::Manager::new(Store::open(&path)?);
        assert!(manager.store.get(&id)?.archived);
        assert_eq!(manager.store.get(&id)?.thread_id.as_deref(), Some("thread"));
        assert_eq!(manager.store.events(&id, 0)?.len(), 1);
        manager.archive(&id, false).await?;
        assert!(!manager.store.get(&id)?.archived);
        assert!(manager.archive("missing", true).await.is_err());
        Ok(())
    }

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
    fn late_send_acknowledgement_cannot_revive_a_lost_approval() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let session = store.create("test", "ws://localhost:1", &[], None)?;
        store.request(
            &session.id,
            "old",
            &json!({"id":7,"method":"approval","params":{}}),
        )?;
        store.claim(&session.id, "old:7", "old")?;
        store.disconnected(&session.id, "old")?;
        store.request_state("old:7", "delivered")?;
        assert_eq!(store.pending(&session.id)?[0].state, "unavailable");
        assert!(store.claim(&session.id, "old:7", "replacement").is_err());
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
