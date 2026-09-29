//! Agent-reported presentation metadata. This never changes execution settings.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub const INSTRUCTIONS: &str = "You are running inside Demodex, a UI for managing agent sessions across machines. Demodex shows your session in a sparse project folder tree. Use demodex.get_session_context to see your identity and execution environments. Use demodex.set_user_visible_session_context when you establish or change the project you are working on, including after creating a project. Supply its execution environment ID, absolute project-root path, and optionally a short description of your work. Keep the project root during incidental commands in other directories. When available, use demodex.set_session_identity to change your session title, display name, or icon when useful or requested. Omitted fields stay unchanged. When available, use demodex.notify when an explicit notification to the user is useful or requested. It records a chat notification and attempts push to enabled devices; acceptance is not proof the user received or read it. Never send duplicate notifications for the same event. Ask questions directly in chat: request_user_input and request_user_input_async are not reliable in this Demodex integration, even if listed in the tool catalogue. Messages support Markdown tables, LaTeX formulas, and fenced mermaid diagrams; users can view and copy raw text. Context and identity tools only update display metadata; they do not change execution directories, sandbox permissions, or session/thread IDs.";

pub use demodex_protocol::{Presentation, UserVisibleContext};

pub fn generate_presentation() -> Presentation {
    let attributes = [
        "Stinky",
        "Sleepy",
        "Suspicious",
        "Curious",
        "Mossy",
        "Cosmic",
        "Quiet",
        "Brisk",
        "Wobbly",
        "Velvet",
        "Rusty",
        "Dapper",
        "Sunny",
        "Feral",
        "Tiny",
        "Grumpy",
    ];
    let subjects = [
        ("Werecat", "🐈"),
        ("Owl", "🦉"),
        ("Badger", "🦡"),
        ("Fox", "🦊"),
        ("Otter", "🦦"),
        ("Raven", "🐦‍⬛"),
        ("Moth", "🦋"),
        ("Frog", "🐸"),
        ("Octopus", "🐙"),
        ("Turtle", "🐢"),
        ("Hedgehog", "🦔"),
        ("Raccoon", "🦝"),
        ("Bat", "🦇"),
        ("Crab", "🦀"),
        ("Dragon", "🐉"),
        ("Snail", "🐌"),
    ];
    let random = uuid::Uuid::new_v4();
    let bytes = random.as_bytes();
    let (subject, icon) = subjects[bytes[1] as usize % subjects.len()];
    Presentation {
        name: format!(
            "{} {subject}",
            attributes[bytes[0] as usize % attributes.len()]
        ),
        icon: icon.into(),
        context_reporting: false,
        context: None,
    }
}

pub fn validate(context: &mut UserVisibleContext, targets: &[crate::store::Target]) -> Result<()> {
    ensure!(
        targets.iter().any(|t| t.id == context.environment_id),
        "environment_id must identify an execution environment attached to this session"
    );
    ensure!(
        context.path.starts_with('/')
            && context.path.len() <= 4096
            && !context.path.chars().any(char::is_control),
        "path must be an absolute path without control characters (at most 4096 bytes)"
    );
    ensure!(
        !context.path.split('/').any(|part| part == ".."),
        "path must not contain parent-directory components"
    );
    let parts: Vec<_> = context
        .path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    context.path = format!("/{}", parts.join("/"));
    ensure!(
        context.description.chars().count() <= 240
            && !context.description.chars().any(char::is_control),
        "description must be a single line of at most 240 characters"
    );
    Ok(())
}

/// A partial display update; IDs and execution settings are never accepted.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityUpdate {
    pub title: Option<String>,
    pub name: Option<String>,
    pub icon: Option<String>,
}

impl IdentityUpdate {
    pub fn validate(&mut self) -> Result<()> {
        ensure!(self.title.is_some() || self.name.is_some() || self.icon.is_some(), "provide at least one of title, name, or icon");
        for (field, value, limit) in [
            ("title", &mut self.title, 120),
            ("name", &mut self.name, 120),
            ("icon", &mut self.icon, 32),
        ] {
            if let Some(value) = value {
                ensure!(!value.chars().any(char::is_control), "{field} must be a single line without control characters");
                *value = value.trim().to_owned();
                ensure!(!value.is_empty() && value.chars().count() <= limit, "{field} must be 1–{limit} characters");
            }
        }
        Ok(())
    }
}

pub fn tools() -> Value {
    json!([{
        "type":"namespace", "name":"demodex", "description":"Session identity, project context, and explicit notifications in Demodex. These tools do not change execution settings or permissions.",
        "tools":[
            {"type":"function","name":"notify","description":"Record a notification in this session chat and attempt encrypted push to enabled devices. Use deliberately when user attention is useful or requested. Delivery is best effort; never assume the user received or read it.","inputSchema":{"type":"object","properties":{"message":{"type":"string","minLength":1,"maxLength":1000},"title":{"type":"string","minLength":1,"maxLength":120}},"required":["message"],"additionalProperties":false}},
            {"type":"function", "name":"set_user_visible_session_context", "description":"Report the project shown beside your session icon. Use the project root, not incidental command directories. description defaults to an empty string. Only updates this session's display metadata.",
             "inputSchema":{"type":"object","properties":{"environment_id":{"type":"string"},"path":{"type":"string","description":"Absolute project-root path in that execution environment"},"description":{"type":"string","description":"Short user-visible description of the work","default":""}},"required":["environment_id","path"],"additionalProperties":false}},
            {"type":"function", "name":"set_session_identity", "description":"Update this session's Demodex title, display identity name, and/or icon. Supply at least one field; omitted fields stay unchanged. Does not rename the underlying Codex thread or change IDs, project context, or execution settings.",
             "inputSchema":{"type":"object","properties":{"title":{"type":"string","description":"Session title in Demodex (1–120 characters)","minLength":1,"maxLength":120},"name":{"type":"string","description":"Display identity name, replacing the generated name (1–120 characters)","minLength":1,"maxLength":120},"icon":{"type":"string","description":"Short display icon, usually an emoji (1–32 characters)","minLength":1,"maxLength":32}},"additionalProperties":false}},
            {"type":"function", "name":"get_session_context", "description":"Read this session's IDs, display identity, reported context, and attached execution environments.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}
        ]
    }])
}

pub fn response(result: Result<Value>) -> Value {
    let (success, text) = match result {
        Ok(value) => (true, value.to_string()),
        Err(error) => (false, format!("{error:#}")),
    };
    json!({"success":success,"contentItems":[{"type":"inputText","text":text}]})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, Target};

    fn call(id: &str, path: &str) -> Value {
        json!({"namespace":"demodex","tool":"set_user_visible_session_context","threadId":"thread","turnId":"turn","callId":id,"arguments":{"environment_id":"host","path":path}})
    }

    #[test]
    fn context_is_scoped_validated_durable_and_idempotent() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("state.sqlite");
        let store = Store::open(&database)?;
        let session = store.create(
            "Title",
            "ws://localhost:1",
            &[Target {
                id: "host".into(),
                url: "ws://localhost:2".into(),
                cwd: "/src".into(),
            }],
            Some("thread"),
        )?;
        store.enable_context_reporting(&session.id)?;
        let first = call("one", "/src//cairn/./");
        let result = store.context_tool(&session.id, "thread", &first)?;
        assert_eq!(result["success"], true);
        assert_eq!(
            store.get(&session.id)?.presentation.context.unwrap().path,
            "/src/cairn"
        );
        store.context_tool(&session.id, "thread", &call("two", "/src/new"))?;
        assert_eq!(store.context_tool(&session.id, "thread", &first)?, result);
        assert_eq!(
            store.get(&session.id)?.presentation.context.unwrap().path,
            "/src/new"
        );
        assert!(
            store
                .context_tool(&session.id, "thread", &call("one", "/src/changed"))
                .is_err()
        );
        for (index, path) in ["relative", "~/project", "/src/../other", "/src/\nproject"]
            .iter()
            .enumerate()
        {
            assert_eq!(
                store.context_tool(&session.id, "thread", &call(&format!("bad-{index}"), path))?["success"],
                false
            );
        }
        let mut foreign = call("foreign", "/src/other");
        foreign["arguments"]["environment_id"] = json!("unattached");
        assert_eq!(
            store.context_tool(&session.id, "thread", &foreign)?["success"],
            false
        );
        foreign["threadId"] = json!("another-thread");
        assert!(store.context_tool(&session.id, "thread", &foreign).is_err());
        let mut unknown = call("unknown", "/src/other");
        unknown["tool"] = json!("execute_command");
        assert_eq!(
            store.context_tool(&session.id, "thread", &unknown)?["success"],
            false
        );
        let mut description = call("description", "/src/other");
        description["arguments"]["description"] = json!("x".repeat(241));
        assert_eq!(
            store.context_tool(&session.id, "thread", &description)?["success"],
            false
        );
        drop(store);
        let store = Store::open(&database)?;
        let saved = store.get(&session.id)?;
        assert_eq!(saved.presentation.name, session.presentation.name);
        assert_eq!(saved.presentation.icon, session.presentation.icon);
        assert_eq!(saved.presentation.context.unwrap().path, "/src/new");
        assert_eq!(saved.targets[0].cwd, "/src");
        assert_eq!(store.context_tool(&session.id, "thread", &first)?, result);
        Ok(())
    }

    #[test]
    fn identity_updates_are_partial_atomic_scoped_and_durable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("identity.sqlite");
        let store = Store::open(&database)?;
        let session = store.create("Original title", "ws://localhost:1", &[Target {
            id:"host".into(),url:"ws://localhost:2".into(),cwd:"/src".into(),
        }], Some("thread"))?;
        let other = store.create("Other", "ws://localhost:1", &[], Some("other-thread"))?;
        store.enable_context_reporting(&session.id)?;
        store.context_tool(&session.id,"thread",&call("project","/src/project"))?;
        let update = |id: &str, arguments: Value| json!({"namespace":"demodex","tool":"set_session_identity","threadId":"thread","turnId":"turn","callId":id,"arguments":arguments});
        let first = update("identity-one", json!({"title":"  New title  ","name":"Suspicious Raven","icon":"🐦‍⬛"}));
        let response = store.context_tool(&session.id,"thread",&first)?;
        assert_eq!(response["success"],true);
        let updated = store.get(&session.id)?;
        assert_eq!(updated.name,"New title");
        assert_eq!(updated.presentation.name,"Suspicious Raven");
        assert_eq!(updated.presentation.icon,"🐦‍⬛");
        assert_eq!(updated.presentation.context.as_ref().unwrap().path,"/src/project");
        assert_eq!(updated.targets[0].cwd,"/src");
        assert_eq!(updated.thread_id,session.thread_id);
        for (index,args) in [json!({}),json!({"title":"Would overwrite","icon":" "}),json!({"name":"bad\nname"}),json!({"icon":"x".repeat(33)}),json!({"title":"x".repeat(121)}),json!({"name":"x".repeat(121)}),json!({"name":"Allowed","session_id":other.id})].into_iter().enumerate() {
            assert_eq!(store.context_tool(&session.id,"thread",&update(&format!("invalid-{index}"),args))?["success"],false);
            assert_eq!(store.get(&session.id)?.name,"New title");
            assert_eq!(store.get(&session.id)?.presentation.name,"Suspicious Raven");
        }
        let second = update("identity-two",json!({"title":"Newest title"}));
        assert_eq!(store.context_tool(&session.id,"thread",&second)?["success"],true);
        assert_eq!(store.context_tool(&session.id,"thread",&first)?,response);
        assert!(store.context_tool(&session.id,"thread",&update("identity-one",json!({"name":"Changed"}))).is_err());
        assert!(store.context_tool(&other.id,"thread",&first).is_err());
        let read = json!({"namespace":"demodex","tool":"get_session_context","threadId":"thread","callId":"read","arguments":{}});
        let read_response = store.context_tool(&session.id,"thread",&read)?;
        let value: Value = serde_json::from_str(read_response["contentItems"][0]["text"].as_str().unwrap())?;
        assert_eq!(value["title"],"Newest title");
        drop(store);
        let store = Store::open(&database)?;
        let saved = store.get(&session.id)?;
        assert_eq!(saved.name,"Newest title");
        assert_eq!(saved.presentation.name,"Suspicious Raven");
        assert_eq!(saved.presentation.icon,"🐦‍⬛");
        assert_eq!(saved.presentation.context.unwrap().path,"/src/project");
        assert_eq!(store.get(&other.id)?.name,"Other");
        assert_eq!(store.context_tool(&session.id,"thread",&first)?,response);
        Ok(())
    }

    #[test]
    fn existing_database_gets_a_stable_identity_without_losing_data() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("state.sqlite");
        let original = rusqlite::Connection::open(&database)?;
        original.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,name TEXT NOT NULL,endpoint TEXT NOT NULL,thread_id TEXT,targets TEXT NOT NULL,status TEXT NOT NULL,error TEXT,created INTEGER NOT NULL DEFAULT(unixepoch())); INSERT INTO sessions(id,name,endpoint,thread_id,targets,status) VALUES('old','Keep title','ws://localhost:1','thread','[]','idle');")?;
        drop(original);
        let store = Store::open(&database)?;
        let first = store.get("old")?;
        assert_eq!(first.name, "Keep title");
        assert_eq!(first.thread_id.as_deref(), Some("thread"));
        assert!(!first.presentation.context_reporting);
        assert!(!first.presentation.name.is_empty());
        drop(store);
        assert_eq!(
            Store::open(&database)?.get("old")?.presentation.name,
            first.presentation.name
        );
        Ok(())
    }
}
