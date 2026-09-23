use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
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
    pub host_fields: BTreeMap<String, BTreeMap<String, String>>,
}

impl Saved {
    /// Keep the current host's legacy fields in place and stash other hosts'
    /// drafts separately. Old saved views deserialize without losing their fields.
    pub fn switch_host(&mut self, host: String) {
        if self.host == host { return; }
        self.host_fields.insert(self.host.clone(), std::mem::take(&mut self.fields));
        self.fields = self.host_fields.remove(&host).unwrap_or_default();
        self.host = host;
        self.separate_creation_fields();
    }

    pub fn separate_creation_fields(&mut self) {
        // The old create/resume widget shared these fields. Copy them once so
        // either interpretation of an existing draft remains recoverable.
        if !self.fields.contains_key("new_session_name") {
            self.fields.insert("new_session_name".into(), self.field("session_name"));
        }
        if !self.fields.contains_key("new_sandbox") {
            self.fields.insert("new_sandbox".into(), self.field("sandbox"));
        }
    }

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
        "" => "Unconfirmed",
        v => v,
    }
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
    fn server_switch_preserves_distinct_setup_and_control_drafts() {
        let mut saved: Saved = serde_json::from_value(json!({
            "host":"https://one",
            "fields":{"session_name":"Existing draft","sandbox":"read-only","control:s:objective":"First goal"}
        })).unwrap();
        saved.separate_creation_fields();
        saved.fields.insert("new_session_name".into(), "New draft".into());
        saved.switch_host("https://two".into());
        assert_eq!(saved.field("control:s:objective"), "");
        saved.fields.insert("control:s:objective".into(), "Second goal".into());
        // Exercise persistence while the other host's fields are stashed.
        let mut saved: Saved = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        saved.switch_host("https://one".into());
        assert_eq!(saved.field("new_session_name"), "New draft");
        assert_eq!(saved.field("session_name"), "Existing draft");
        assert_eq!(saved.field("new_sandbox"), "read-only");
        assert_eq!(saved.field("control:s:objective"), "First goal");
        saved.switch_host("https://two".into());
        assert_eq!(saved.field("control:s:objective"), "Second goal");
    }
    #[test]
    fn completed_items_replace_streamed_text() {
        let events = vec![
            json!({"message":{"method":"item/agentMessage/delta","params":{"itemId":"one","delta":"hel"}}}),
            json!({"message":{"method":"item/completed","params":{"item":{"id":"one","type":"agentMessage","text":"hello"}}}}),
        ];
        let mut projection = crate::transcript::Transcript::default();
        projection.append(&events);
        let items = &projection.chunks[0];
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "hello");
    }
}

// DOM selection offsets count UTF-16 code units, not Rust bytes.
pub fn insert_image_path(draft: &str, path: &str, start: u32, end: u32) -> String {
    fn offset(text: &str, units: u32) -> usize {
        let mut count = 0;
        for (index, ch) in text.char_indices() {
            if count >= units { return index; }
            count += ch.len_utf16() as u32;
        }
        text.len()
    }
    let start = offset(draft, start);
    let end = offset(draft, end).max(start);
    let before = &draft[..start];
    let after = &draft[end..];
    format!("{}{}\"{}\"{}{}", before,
        if before.is_empty() || before.ends_with(char::is_whitespace) { "" } else { " " },
        path,
        if after.is_empty() || after.starts_with(char::is_whitespace) { "" } else { " " }, after)
}

#[cfg(test)]
mod image_tests {
    use super::*;
    #[test]
    fn path_insertion_uses_browser_offsets_and_preserves_surrounding_text() {
        assert_eq!(insert_image_path("🙂 replace end", "/a b.png", 3, 10), "🙂 \"/a b.png\" end");
        assert_eq!(insert_image_path("later edits", "/image.png", u32::MAX, u32::MAX), "later edits \"/image.png\"");
    }
}
