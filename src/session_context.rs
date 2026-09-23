//! Agent-reported presentation metadata. This never changes execution settings.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub const INSTRUCTIONS: &str = "You are running inside Demodex, a UI for managing agent sessions across machines. Demodex shows your session in a sparse project folder tree. Use demodex.get_session_context to see your identity and execution environments. Use demodex.set_user_visible_session_context when you establish or change the project you are working on, including after creating a project. Supply its execution environment ID, absolute project-root path, and optionally a short description of your work. Keep the project root during incidental commands in other directories. These tools only update display metadata; they do not change execution directories, sandbox permissions, or session identity.";

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

pub fn tools() -> Value {
    json!([{
        "type":"namespace", "name":"demodex", "description":"User-visible session context in the Demodex operator UI. These tools do not change execution settings or permissions.",
        "tools":[
            {"type":"function", "name":"set_user_visible_session_context", "description":"Report the project shown beside your session icon. Use the project root, not incidental command directories. description defaults to an empty string. Only updates this session's display metadata.",
             "inputSchema":{"type":"object","properties":{"environment_id":{"type":"string"},"path":{"type":"string","description":"Absolute project-root path in that execution environment"},"description":{"type":"string","description":"Short user-visible description of the work","default":""}},"required":["environment_id","path"],"additionalProperties":false}},
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
