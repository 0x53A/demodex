//! Last reported child activity, scoped to a live Codex connection.
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Activity(HashMap<String, bool>);
impl Activity {
    pub(crate) fn item(&mut self, item: &Value) {
        match item["type"].as_str() {
            Some("subAgentActivity") => {
                let Some(id) = item["agentThreadId"].as_str().filter(|id| !id.is_empty()) else { return };
                let active = match item["kind"].as_str() {
                    Some("started") => true,
                    Some("completed" | "interrupted") => false,
                    _ => return,
                };
                self.0.insert(id.into(), active);
            }
            Some("collabAgentToolCall") => {
                if let Some(states) = item["agentsStates"].as_object() {
                    for (id, state) in states {
                        let active = match state["status"].as_str() {
                            Some("pendingInit" | "running") => true,
                            Some("interrupted" | "completed" | "errored" | "shutdown" | "notFound") => false,
                            _ => continue,
                        };
                        self.0.insert(id.clone(), active);
                    }
                }
            }
            _ => {}
        }
    }
    pub(crate) fn snapshot(&mut self, thread: &Value) {
        if let Some(turns) = thread["turns"].as_array() {
            for turn in turns {
                if let Some(items) = turn["items"].as_array() { for item in items { self.item(item); } }
            }
        }
    }
    pub(crate) async fn reconcile(&mut self, rpc: &crate::rpc::Rpc) {
        use futures_util::StreamExt;
        // Historical spawn records can survive the runtime that owned the child.
        // Read metadata only, and bound both fan-out and total resume latency.
        let ids: Vec<_> = self.0.iter().filter(|(_,active)|**active).take(64).map(|(id,_)|id.clone()).collect();
        let reads=futures_util::stream::iter(ids).map(|id|async move {
            let state=rpc.call("thread/read",serde_json::json!({"threadId":id,"includeTurns":false})).await;
            (id,state)
        }).buffer_unordered(4);
        tokio::pin!(reads);
        let _=tokio::time::timeout(std::time::Duration::from_secs(2),async {
            while let Some((id,state))=reads.next().await {
                if let Ok(state)=state { self.thread_status(&id,&state["thread"]["status"]); }
            }
        }).await;
    }
    fn thread_status(&mut self,id:&str,status:&Value) {
        match status["type"].as_str() {
            Some("active")=>{self.0.insert(id.into(),true);}
            Some("idle"|"notLoaded"|"systemError")=>{self.0.insert(id.into(),false);}
            _=>{}
        }
    }
    pub(crate) fn active(&self) -> usize { self.0.values().filter(|active| **active).count() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn resumed_history_does_not_keep_unloaded_children_active() {
        let mut activity=Activity::default();
        activity.item(&json!({"type":"subAgentActivity","agentThreadId":"old","kind":"started"}));
        activity.thread_status("old",&json!({"type":"notLoaded"}));
        assert_eq!(activity.active(),0);
        activity.thread_status("old",&json!({"type":"active"}));
        assert_eq!(activity.active(),1);
    }
    #[test]
    fn latest_state_counts_each_child_once_and_interactions_do_not_restart_it() {
        let mut activity = Activity::default();
        activity.snapshot(&json!({"turns":[{"items":[{"type":"collabAgentToolCall","agentsStates":{"a":{"status":"running"},"b":{"status":"pendingInit"}}}]}]}));
        assert_eq!(activity.active(),2);
        for _ in 0..2 { activity.item(&json!({"type":"subAgentActivity","agentThreadId":"a","kind":"started"})); }
        assert_eq!(activity.active(),2);
        activity.item(&json!({"type":"subAgentActivity","agentThreadId":"a","kind":"completed"}));
        activity.item(&json!({"type":"subAgentActivity","agentThreadId":"a","kind":"interacted"}));
        assert_eq!(activity.active(),1);
        activity.item(&json!({"type":"collabAgentToolCall","agentsStates":{"b":{"status":"errored"}}}));
        assert_eq!(activity.active(),0);
        activity.item(&json!({"type":"subAgentActivity","agentThreadId":"a","kind":"started"}));
        assert_eq!(activity.active(),1);
    }
}
