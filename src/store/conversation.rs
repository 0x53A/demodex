//! Durable, incrementally projected items. Raw events remain authoritative.
//! Published high-water marks retain their item versions across later updates.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use demodex_protocol::transcript::Transcript;

// Bump whenever projection semantics or dependency selection changes.
const INDEX_VERSION: i64 = 1;

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS conversation_index (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        version INTEGER NOT NULL, through INTEGER NOT NULL, event_count INTEGER NOT NULL,
        item_count INTEGER NOT NULL, next_position INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS conversation_items (
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        position INTEGER NOT NULL, from_seq INTEGER NOT NULL, to_seq INTEGER,
        item_id TEXT NOT NULL, value TEXT NOT NULL, bytes INTEGER NOT NULL, pending INTEGER NOT NULL,
        PRIMARY KEY(session_id,position,from_seq));
      CREATE INDEX IF NOT EXISTS conversation_page_order ON conversation_items(session_id,position DESC,from_seq,to_seq,bytes);
      CREATE UNIQUE INDEX IF NOT EXISTS conversation_current_item ON conversation_items(session_id,item_id) WHERE to_seq IS NULL;
      CREATE INDEX IF NOT EXISTS conversation_pending ON conversation_items(session_id,position) WHERE to_seq IS NULL AND pending=1;
      CREATE TABLE IF NOT EXISTS conversation_marks (
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        through INTEGER NOT NULL, event_count INTEGER NOT NULL, item_count INTEGER NOT NULL,
        PRIMARY KEY(session_id,through));")?;
    Ok(())
}

#[derive(Clone)]
struct StoredItem { position: i64, from: i64, encoded: String }

impl Store {
    pub(super) fn indexed_conversation_page(&self, id: &str, older: Option<&demodex_protocol::ConversationPageCursor>, budget: usize, limit: usize) -> Result<demodex_protocol::ConversationSnapshot> {
        let through = {
            let db = self.lock()?;
            let end: i64 = db.query_row("SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?1", [id], |r| r.get(0))?;
            let through = older.map_or(end, |c| c.through);
            ensure!((0..=end).contains(&through), "Invalid conversation cursor");
            through
        };
        loop {
            // Ready pages do not wait behind another session's backfill. The
            // database transaction publishes the mark before a builder can
            // retire item versions, even if that builder is already projecting.
            if let Some(page) = self.read_indexed_page(id, older, through, budget, limit)? { return Ok(page); }
            let guard = self.2.lock().map_err(|_| anyhow::anyhow!("conversation index poisoned"))?;
            self.ensure_conversation_index(id)?;
            if let Some(page) = self.read_indexed_page(id, older, through, budget, limit)? { return Ok(page); }
            let indexed: i64 = self.lock()?.query_row("SELECT through FROM conversation_index WHERE session_id=?1", [id], |r| r.get(0))?;
            if indexed >= through {
                // Pre-index/old-version cursors were never published by this
                // index. Preserve them using authoritative replay, not a guess.
                drop(guard);
                return self.replayed_conversation_page_limits(id, older, budget, limit);
            }
            self.advance_conversation_index(id, through)?;
            // Give other builders an opportunity between resumable batches.
            drop(guard);
        }
    }

    fn read_indexed_page(&self, id: &str, older: Option<&demodex_protocol::ConversationPageCursor>, through: i64, budget: usize, limit: usize) -> Result<Option<demodex_protocol::ConversationSnapshot>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let indexed: Option<i64> = tx.query_row("SELECT through FROM conversation_index WHERE session_id=?1 AND version=?2", params![id,INDEX_VERSION], |r| r.get(0)).optional()?;
        let Some(indexed) = indexed.filter(|seq| *seq >= through) else { return Ok(None); };
        // Initial reads may use a newer coherent view if indexing raced the
        // captured tail. Explicit older cursors retain their exact high water.
        let through = if older.is_none() { indexed } else { through };
        let counts: Option<(i64,i64)> = if indexed == through {
            let counts = tx.query_row("SELECT event_count,item_count FROM conversation_index WHERE session_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?)))?;
            tx.execute("INSERT OR IGNORE INTO conversation_marks VALUES(?1,?2,?3,?4)", params![id,through,counts.0,counts.1])?;
            Some(counts)
        } else {
            tx.query_row("SELECT event_count,item_count FROM conversation_marks WHERE session_id=?1 AND through=?2", params![id,through], |r| Ok((r.get(0)?,r.get(1)?))).optional()?
        };
        let Some((event_count,item_count)) = counts else { return Ok(None); };
        let before = older.map_or(item_count as u64, |c| c.before);
        ensure!(before <= item_count as u64, "Invalid conversation item cursor");
        let mut encoded = Vec::new();
        let mut bytes = 1024usize;
        {
            let mut query = tx.prepare("SELECT value,bytes FROM conversation_items INDEXED BY conversation_page_order WHERE session_id=?1 AND from_seq<=?2 AND (to_seq IS NULL OR to_seq>?2) ORDER BY position DESC LIMIT ?3 OFFSET ?4")?;
            let mut rows = query.query(params![id, through, limit.min(i64::MAX as usize) as i64, item_count - before as i64])?;
            while let Some(row) = rows.next()? {
                let size: i64 = row.get(1)?;
                if !encoded.is_empty() && bytes.saturating_add(size as usize) > budget { break; }
                bytes = bytes.saturating_add(size as usize);
                encoded.push(row.get::<_,String>(0)?);
            }
        }
        tx.commit()?;
        drop(db);
        let start = before - encoded.len() as u64;
        let items = encoded.into_iter().rev().map(|s| serde_json::from_str(&s)).collect::<serde_json::Result<_>>()?;
        Ok(Some(demodex_protocol::ConversationSnapshot { items, cursor:through, event_count:event_count as u64,
            older:(start > 0).then_some(demodex_protocol::ConversationPageCursor { through, before:start }) }))
    }

    fn ensure_conversation_index(&self, id: &str) -> Result<()> {
        let mut db = self.lock()?;
        let version: Option<i64> = db.query_row("SELECT version FROM conversation_index WHERE session_id=?1", [id], |r| r.get(0)).optional()?;
        if version == Some(INDEX_VERSION) { return Ok(()); }
        let tx = db.transaction()?;
        tx.execute("DELETE FROM conversation_items WHERE session_id=?1", [id])?;
        tx.execute("DELETE FROM conversation_marks WHERE session_id=?1", [id])?;
        tx.execute("INSERT OR REPLACE INTO conversation_index VALUES(?1,?2,0,0,0,0)", params![id,INDEX_VERSION])?;
        tx.commit()?;
        Ok(())
    }

    // Caller holds the builder mutex. Only one bounded event batch is committed,
    // including its progress marker, so shutdown/crash can resume without replay.
    fn advance_conversation_index(&self, id: &str, through: i64) -> Result<()> {
        let (after,count,mut item_count,mut next): (i64,i64,i64,i64) = self.lock()?.query_row(
            "SELECT through,event_count,item_count,next_position FROM conversation_index WHERE session_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let events = self.projection_events(id, after, through)?;
        let end = events.last().map_or(through, |e| e.seq);
        let mut required = BTreeSet::new();
        for event in &events { required.extend(Transcript::projection_dependencies(&event.message, event.seq)); }
        let mut old: BTreeMap<String, StoredItem> = BTreeMap::new();
        if events.iter().any(|e| !e.message.is_null()) {
            let db = self.lock()?;
            let mut pending = db.prepare("SELECT item_id,position,from_seq,value FROM conversation_items WHERE session_id=?1 AND to_seq IS NULL AND pending=1")?;
            for row in pending.query_map([id], |r| Ok((r.get(0)?, StoredItem { position:r.get(1)?, from:r.get(2)?, encoded:r.get(3)? })))? {
                let (key,item) = row?; old.insert(key,item);
            }
            let mut get = db.prepare("SELECT position,from_seq,value FROM conversation_items WHERE session_id=?1 AND item_id=?2 AND to_seq IS NULL")?;
            for key in required {
                if old.contains_key(&key) { continue; }
                if let Some(item) = get.query_row(params![id,key], |r| Ok(StoredItem { position:r.get(0)?, from:r.get(1)?, encoded:r.get(2)? })).optional()? { old.insert(key,item); }
            }
        }
        let mut ordered: Vec<_> = old.values().collect();
        ordered.sort_by_key(|item| item.position);
        let items = ordered.into_iter().map(|item| serde_json::from_str(&item.encoded)).collect::<serde_json::Result<Vec<_>>>()?;
        let mut transcript = Transcript::restore_projection(items, after);
        transcript.append_records(events.iter().map(|e| (Some(e.seq), e.at.as_str(), &e.message)));
        let removed = transcript.removed_projection_ids();
        let mut output = Vec::new();
        let mut surviving = BTreeSet::new();
        for item in transcript.into_projection() {
            let key = item["id"].as_str().context("Projected item without ID")?.to_owned();
            surviving.insert(key.clone());
            let encoded = serde_json::to_string(&item)?;
            let previous = old.get(&key);
            if previous.is_some_and(|v| v.encoded == encoded) && !removed.contains(&key) { continue; }
            let position = if let Some(previous) = previous.filter(|_| !removed.contains(&key)) { previous.position } else { let position = next; next += 1; position };
            let pending = item["_demodexLifecycle"] == "running" || item["_demodexSteering"].is_string();
            output.push((key,position,encoded,pending));
        }
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let published: Option<i64> = tx.query_row("SELECT MAX(through) FROM conversation_marks WHERE session_id=?1", [id], |r| r.get(0))?;
        let mut retire = |previous: &StoredItem| -> Result<()> {
            // Unpublished intermediate versions can be replaced. Published
            // versions survive for fixed high-water cursors, including removals.
            if published.is_some_and(|mark| previous.from <= mark) {
                tx.execute("UPDATE conversation_items SET to_seq=?4 WHERE session_id=?1 AND position=?2 AND from_seq=?3", params![id,previous.position,previous.from,end])?;
            } else {
                tx.execute("DELETE FROM conversation_items WHERE session_id=?1 AND position=?2 AND from_seq=?3", params![id,previous.position,previous.from])?;
            }
            item_count -= 1;
            Ok(())
        };
        for (key,previous) in &old { if !surviving.contains(key) { retire(previous)?; } }
        for (key,position,encoded,pending) in &output {
            if let Some(previous) = old.get(key) { retire(previous)?; }
            tx.execute("INSERT INTO conversation_items VALUES(?1,?2,?3,NULL,?4,?5,?6,?7)", params![id,position,end,key,encoded,(encoded.len()+1) as i64,pending])?;
        }
        drop(retire);
        item_count += output.len() as i64;
        tx.execute("UPDATE conversation_index SET through=?2,event_count=?3,item_count=?4,next_position=?5 WHERE session_id=?1", params![id,end,count+events.len() as i64,item_count,next])?;
        tx.commit()?;
        Ok(())
    }

    /// One fair background batch. Index all sessions, including legacy histories,
    /// without tying runtime startup or event ingestion to a full backfill.
    pub(crate) fn index_conversations_batch(&self) -> Result<bool> {
        let _guard = self.2.lock().map_err(|_| anyhow::anyhow!("conversation index poisoned"))?;
        let candidate: Option<(String,i64)> = self.lock()?.query_row(
            "SELECT s.id,(SELECT MAX(e.seq) FROM events e WHERE e.session_id=s.id) AS last FROM sessions s LEFT JOIN conversation_index c ON c.session_id=s.id WHERE last IS NOT NULL AND (c.version IS NULL OR c.version!=?1 OR c.through<last) ORDER BY COALESCE(c.through,0),s.id LIMIT 1",
            [INDEX_VERSION], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((id,end)) = candidate else { return Ok(false); };
        self.ensure_conversation_index(&id)?;
        self.advance_conversation_index(&id,end)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(store: &Store, id: &str) -> Result<demodex_protocol::ConversationSnapshot> {
        let page = store.indexed_conversation_page(id, None, usize::MAX, usize::MAX)?;
        let reference = store.replayed_conversation_page_limits(id, None, usize::MAX, usize::MAX)?;
        assert_eq!(serde_json::to_value(&page)?, serde_json::to_value(reference)?);
        Ok(page)
    }

    #[test]
    fn conversation_index_matches_replay_for_steering_snapshots_and_old_versions() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let id = store.create("indexed", "ws://localhost:1", &[], None)?.id;
        let events = [
            json!({"method":"item/completed","params":{"item":{"id":"old","type":"agentMessage","text":"original"}}}),
            json!({"method":"demodex/promptSteering","params":{"clientUserMessageId":"steer","turnId":"t","text":"hello"}}),
            json!({"method":"item/started","params":{"turnId":"t","item":{"id":"tool","type":"mcpToolCall"}}}),
            json!({"method":"demodex/promptSteeringFailed","params":{"clientUserMessageId":"steer"}}),
            json!({"method":"item/started","params":{"turnId":"t","item":{"id":"user","clientId":"steer","type":"userMessage","content":[{"type":"text","text":"hello"}]}}}),
            json!({"method":"demodex/promptSteering","params":{"clientUserMessageId":"same","turnId":"t","text":"same ID"}}),
            json!({"method":"item/completed","params":{"item":{"id":"middle","type":"agentMessage","text":"between"}}}),
            json!({"method":"item/started","params":{"turnId":"t","item":{"id":"same","type":"userMessage","content":[{"type":"text","text":"same ID"}]}}}),
            json!({"method":"item/mcpToolCall/progress","params":{"itemId":"tool","message":"working"}}),
            json!({"method":"item/agentMessage/delta","params":{"itemId":"old","delta":" more"}}),
            json!({"method":"demodex/threadSnapshot","params":{"thread":{"turns":[{"id":"t","items":[{"id":"old","type":"agentMessage","text":"revised"},{"id":"new","type":"agentMessage","text":"new"}]}]}}}),
            json!({"method":"demodex/messageFiles","params":{"itemId":"old","files":[]}}),
            json!({"method":"turn/completed","params":{"turn":{"id":"t","status":"failed","error":{"message":"stopped"}}}}),
            json!({"method":"demodex/notification","params":{"id":"n","message":"notice"}}),
            json!({"method":"demodex/notificationDelivery","params":{"id":"n","accepted":1}}),
            json!({"method":"turn/plan/updated","params":{"turnId":"t","plan":[]}}),
            json!({"method":"error","params":{"error":{"message":"test"}}}),
            json!({"method":"item/reasoning/summaryTextDelta","params":{"itemId":"reason","summaryIndex":0,"delta":"thinking"}}),
            json!({"method":"item/reasoning/summaryTextDelta","params":{"itemId":"reason","summaryIndex":0,"delta":" more"}}),
        ];
        let mut snapshots = Vec::new();
        for event in &events {
            store.event(&id,event)?;
            snapshots.push(check(&store,&id)?);
        }
        for snapshot in snapshots {
            let page = store.indexed_conversation_page(&id, Some(&demodex_protocol::ConversationPageCursor { through:snapshot.cursor, before:snapshot.items.len() as u64 }), usize::MAX, usize::MAX)?;
            assert_eq!(serde_json::to_value(page)?, serde_json::to_value(snapshot)?);
        }
        // Process removal and re-addition together, not just at request boundaries.
        store.event(&id,&json!({"method":"demodex/promptSteering","params":{"clientUserMessageId":"discard","turnId":"t","text":"x"}}))?;
        store.event(&id,&json!({"method":"demodex/promptSteeringDiscarded","params":{"clientUserMessageId":"discard"}}))?;
        store.event(&id,&json!({"method":"item/completed","params":{"item":{"id":"discard","type":"agentMessage","text":"new identity"}}}))?;
        check(&store,&id)?;
        Ok(())
    }

    #[test]
    fn indexed_pages_do_not_wait_for_an_unrelated_builder() -> Result<()> {
        let store = Arc::new(Store::open(Path::new(":memory:"))?);
        let id = store.create("ready", "ws://localhost:1", &[], None)?.id;
        store.event(&id,&json!({"method":"item/completed","params":{"item":{"id":"ready","type":"agentMessage","text":"ready"}}}))?;
        store.conversation(&id)?;
        let guard = store.2.lock().unwrap();
        let reader = store.clone();
        let (send,receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || { let _ = send.send(reader.conversation(&id)); });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        drop(guard);
        thread.join().unwrap();
        assert_eq!(result.context("Indexed read waited for builder")??.items[0]["text"], "ready");
        Ok(())
    }

    #[test]
    fn conversation_backfill_resumes_and_commits_progress_with_items() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let store = Store::open(&path)?;
        let id = store.create("backfill", "ws://localhost:1", &[], None)?.id;
        for n in 0..1100 { store.event(&id,&json!({"method":"item/agentMessage/delta","params":{"itemId":"live","delta":format!("{n},")}}))?; }
        assert!(store.index_conversations_batch()?);
        assert_eq!(store.lock()?.query_row("SELECT event_count FROM conversation_index WHERE session_id=?1", [&id], |r| r.get::<_,i64>(0))?, 500);
        drop(store);
        let store = Store::open(&path)?;
        store.lock()?.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON conversation_items BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;
        assert!(store.index_conversations_batch().is_err());
        assert_eq!(store.lock()?.query_row("SELECT event_count FROM conversation_index WHERE session_id=?1", [&id], |r| r.get::<_,i64>(0))?, 500);
        store.lock()?.execute_batch("DROP TRIGGER fail_projection")?;
        while store.index_conversations_batch()? {}
        check(&store,&id)?;
        // Unpublished intermediate versions do not accumulate during backfill.
        assert_eq!(store.lock()?.query_row("SELECT COUNT(*) FROM conversation_items", [], |r| r.get::<_,i64>(0))?, 1);
        Ok(())
    }
}
