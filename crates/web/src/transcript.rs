//! Append-only event projection with copy-on-write render chunks.
//! Compaction and resume snapshots merge by item ID; they never erase older turns.
use crate::model::text;
use serde_json::{Value, json};
use std::{collections::BTreeMap, rc::Rc};

pub const CHUNK_SIZE: usize = 64;
pub type Chunk = Rc<Vec<Rc<Value>>>;
pub type Chunks = Rc<Vec<Chunk>>;

#[derive(Default)]
pub struct Transcript {
    pub chunks: Chunks,
    pub groups: Chunks,
    group_positions: BTreeMap<String, (usize, usize)>,
    positions: BTreeMap<String, usize>,
    len: usize,
    cursor: i64,
    event_at: String,
    event_imported: bool,
}

pub fn is_activity(item: &Value) -> bool {
    matches!(text(item,"type"), "reasoning" | "commandExecution" | "fileChange" | "webSearch" | "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall" | "subAgentActivity" | "imageView" | "imageGeneration" | "sleep")
}

fn steering_text(item: &Value) -> String {
    item["content"].as_array().map(|content| content.iter().filter(|part| part["type"] == "text").map(|part| text(part,"text")).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

impl Transcript {
    fn group_item(&mut self, item: Rc<Value>) {
        let id = text(&item, "id").to_owned();
        if let Some(&(group, index)) = self.group_positions.get(&id) {
            let old = &self.groups[group][index];
            if is_activity(old) != is_activity(&item) || old["_demodexTurnId"] != item["_demodexTurnId"] {
                self.groups = Rc::default();
                self.group_positions.clear();
                let items: Vec<_> = self.chunks.iter().flat_map(|c| c.iter().cloned()).collect();
                for item in items { self.group_item(item); }
            } else {
                Rc::make_mut(&mut Rc::make_mut(&mut self.groups)[group])[index] = item;
            }
            return;
        }
        let groups = Rc::make_mut(&mut self.groups);
        let append = groups.last().and_then(|g| g.last()).is_some_and(|last|
            is_activity(last) && is_activity(&item) && last["_demodexTurnId"] == item["_demodexTurnId"]);
        if !append { groups.push(Rc::new(Vec::new())); }
        let group = groups.len()-1;
        let items = Rc::make_mut(&mut groups[group]);
        self.group_positions.insert(id, (group, items.len()));
        items.push(item);
    }

    fn remove(&mut self, id: &str) {
        if !self.positions.contains_key(id) { return; }
        let items: Vec<_> = self.chunks.iter().flat_map(|c| c.iter()).filter(|item| text(item, "id") != id).cloned().collect();
        self.chunks = Rc::default();
        self.groups = Rc::default();
        self.positions.clear();
        self.group_positions.clear();
        self.len = 0;
        for item in items { self.put(&item); }
    }

    fn consume_steering(&mut self, item: &Value, turn_id: &str) {
        if item["type"] != "userMessage" { return; }
        if self.positions.get(text(item,"id")).is_some_and(|&i| self.chunks[i / CHUNK_SIZE][i % CHUNK_SIZE]["_demodexSteering"].is_null()) { return; }
        let pending = self.chunks.iter().flat_map(|c| c.iter()).find(|pending| {
            pending["_demodexSteering"].is_string()
                && (text(pending, "id") == text(item, "id")
                    || text(pending, "id") == text(item, "clientId")
                    || (pending["_demodexSteering"] == "waiting" && text(item,"clientId").is_empty() && text(pending, "_demodexTurnId") == turn_id && steering_text(pending) == steering_text(item)))
        }).map(|pending| (text(pending, "id").to_owned(), pending["_demodexAt"].clone(), pending["_demodexTimeSource"].clone()));
        if let Some((id, at, source)) = pending {
            self.remove(&id);
            let mut confirmed = item.clone();
            confirmed["_demodexAt"] = at;
            confirmed["_demodexTimeSource"] = source;
            self.put(&confirmed);
        }
    }

    fn put(&mut self, item: &Value) {
        let mut item = item.clone();
        // Preserve the first recorded time through streaming, completion and
        // later resume snapshots. Imported history has no per-item Codex time.
        if let Some(&index) = self.positions.get(text(&item,"id")) {
            let old = &self.chunks[index / CHUNK_SIZE][index % CHUNK_SIZE];
            for field in ["_demodexAt", "_demodexTimeSource", "_demodexFilesRevision"] {
                if !old[field].is_null() && (field != "_demodexFilesRevision" || item[field].is_null()) { item[field] = old[field].clone(); }
            }
        }
        if item["_demodexAt"].is_null() && !self.event_at.is_empty() && item.is_object() {
            item["_demodexAt"] = json!(self.event_at);
            item["_demodexTimeSource"] = json!(if self.event_imported {"imported"}else{"recorded"});
        }
        let item = &item;
        let id = text(item, "id");
        if id.is_empty() {
            return;
        }
        if let Some(&index) = self.positions.get(id) {
            if self.chunks[index / CHUNK_SIZE][index % CHUNK_SIZE].as_ref() == item {
                return;
            }
            let chunks = Rc::make_mut(&mut self.chunks);
            Rc::make_mut(&mut chunks[index / CHUNK_SIZE])[index % CHUNK_SIZE] =
                Rc::new(item.clone());
        } else {
            self.positions.insert(id.into(), self.len);
            let chunks = Rc::make_mut(&mut self.chunks);
            if self.len.is_multiple_of(CHUNK_SIZE) {
                chunks.push(Rc::new(Vec::new()));
            }
            Rc::make_mut(chunks.last_mut().unwrap()).push(Rc::new(item.clone()));
            self.len += 1;
        }
        let index = self.positions[id];
        self.group_item(self.chunks[index / CHUNK_SIZE][index % CHUNK_SIZE].clone());
    }

    fn finish_turn(&mut self, turn_id: &str) {
        // A lost completion event is not evidence that the tool succeeded, or
        // that a remote process stopped. Do not leave historical work "Running".
        let unfinished: Vec<_> = self
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .filter(|item| {
                (item["_demodexLifecycle"] == "running" || item["_demodexSteering"] == "waiting")
                    && (text(item, "_demodexTurnId").is_empty()
                        || text(item, "_demodexTurnId") == turn_id)
            })
            .map(|item| {
                let mut item = item.as_ref().clone();
                item["_demodexLifecycle"] = json!("ended");
                if item["_demodexSteering"] == "waiting" {
                    item["_demodexSteering"] = json!("unconfirmed");
                }
                item
            })
            .collect();
        for item in unfinished {
            self.put(&item);
        }
    }

    pub fn append(&mut self, events: &[Value]) {
        for event in events {
            if let Some(seq) = event["seq"].as_i64() {
                if seq <= self.cursor {
                    continue;
                }
                self.cursor = seq;
            }
            let message = &event["message"];
            self.event_at = text(event,"at").to_owned();
            self.event_imported = text(message,"method") == "demodex/threadSnapshot";
            let params = &message["params"];
            match text(message, "method") {
                "demodex/messageFiles" => {
                    if let Some(&i) = self.positions.get(text(params,"itemId")) {
                        let mut item=self.chunks[i/CHUNK_SIZE][i%CHUNK_SIZE].as_ref().clone();
                        item["_demodexFilesRevision"] = json!(self.cursor);
                        self.put(&item);
                    }
                }
                "demodex/notification" => {
                    self.put(&json!({"id":format!("demodex:notification:{}",text(params,"id")),"type":"demodexNotification","title":params["title"],"text":params["message"]}));
                }
                "demodex/notificationDelivery" => {
                    let id=format!("demodex:notification:{}",text(params,"id"));
                    if let Some(&i)=self.positions.get(&id) {
                        let mut item=self.chunks[i/CHUNK_SIZE][i%CHUNK_SIZE].as_ref().clone();
                        item["delivery"]=params.clone();
                        self.put(&item);
                    }
                }
                "demodex/promptSteering" => {
                    self.put(&json!({"id":params["clientUserMessageId"],"type":"userMessage","content":[{"type":"text","text":params["text"]}],"_demodexTurnId":params["turnId"],"_demodexSteering":"waiting"}));
                }
                "demodex/promptSteeringDiscarded" => {
                    self.remove(text(params, "clientUserMessageId"));
                }
                "demodex/promptSteeringFailed" => {
                    if let Some(&i) = self.positions.get(text(params, "clientUserMessageId")) {
                        let mut item = self.chunks[i / CHUNK_SIZE][i % CHUNK_SIZE].as_ref().clone();
                        item["_demodexSteering"] = json!("failed");
                        self.put(&item);
                    }
                }
                "demodex/threadSnapshot" => {
                    if let Some(turns) = params["thread"]["turns"].as_array() {
                        for turn in turns {
                            if let Some(items) = turn["items"].as_array() {
                                for item in items {
                                    self.consume_steering(item, text(turn, "id"));
                                    let mut item = item.clone();
                                    if item.is_object()
                                        && let Some(&i) = self.positions.get(text(&item, "id"))
                                    {
                                        let old = &self.chunks[i / CHUNK_SIZE][i % CHUNK_SIZE];
                                        for field in [
                                            "_demodexLifecycle",
                                            "_demodexProgress",
                                            "_demodexTurnId",
                                        ] {
                                            if !old[field].is_null() {
                                                item[field] = old[field].clone();
                                            }
                                        }
                                    }
                                    self.put(&item);
                                }
                            }
                        }
                    }
                }
                "item/started" | "item/completed" => {
                    let mut item = params["item"].clone();
                    if item.is_object() {
                        self.consume_steering(&item, text(params, "turnId"));
                        if !text(params, "turnId").is_empty() {
                            item["_demodexTurnId"] = params["turnId"].clone();
                        }
                        item["_demodexLifecycle"] =
                            json!(if text(message, "method") == "item/started" {
                                "running"
                            } else {
                                "completed"
                            });
                        self.put(&item);
                    }
                }
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    let id = text(params, "itemId");
                    let summary = text(message, "method") == "item/reasoning/summaryTextDelta";
                    let field = if summary { "summary" } else { "content" };
                    let index_field = if summary {
                        "summaryIndex"
                    } else {
                        "contentIndex"
                    };
                    let Some(index) = params[index_field].as_u64().filter(|i| *i < 4096) else {
                        continue;
                    };
                    if id.is_empty() {
                        continue;
                    }
                    let mut item = self
                        .positions
                        .get(id)
                        .map(|&i| self.chunks[i / CHUNK_SIZE][i % CHUNK_SIZE].as_ref().clone())
                        .unwrap_or_else(|| json!({"id":id,"type":"reasoning"}));
                    if !item[field].is_array() {
                        item[field] = json!([]);
                    }
                    let parts = item[field].as_array_mut().unwrap();
                    parts.resize(parts.len().max(index as usize + 1), json!(""));
                    let mut part = parts[index as usize]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    part.push_str(text(params, "delta"));
                    parts[index as usize] = json!(part);
                    self.put(&item);
                }
                "item/mcpToolCall/progress" => {
                    let id = text(params, "itemId");
                    if let Some(&i) = self.positions.get(id) {
                        let mut item = self.chunks[i / CHUNK_SIZE][i % CHUNK_SIZE].as_ref().clone();
                        item["_demodexProgress"] = params["message"].clone();
                        self.put(&item);
                    }
                }
                "turn/plan/updated" => {
                    self.put(&json!({"id":format!("demodex:plan:{}",text(params,"turnId")),"type":"demodexPlan","plan":params["plan"],"text":params["explanation"]}));
                }
                "error" => {
                    self.put(&json!({"id":format!("demodex:error:{}",self.cursor),"type":"demodexError","text":params["error"]["message"],"retrying":params["willRetry"]}));
                }
                "turn/completed" => {
                    self.finish_turn(text(&params["turn"], "id"));
                    if matches!(text(&params["turn"], "status"), "failed" | "interrupted") {
                        self.put(&json!({"id":format!("demodex:turn:{}",text(&params["turn"],"id")),"type":"demodexTurnEnd","status":params["turn"]["status"],"text":params["turn"]["error"]["message"]}));
                    }
                }
                "item/agentMessage/delta"
                | "item/commandExecution/outputDelta"
                | "item/plan/delta" => {
                    let id = text(params, "itemId");
                    let delta = text(params, "delta");
                    if id.is_empty() || delta.is_empty() {
                        continue;
                    }
                    let output = text(message, "method") == "item/commandExecution/outputDelta";
                    let field = if output { "aggregatedOutput" } else { "text" };
                    if !self.positions.contains_key(id) {
                        self.put(&json!({"id":id,"type":if output {"commandExecution"} else if text(message,"method")=="item/plan/delta" {"plan"} else {"agentMessage"}}));
                    }
                    let index = self.positions[id];
                    let chunks = Rc::make_mut(&mut self.chunks);
                    let chunk = Rc::make_mut(&mut chunks[index / CHUNK_SIZE]);
                    let item = Rc::make_mut(&mut chunk[index % CHUNK_SIZE]);
                    if !item[field].is_string() {
                        item[field] = json!("");
                    }
                    if let Value::String(value) = &mut item[field] {
                        value.push_str(delta);
                    }
                    let updated = chunk[index % CHUNK_SIZE].clone();
                    self.group_item(updated);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn streamed_deltas_update_the_render_groups_without_touching_old_groups() {
        let mut transcript=Transcript::default();
        transcript.append(&[event(1,"item/completed",json!({"item":{"id":"old","type":"agentMessage","text":"Earlier"}}))]);
        let old=transcript.groups[0].clone();
        transcript.append(&[
            event(2,"item/agentMessage/delta",json!({"itemId":"live","delta":"Hello "})),
            event(3,"item/agentMessage/delta",json!({"itemId":"live","delta":"world"})),
            event(4,"item/commandExecution/outputDelta",json!({"itemId":"command","delta":"output"})),
        ]);
        assert_eq!(transcript.groups[1][0]["text"],"Hello world");
        assert_eq!(transcript.groups[2][0]["aggregatedOutput"],"output");
        assert!(Rc::ptr_eq(&old,&transcript.groups[0]));
    }

    #[test]
    fn notifications_stay_visible_and_delivery_updates_the_same_item() {
        let mut transcript=Transcript::default();
        transcript.append(&[event(1,"demodex/notification",json!({"id":"n","title":"Ready","message":"Please review"}))]);
        assert_eq!(transcript.groups.len(),1);
        assert!(!is_activity(&transcript.groups[0][0]));
        transcript.append(&[event(2,"demodex/notificationDelivery",json!({"id":"n","accepted":1,"failed":0,"state":"complete"}))]);
        assert_eq!(transcript.len,1);
        assert_eq!(transcript.groups[0][0]["text"],"Please review");
        assert_eq!(transcript.groups[0][0]["delivery"]["accepted"],1);
    }

    #[test]
    fn activity_groups_cross_chunks_and_preserve_unmodified_identity() {
        let mut transcript=super::Transcript::default();
        transcript.put(&serde_json::json!({"id":"text","type":"agentMessage","text":"Working"}));
        for n in 0..130 { transcript.put(&serde_json::json!({"id":format!("tool-{n}"),"type":"commandExecution","status":"inProgress"})); }
        assert_eq!(transcript.groups.len(),2);
        assert_eq!(transcript.groups[1].len(),130);
        let first=transcript.groups[0].clone();
        let first_tool=transcript.groups[1][0].clone();
        transcript.put(&serde_json::json!({"id":"tool-129","type":"commandExecution","status":"completed"}));
        assert!(std::rc::Rc::ptr_eq(&first,&transcript.groups[0]));
        assert!(std::rc::Rc::ptr_eq(&first_tool,&transcript.groups[1][0]));
        transcript.put(&serde_json::json!({"id":"after","type":"agentMessage","text":"Done"}));
        transcript.put(&serde_json::json!({"id":"last","type":"fileChange"}));
        assert_eq!(transcript.groups.len(),4);
        transcript.put(&serde_json::json!({"id":"last","type":"agentMessage","text":"Corrected snapshot"}));
        assert!(!super::is_activity(&transcript.groups[3][0]));
    }

    use super::*;
    #[test]
    fn message_time_survives_deltas_completion_and_resume_and_labels_imports() {
        let mut transcript=Transcript::default();
        transcript.append(&[
            json!({"seq":1,"at":"2026-09-28T10:00:00Z","message":{"method":"item/started","params":{"item":{"id":"live","type":"agentMessage","text":""}}}}),
            json!({"seq":2,"at":"2026-09-28T10:01:00Z","message":{"method":"item/agentMessage/delta","params":{"itemId":"live","delta":"Hello"}}}),
            json!({"seq":3,"at":"2026-09-28T10:02:00Z","message":{"method":"item/completed","params":{"item":{"id":"live","type":"agentMessage","text":"Hello"}}}}),
            json!({"seq":4,"at":"2026-09-28T10:03:00Z","message":{"method":"demodex/threadSnapshot","params":{"thread":{"turns":[{"items":[{"id":"live","type":"agentMessage","text":"Hello"},{"id":"old","type":"agentMessage","text":"History"}]}]}}}}),
        ]);
        assert_eq!(transcript.chunks[0][0]["_demodexAt"],"2026-09-28T10:00:00Z");
        assert_eq!(transcript.chunks[0][0]["_demodexTimeSource"],"recorded");
        assert_eq!(transcript.chunks[0][1]["_demodexTimeSource"],"imported");
        assert_eq!(transcript.chunks[0][1]["_demodexAt"],"2026-09-28T10:03:00Z");
    }

    #[test]
    fn confirmed_steering_preserves_submission_time_under_codex_item_id() {
        let mut transcript=Transcript::default();
        transcript.append(&[
            json!({"seq":1,"at":"2026-09-28T10:00:00Z","message":{"method":"demodex/promptSteering","params":{"clientUserMessageId":"client","turnId":"turn","text":"Steer"}}}),
            json!({"seq":2,"at":"2026-09-28T10:02:00Z","message":{"method":"item/started","params":{"turnId":"turn","item":{"id":"codex","clientId":"client","type":"userMessage","content":[{"type":"text","text":"Steer"}]}}}}),
        ]);
        assert_eq!(transcript.len,1);
        assert_eq!(transcript.chunks[0][0]["id"],"codex");
        assert_eq!(transcript.chunks[0][0]["_demodexAt"],"2026-09-28T10:00:00Z");
    }

    #[test]
    fn steering_stays_visible_until_consumed_and_survives_event_pages() {
        let mut transcript = Transcript::default();
        let pending = event(1,"demodex/promptSteering",json!({"clientUserMessageId":"client","text":"Please adjust","turnId":"turn"}));
        transcript.append(&[pending.clone()]);
        assert_eq!(transcript.chunks[0][0]["_demodexSteering"], "waiting");
        transcript.append(&[event(2,"item/started",json!({"turnId":"turn","item":{"id":"tool","type":"commandExecution"}}))]);
        assert_eq!(transcript.chunks[0][0]["_demodexSteering"], "waiting");
        let consumed = event(3,"item/started",json!({"turnId":"turn","item":{"id":"server","type":"userMessage","content":[{"type":"text","text":"Please adjust","text_elements":[]}]}}));
        transcript.append(&[consumed.clone()]);
        assert_eq!(transcript.len, 2);
        assert!(!transcript.positions.contains_key("client"));
        assert!(transcript.positions.contains_key("server"));
        let mut replay = Transcript::default();
        replay.append(&[pending, consumed]);
        assert_eq!(replay.len,1);
        assert!(replay.chunks[0][0]["_demodexSteering"].is_null());
    }

    #[test]
    fn steering_client_ids_distinguish_identical_messages_and_late_confirmation() {
        let mut transcript = Transcript::default();
        transcript.append(&[
            event(1,"demodex/promptSteering",json!({"clientUserMessageId":"one","text":"Same","turnId":"turn"})),
            event(2,"demodex/promptSteering",json!({"clientUserMessageId":"two","text":"Same","turnId":"turn"})),
            event(3,"item/started",json!({"turnId":"turn","item":{"id":"server","clientId":"two","type":"userMessage","content":[{"type":"text","text":"Same"}]}})),
            event(4,"item/completed",json!({"turnId":"turn","item":{"id":"server","clientId":"two","type":"userMessage","content":[{"type":"text","text":"Same"}]}})),
        ]);
        assert!(transcript.positions.contains_key("one"));
        assert!(!transcript.positions.contains_key("two"));
        assert_eq!(transcript.len,2);
        transcript.append(&[event(5,"turn/completed",json!({"turn":{"id":"turn","status":"completed"}}))]);
        assert_eq!(transcript.chunks[0][0]["_demodexSteering"],"unconfirmed");
        transcript.append(&[event(6,"demodex/threadSnapshot",json!({"thread":{"turns":[{"id":"turn","items":[{"id":"late","clientId":"one","type":"userMessage","content":[{"type":"text","text":"Same"}]}]}]}}))]);
        assert!(!transcript.positions.contains_key("one"));
        assert_eq!(transcript.len,2);
    }

    #[test]
    fn steering_failure_and_fallback_are_explicit() {
        let mut transcript = Transcript::default();
        transcript.append(&[
            event(1,"demodex/promptSteering",json!({"clientUserMessageId":"client","text":"Adjust","turnId":"turn"})),
            event(2,"demodex/promptSteeringFailed",json!({"clientUserMessageId":"client"})),
        ]);
        assert_eq!(transcript.chunks[0][0]["_demodexSteering"], "failed");
        transcript.append(&[event(3,"demodex/promptSteeringDiscarded",json!({"clientUserMessageId":"client"}))]);
        assert_eq!(transcript.len,0);
    }

    fn event(seq: i64, method: &str, params: Value) -> Value {
        json!({"seq":seq,"message":{"method":method,"params":params}})
    }
    #[test]
    fn progress_plans_and_errors_survive_paging_and_completion() {
        let events = vec![
            event(
                1,
                "item/started",
                json!({"item":{"id":"r","type":"reasoning","summary":[]}}),
            ),
            event(
                2,
                "item/reasoning/summaryTextDelta",
                json!({"itemId":"r","summaryIndex":1,"delta":"Second"}),
            ),
            event(
                3,
                "item/reasoning/summaryTextDelta",
                json!({"itemId":"r","summaryIndex":0,"delta":"First"}),
            ),
            event(
                4,
                "item/reasoning/summaryTextDelta",
                json!({"itemId":"r","summaryIndex":0,"delta":" summary"}),
            ),
            event(
                5,
                "item/plan/delta",
                json!({"itemId":"p","delta":"Proposed plan"}),
            ),
            event(
                6,
                "turn/plan/updated",
                json!({"turnId":"t","plan":[{"step":"Build","status":"inProgress"}]}),
            ),
            event(
                7,
                "item/started",
                json!({"item":{"id":"m","type":"mcpToolCall","status":"inProgress"}}),
            ),
            event(
                8,
                "item/mcpToolCall/progress",
                json!({"itemId":"m","message":"Reading files"}),
            ),
            event(
                9,
                "error",
                json!({"error":{"message":"Connection interrupted"},"willRetry":true}),
            ),
            event(
                10,
                "turn/plan/updated",
                json!({"turnId":"t","plan":[{"step":"Build","status":"completed"}]}),
            ),
        ];
        let mut whole = Transcript::default();
        whole.append(&events);
        assert_eq!(whole.len, 5);
        assert_eq!(
            whole.chunks[0][0]["summary"],
            json!(["First summary", "Second"])
        );
        assert_eq!(whole.chunks[0][1]["type"], "plan");
        assert_eq!(whole.chunks[0][2]["plan"][0]["status"], "completed");
        assert_eq!(whole.chunks[0][3]["_demodexProgress"], "Reading files");
        assert_eq!(whole.chunks[0][4]["retrying"], true);
        for size in [1, 3, 7] {
            let mut paged = Transcript::default();
            for page in events.chunks(size) {
                paged.append(page);
            }
            assert_eq!(whole.chunks, paged.chunks);
        }
        whole.append(&[event(
            11,
            "item/completed",
            json!({"item":{"id":"r","type":"reasoning","summary":["Final summary"]}}),
        )]);
        assert_eq!(whole.chunks[0][0]["summary"], json!(["Final summary"]));
        whole.append(&[event(
            12,
            "turn/completed",
            json!({"turn":{"id":"t","status":"failed","error":{"message":"Cannot continue"}}}),
        )]);
        assert_eq!(whole.chunks[0].last().unwrap()["text"], "Cannot continue");
        assert_eq!(whole.chunks[0][3]["_demodexLifecycle"], "ended");
        assert_eq!(whole.chunks[0][0]["_demodexLifecycle"], "completed");
    }

    #[test]
    fn streaming_only_changes_its_chunk_and_deduplicates_delivery() {
        let mut transcript = Transcript::default();
        let events: Vec<_> = (0..200).map(|n| event(n+1,"item/completed",
            json!({"item":{"id":format!("item-{n}"),"type":"agentMessage","text":"original"}}))).collect();
        transcript.append(&events);
        let old = transcript.chunks.clone();
        let delta = event(
            201,
            "item/agentMessage/delta",
            json!({"itemId":"item-199","delta":" tail"}),
        );
        transcript.append(&[delta.clone(), delta]);
        assert!(Rc::ptr_eq(&old[0], &transcript.chunks[0]));
        assert!(Rc::ptr_eq(&old[2], &transcript.chunks[2]));
        assert!(!Rc::ptr_eq(&old[3], &transcript.chunks[3]));
        assert!(Rc::ptr_eq(&old[3][0], &transcript.chunks[3][0]));
        assert_eq!(transcript.chunks[3][7]["text"], "original tail");
        assert_eq!(old[3][7]["text"], "original");
        transcript.append(&[event(
            202,
            "item/completed",
            json!({"item":{"id":"item-199","type":"agentMessage","text":"final"}}),
        )]);
        assert_eq!(transcript.chunks[3][7]["text"], "final");
    }

    #[test]
    fn chunk_boundaries_and_arbitrary_event_pages_have_the_same_projection() {
        let mut events = Vec::new();
        for n in 0..200 {
            events.push(event(
                events.len() as i64 + 1,
                "item/agentMessage/delta",
                json!({"itemId":format!("item-{n}"),"delta":"partial"}),
            ));
            events.push(event(
                events.len() as i64 + 1,
                "item/completed",
                json!({"item":{"id":format!("item-{n}"),"type":"agentMessage","text":"final"}}),
            ));
        }
        let mut whole = Transcript::default();
        whole.append(&events);
        for page_size in [1, 3, 63, 64, 65, 137] {
            let mut paged = Transcript::default();
            for page in events.chunks(page_size) {
                paged.append(page);
            }
            assert_eq!(paged.chunks, whole.chunks);
        }
        let old = whole.chunks.clone();
        whole.append(&[event(
            401,
            "item/completed",
            json!({"item":{"id":"item-0","type":"agentMessage","text":"corrected"}}),
        )]);
        assert!(!Rc::ptr_eq(&old[0], &whole.chunks[0]));
        for index in 1..old.len() {
            assert!(Rc::ptr_eq(&old[index], &whole.chunks[index]));
        }
    }

    #[test]
    fn snapshots_across_compactions_keep_old_items_and_update_in_place() {
        let mut transcript = Transcript::default();
        transcript.append(&[
            event(1,"item/completed",json!({"item":{"id":"day-one","type":"agentMessage","text":"old"}})),
            event(2,"item/completed",json!({"item":{"id":"compact","type":"contextCompaction"}})),
            event(3,"demodex/threadSnapshot",json!({"thread":{"turns":[{"items":[
                {"id":"compact","type":"contextCompaction"},
                {"id":"day-two","type":"agentMessage","text":"new"}
            ]}]}})),
            event(4,"item/commandExecution/outputDelta",json!({"itemId":"command","delta":"partial"})),
            event(5,"item/completed",json!({"item":{"id":"command","type":"commandExecution","command":"pwd","aggregatedOutput":"complete"}})),
        ]);
        assert_eq!(transcript.len, 4);
        assert_eq!(transcript.chunks[0][0]["text"], "old");
        assert_eq!(transcript.chunks[0][3]["aggregatedOutput"], "complete");
        let unchanged = transcript.chunks.clone();
        transcript.append(&[event(6, "thread/tokenUsage/updated", json!({}))]);
        assert!(Rc::ptr_eq(&unchanged, &transcript.chunks));
        transcript.append(&[event(
            7,
            "demodex/threadSnapshot",
            json!({"thread":{"turns":[{"items":[
                {"id":"day-two","type":"agentMessage","text":"new"}
            ]}]}}),
        )]);
        assert!(Rc::ptr_eq(&unchanged, &transcript.chunks));
    }
}
