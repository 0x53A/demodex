pub use demodex_protocol::transcript::*;

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
