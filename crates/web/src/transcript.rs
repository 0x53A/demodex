pub use demodex_protocol::transcript::*;

/// A partial view cannot distinguish unseen old snapshot items from new ones.
/// Rebuild it from the daemon's full projection before consuming that batch.
pub fn needs_history_reload(has_older: bool, events: &[serde_json::Value]) -> bool {
    has_older && events.iter().any(|event|
        event["message"]["method"] == "demodex/threadSnapshot")
}

/// Recently visited conversations, owned only by the current browser/host.
/// Moving projections preserves their cursor and avoids rebuilding render chunks.
#[derive(Default)]
pub struct ConversationCache {
    entries: std::collections::VecDeque<(String, Transcript, usize, Option<demodex_protocol::ConversationPageCursor>)>,
}

impl ConversationCache {
    pub fn insert(&mut self, id: String, transcript: Transcript, count: usize, older: Option<demodex_protocol::ConversationPageCursor>) {
        self.take(&id);
        self.entries.push_back((id, transcript, count, older));
        while self.entries.len() > 8 {
            self.entries.pop_front();
        }
    }

    pub fn take(&mut self, id: &str) -> Option<(Transcript, usize, Option<demodex_protocol::ConversationPageCursor>)> {
        let index = self.entries.iter().position(|entry| entry.0 == id)?;
        let (_, transcript, count, older) = self.entries.remove(index)?;
        Some((transcript, count, older))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_history_reloads_resume_snapshots_but_keeps_live_events_incremental() {
        let snapshot = json!({"seq": 43, "message": {"method": "demodex/threadSnapshot",
            "params": {"thread": {"turns": [{"items": [
                {"id": "old", "type": "userMessage", "content": []},
                {"id": "recent", "type": "agentMessage", "text": "Recent"}
            ]}]}}}});
        let live = json!({"seq": 44, "message": {"method": "item/completed",
            "params": {"item": {"id": "new", "type": "agentMessage", "text": "New"}}}});
        // Reproduce the pagination boundary: the old ID exists only on an
        // unloaded page. Naive replay puts it after the recent message.
        let recent = json!({"id": "recent", "type": "agentMessage", "text": "Recent"});
        let mut partial = Transcript::restore(&[recent], 42);
        partial.append(&[snapshot.clone()]);
        assert_eq!(partial.chunks[0][1]["id"], "old");
        // A full daemon projection knows both IDs and retains their order.
        let mut full = Transcript::default();
        full.append(&[snapshot.clone()]);
        full.append(&[json!({"seq": 45, "message": snapshot["message"].clone()})]);
        assert_eq!(full.chunks[0].len(), 2);
        assert_eq!(full.chunks[0][0]["id"], "old");
        assert_eq!(full.chunks[0][1]["id"], "recent");
        assert!(needs_history_reload(true, &[snapshot.clone(), live.clone()]));
        assert!(!needs_history_reload(false, &[snapshot]));
        assert!(!needs_history_reload(true, &[live]));
        assert!(!needs_history_reload(true, &[]));
    }

    #[test]
    fn returning_preserves_render_chunks_and_incremental_cursor() {
        let transcript = Transcript::restore(&[json!({"id":"message", "type":"agentMessage", "text":"Earlier message"})], 42);
        let groups = transcript.groups.clone();
        let mut cache = ConversationCache::default();
        cache.insert("a".into(), transcript, 100, None);
        assert!(cache.take("b").is_none());
        let (restored, count, _) = cache.take("a").unwrap();
        assert_eq!(restored.cursor(), 42);
        assert_eq!(count, 100);
        assert!(std::rc::Rc::ptr_eq(&groups, &restored.groups));
        assert!(cache.take("a").is_none());
    }

    #[test]
    fn evicts_least_recently_visited_conversations() {
        let mut cache = ConversationCache::default();
        for i in 0..8 {
            cache.insert(i.to_string(), Transcript::default(), i, None);
        }
        let (transcript, count, older) = cache.take("0").unwrap();
        cache.insert("0".into(), transcript, count, older);
        cache.insert("8".into(), Transcript::default(), 8, None);
        assert!(cache.take("1").is_none());
        assert!(cache.take("0").is_some());
    }
}
