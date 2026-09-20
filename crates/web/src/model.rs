use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub url: String,
    pub token: String,
}

// Browser history contains navigation only, never credentials or drafts.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Navigation {
    pub host: String,
    pub selected: String,
    pub page: String,
    pub connections: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub host: String,
    pub selected: String,
    pub page: String,
    pub drafts: BTreeMap<String, String>,
    pub answers: BTreeMap<String, String>,
    pub fields: BTreeMap<String, String>,
    pub receipts: BTreeMap<String, String>,
}

impl Saved {
    pub fn legacy(value: Value, host: String) -> Self {
        let mut saved = Self {
            host,
            selected: text(&value, "selected").into(),
            ..Self::default()
        };
        saved.page = if value["environmentView"] == true {
            "environments"
        } else if value["adding"] == true {
            "external"
        } else {
            ""
        }
        .into();
        if let Some(drafts) = value["drafts"].as_object() {
            for (id, draft) in drafts {
                if let Some(draft) = draft.as_str() {
                    saved
                        .drafts
                        .insert(format!("{}:{id}", saved.host), draft.into());
                }
            }
        }
        if let Some(answers) = value["answers"].as_object() {
            for (key, answer) in answers {
                if let Some(answer) = answer.as_str() {
                    saved
                        .answers
                        .insert(format!("{}:{key}", saved.host), answer.into());
                }
            }
        }
        if let Some(raw) = value["raw"].as_object() {
            for (key, answer) in raw {
                if let Some(answer) = answer.as_str() {
                    saved
                        .answers
                        .insert(format!("{}:{key}:raw", saved.host), answer.into());
                }
            }
        }
        for (old, new) in [
            ("hostSessionName", "session_name"),
            ("hostThread", "thread_id"),
            ("hostCwd", "cwd"),
            ("newSandbox", "sandbox"),
            ("name", "external_name"),
            ("endpoint", "endpoint"),
            ("thread", "external_thread"),
            ("environmentName", "environment_name"),
            ("memory", "memory"),
            ("cpus", "cpus"),
            ("internet", "internet"),
        ] {
            if let Some(value) = value.get(old) {
                saved.fields.insert(
                    new.into(),
                    value
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
        }
        if value["targets"].is_array() {
            saved
                .fields
                .insert("targets".into(), value["targets"].to_string());
        }
        saved
    }
    pub fn answer(&self, key: &str, question: &str) -> String {
        self.answers
            .get(&format!("{}:{key}:{question}", self.host))
            .or_else(|| self.answers.get(&format!("{}:{key}{question}", self.host)))
            .cloned()
            .unwrap_or_default()
    }
    pub fn key(&self) -> String {
        format!("{}:{}", self.host, self.selected)
    }
    pub fn draft(&self) -> String {
        self.drafts.get(&self.key()).cloned().unwrap_or_default()
    }
    pub fn field(&self, name: &str) -> String {
        self.fields.get(name).cloned().unwrap_or_default()
    }
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
pub fn array(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}
pub fn active(status: &str) -> bool {
    matches!(status, "working" | "active" | "running" | "waiting")
}
pub fn sandbox_name(value: &str) -> &str {
    match value {
        "readOnly" => "Read-only",
        "workspaceWrite" => "Workspace-write",
        "dangerFullAccess" => "Danger-full-access",
        "externalSandbox" => "External sandbox",
        "" => "Not connected",
        v => v,
    }
}

/// Deterministic transcript projection: replay and duplicate delivery are harmless.
pub fn transcript(events: &[Value]) -> Vec<Value> {
    let mut items = Vec::<Value>::new();
    let mut positions = BTreeMap::<String, usize>::new();
    fn put(items: &mut Vec<Value>, positions: &mut BTreeMap<String, usize>, item: Value) {
        let id = text(&item, "id").to_owned();
        if id.is_empty() {
            return;
        }
        if let Some(index) = positions.get(&id) {
            items[*index] = item;
        } else {
            positions.insert(id, items.len());
            items.push(item);
        }
    }
    for event in events {
        let message = &event["message"];
        let params = &message["params"];
        match text(message, "method") {
            "demodex/threadSnapshot" => {
                for turn in array(&params["thread"]["turns"]) {
                    for item in array(&turn["items"]) {
                        put(&mut items, &mut positions, item);
                    }
                }
            }
            "item/started" | "item/completed" => {
                put(&mut items, &mut positions, params["item"].clone())
            }
            "item/agentMessage/delta" | "item/commandExecution/outputDelta" => {
                let id = text(params, "itemId");
                let output = text(message, "method") == "item/commandExecution/outputDelta";
                let mut item = positions.get(id).map(|index|items[*index].clone()).unwrap_or_else(||json!({"id":id,"type":if output {"commandExecution"} else {"agentMessage"}}));
                let field = if output { "aggregatedOutput" } else { "text" };
                item[field] = json!(format!("{}{}", text(&item, field), text(params, "delta")));
                put(&mut items, &mut positions, item);
            }
            _ => {}
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_retains_unsent_input() {
        let saved = Saved::legacy(
            json!({"selected":"session","drafts":{"session":"draft"},"answers":{"requestchoice":"explicit"},"hostCwd":"/work"}),
            "https://host".into(),
        );
        assert_eq!(saved.draft(), "draft");
        assert_eq!(saved.answer("request", "choice"), "explicit");
        assert_eq!(saved.field("cwd"), "/work");
    }
    #[test]
    fn completed_items_replace_streamed_text() {
        let events = vec![
            json!({"message":{"method":"item/agentMessage/delta","params":{"itemId":"one","delta":"hel"}}}),
            json!({"message":{"method":"item/completed","params":{"item":{"id":"one","type":"agentMessage","text":"hello"}}}}),
        ];
        let items = transcript(&events);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "hello");
    }
}
