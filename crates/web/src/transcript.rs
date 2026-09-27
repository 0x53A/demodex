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
    positions: BTreeMap<String, usize>,
    len: usize,
    cursor: i64,
}

impl Transcript {
    fn put(&mut self, item: &Value) {
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
    }

    fn finish_turn(&mut self, turn_id: &str) {
        // A lost completion event is not evidence that the tool succeeded, or
        // that a remote process stopped. Do not leave historical work "Running".
        let unfinished: Vec<_> = self
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .filter(|item| {
                item["_demodexLifecycle"] == "running"
                    && (text(item, "_demodexTurnId").is_empty()
                        || text(item, "_demodexTurnId") == turn_id)
            })
            .map(|item| {
                let mut item = item.as_ref().clone();
                item["_demodexLifecycle"] = json!("ended");
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
            let params = &message["params"];
            match text(message, "method") {
                "demodex/threadSnapshot" => {
                    if let Some(turns) = params["thread"]["turns"].as_array() {
                        for turn in turns {
                            if let Some(items) = turn["items"].as_array() {
                                for item in items {
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
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
