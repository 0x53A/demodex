//! Daemon-local, durable reports. Filing and the dynamic tool receipt commit together.
use crate::store::{Session, Store};
use anyhow::{Result, ensure};
use demodex_protocol::{BugReport, BugReportPage};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    title: String,
    description: String,
}

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS bug_reports (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
        title TEXT NOT NULL, description TEXT NOT NULL,
        session_id TEXT NOT NULL, thread_id TEXT, session_title TEXT NOT NULL,
        project TEXT NOT NULL);")?;
    Ok(())
}

pub fn record(db: &Connection, session: &Session, input: Input) -> Result<Value> {
    ensure!(!input.title.trim().is_empty() && input.title.chars().count() <= 120
        && !input.title.chars().any(char::is_control), "title must be 1–120 characters on one line");
    ensure!(!input.description.trim().is_empty() && input.description.chars().count() <= 16000
        && !input.description.chars().any(|c| c.is_control() && c != '\n' && c != '\t'),
        "description must be 1–16000 characters without control characters except newline and tab");
    db.execute("INSERT INTO bug_reports(title,description,session_id,thread_id,session_title,project) VALUES(?1,?2,?3,?4,?5,?6)",
        params![input.title.trim(),input.description,session.id,session.thread_id,session.name,serde_json::to_string(&session.presentation.context)?])?;
    Ok(json!({"report_id":db.last_insert_rowid(),"recorded":true,"location":"Server settings → Bug reports on this daemon","message":"Saved for later review. No notification sent."}))
}

/// Small runtime summary; never fetch report bodies just to show an indicator.
pub fn summary(store: &Store) -> Result<Value> {
    let db = store.lock()?;
    let latest: i64 = db.query_row("SELECT coalesce(max(id),0) FROM bug_reports", [], |r| r.get(0))?;
    let server: String = db.query_row("SELECT server_id FROM push_identity WHERE id=1", [], |r| r.get(0))?;
    Ok(json!({"server_id":server,"latest_id":latest}))
}

pub fn list(store: &Store, before: Option<i64>) -> Result<BugReportPage> {
    ensure!(before.is_none_or(|n| n > 0), "before must be a positive report ID");
    let db = store.lock()?;
    let mut stmt = db.prepare("SELECT id,created_at,title,description,session_id,thread_id,session_title,project FROM bug_reports WHERE id < ?1 ORDER BY id DESC LIMIT 51")?;
    let rows = stmt.query_map([before.unwrap_or(i64::MAX)], |r| {
        Ok((BugReport {
            id:r.get(0)?,created_at:r.get(1)?,title:r.get(2)?,description:r.get(3)?,
            session_id:r.get(4)?,thread_id:r.get(5)?,session_title:r.get(6)?,project:None,
        }, r.get::<_,String>(7)?))
    })?;
    let mut reports = Vec::new();
    for row in rows {
        let (mut report, project) = row?;
        report.project = serde_json::from_str(&project)?;
        reports.push(report);
    }
    let next_before = if reports.len() > 50 {
        reports.truncate(50);
        reports.last().map(|r|r.id)
    } else { None };
    Ok(BugReportPage {reports,next_before})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(id: &str, title: &str) -> Value {
        json!({"namespace":"demodex","tool":"bug_report","threadId":"thread","callId":id,"arguments":{"title":title,"description":"Expected a reply.\nObserved a timeout."}})
    }
    #[test]
    fn reports_are_scoped_validated_durable_and_paged_without_replay() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let store = Store::open(&path)?;
        let session = store.create("Original title", "ws://localhost:1", &[], Some("thread"))?;
        store.enable_context_reporting(&session.id)?;
        let first = call("first", "Missing reply");
        let response = store.context_tool(&session.id,"thread",&first)?;
        assert_eq!(response["success"],true);
        assert_eq!(store.context_tool(&session.id,"thread",&first)?,response);
        assert!(store.context_tool(&session.id,"thread",&call("first","Changed")).is_err());
        let mut foreign = call("foreign","Wrong thread");
        foreign["threadId"] = json!("other");
        assert!(store.context_tool(&session.id,"thread",&foreign).is_err());
        for (index,title) in ["", "   ", "newline\nhere", &"x".repeat(121)].iter().enumerate() {
            assert_eq!(store.context_tool(&session.id,"thread",&call(&format!("bad-{index}"),title))?["success"],false);
        }
        for (index,args) in [json!({"title":"Valid","description":" "}),json!({"title":"Valid","description":"x".repeat(16001)}),json!({"title":"Valid","description":"text","session_id":"other"})].iter().enumerate() {
            let mut bad=call(&format!("bad-args-{index}"),"Valid");bad["arguments"]=args.clone();
            assert_eq!(store.context_tool(&session.id,"thread",&bad)?["success"],false);
        }
        store.lock()?.execute_batch("CREATE TRIGGER fail_report_receipt BEFORE INSERT ON context_tool_receipts BEGIN SELECT RAISE(FAIL,'fixture failure'); END;")?;
        assert!(store.context_tool(&session.id,"thread",&call("receipt-failure","Must roll back")).is_err());
        store.lock()?.execute_batch("DROP TRIGGER fail_report_receipt")?;
        assert_eq!(list(&store,None)?.reports.len(),1);
        // Reports do not enqueue push or manufacture a conversation notification.
        assert!(store.events(&session.id,0)?.is_empty());
        assert_eq!(store.lock()?.query_row("SELECT count(*) FROM notification_outbox",[],|r|r.get::<_,i64>(0))?,0);
        store.rename(&session.id,"New title")?;
        for i in 0..50 {store.context_tool(&session.id,"thread",&call(&format!("next-{i}"),"Another report"))?;}
        let page=list(&store,None)?;
        assert_eq!(page.reports.len(),50);
        let older=list(&store,page.next_before)?;
        assert_eq!(older.reports.len(),1);
        assert_eq!(older.reports[0].session_title,"Original title");
        assert_eq!(older.reports[0].session_id,session.id);
        assert_eq!(older.next_before,None);
        drop(store);
        let store=Store::open(&path)?;
        assert_eq!(store.context_tool(&session.id,"thread",&first)?,response);
        assert_eq!(list(&store,page.next_before)?.reports,older.reports);
        Ok(())
    }
}
