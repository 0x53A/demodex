use crate::model::{array, text};
use serde_json::Value;
use std::collections::BTreeMap;
use yew::prelude::*;

#[derive(Default)]
pub struct Folder {
    children: BTreeMap<String, Folder>,
    sessions: Vec<Value>,
}

pub fn locations(session: &Value) -> Vec<(String, String, bool)> {
    let context = &session["presentation"]["context"];
    let environment = text(context, "environment_id");
    let targets = array(&session["targets"]);
    if !environment.is_empty()
        && targets.iter().any(|t| text(t, "id") == environment)
        && text(context, "path").starts_with('/')
    {
        return vec![(environment.into(), text(context, "path").into(), true)];
    }
    targets
        .iter()
        .map(|t| (text(t, "id").into(), text(t, "cwd").into(), false))
        .collect()
}

pub fn forest(sessions: &[Value]) -> Folder {
    let mut root = Folder::default();
    for session in sessions {
        // Show each session once: reported project, otherwise its primary directory.
        // Executor generations are transport identities, not folder groups.
        let mut folder = &mut root;
        if let Some((_, path, _)) = locations(session).first() {
            for part in path.split('/').filter(|part| !part.is_empty()) {
                folder = folder.children.entry(part.into()).or_default();
            }
        }
        folder.sessions.push(session.clone());
    }
    root
}

pub fn identity(session: &Value) -> String {
    let name = text(&session["presentation"], "name");
    if name.is_empty() {
        text(session, "name").into()
    } else {
        name.into()
    }
}

pub fn status_class(status: &str) -> &'static str {
    match status {
        "working" | "active" => "working",
        "waiting" => "waiting",
        "error" | "systemError" => "failed",
        "disconnected" | "notLoaded" => "disconnected",
        _ => "idle",
    }
}

pub fn environment_label(id: &str, environments: &[Value]) -> String {
    if id.is_empty() {
        return "No execution environment".into();
    }
    if id == "host" || id.starts_with("host-") {
        return "Host".into();
    }
    for environment in environments {
        if id == text(environment, "id") || id.starts_with(&format!("{}-", text(environment, "id")))
        {
            return format!(
                "{} · {}",
                text(environment, "kind"),
                text(environment, "name")
            );
        }
        if id.starts_with(&format!("vm-{}-", text(environment, "id"))) {
            return format!("VM · {}", text(environment, "name"));
        }
    }
    id.into()
}

fn session_view(
    session: &Value,
    environments: &[Value],
    selected: &str,
    select: &Callback<String>,
) -> Html {
    let id = text(session, "id").to_owned();
    let chosen = id == selected;
    let select = select.clone();
    let status = text(session, "status");
    let description = text(&session["presentation"]["context"], "description");
    html! {<li class="tree-agent"><button class={classes!("session",chosen.then_some("chosen"))} aria-current={chosen.then_some("page")} onclick={Callback::from(move |_|select.emit(id.clone()))}>
        <strong class="agent-identity"><span class="agent-icon" aria-hidden="true">{text(&session["presentation"],"icon")}</span>{identity(session)}</strong>
        <small class="session-executors">{array(&session["targets"]).iter().map(|target|environment_label(text(target,"id"),environments)).collect::<Vec<_>>().join(" · ")}</small>
        <span class="session-title">{text(session,"name")}</span>
        {if !description.is_empty(){html!{<small>{description}</small>}}else{Html::default()}}
        <span class={classes!("agent-status",status_class(status))}>{status}</span>
        {crate::usage::context(session,false)}
        <small class="location-source">{if locations(session).iter().any(|(_,_,reported)|*reported){"Agent-reported project"}else{"Executor directory"}}</small>
    </button></li>}
}

fn folder_view(
    mut name: String,
    mut folder: &Folder,
    environments: &[Value],
    selected: &str,
    select: &Callback<String>,
) -> Html {
    // Collapse empty ancestry, but retain every folder with an attached agent.
    while folder.sessions.is_empty() && folder.children.len() == 1 {
        let (child, next) = folder.children.first_key_value().unwrap();
        if !name.ends_with('/') {
            name.push('/');
        }
        name.push_str(child);
        folder = next;
    }
    html! {<li class="tree-folder"><div class="folder-name"><span aria-hidden="true">{"▱ "}</span>{name}</div><ul>
        {for folder.sessions.iter().map(|s|session_view(s,environments,selected,select))}
        {for folder.children.iter().map(|(name,child)|folder_view(name.clone(),child,environments,selected,select))}
    </ul></li>}
}

pub fn view(
    sessions: &[Value],
    environments: &[Value],
    selected: &str,
    select: Callback<String>,
) -> Html {
    let tree = forest(sessions);
    html! {<nav class="session-tree" aria-label="Sessions by project">
        {if sessions.is_empty(){html!{<p class="muted">{"No sessions on this server yet. Choose New Session to start one."}</p>}}else{html!{<ul>{folder_view("/".into(),&tree,environments,selected,&select)}</ul>}}}
    </nav>}
}

pub fn context_view(session: &Value, environments: &[Value]) -> Html {
    let description = text(&session["presentation"]["context"], "description");
    let locations = locations(session);
    let reported = locations.iter().any(|(_, _, reported)| *reported);
    html! {<section class="session-context" aria-label="Session identity and project">
        <div class="context-project"><span class="eyebrow">{if reported{"AGENT-REPORTED PROJECT"}else{"EXECUTOR DIRECTORY"}}</span>
            {for locations.iter().map(|(environment,path,_)|html!{<p><span class="environment-label">{environment_label(environment,environments)}</span><code>{path}</code></p>})}
            {if reported && !description.is_empty(){html!{<p class="context-description">{description}</p>}}else{Html::default()}}
            {if !reported{html!{<p class="muted">{if session["presentation"]["context_reporting"]==true{"The agent has not reported a project for this execution environment yet."}else{"Project reporting is unavailable for this imported or older thread. Showing its configured executor directory."}}</p>}}else{Html::default()}}
        </div>
        <div class="session-identifiers">
            <label>{"Codex thread UUID"}<input class="session-uuid" aria-label="Codex thread UUID" readonly=true value={text(session,"thread_id").to_owned()} placeholder="Assigned when connected" onclick={Callback::from(|e:MouseEvent|e.target_unchecked_into::<web_sys::HtmlInputElement>().select())}/></label>
            <label>{"Demodex session UUID"}<input class="session-uuid" aria-label="Demodex session UUID" readonly=true value={text(session,"id").to_owned()} onclick={Callback::from(|e:MouseEvent|e.target_unchecked_into::<web_sys::HtmlInputElement>().select())}/></label>
        </div>
    </section>}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reported_project_replaces_fallback_only_in_its_attached_environment() {
        let mut session = json!({"id":"one","targets":[{"id":"host","cwd":"/home/lukas/src"}],"presentation":{"context":{"environment_id":"host","path":"/home/lukas/src/cairn"}}});
        assert_eq!(
            locations(&session),
            vec![("host".into(), "/home/lukas/src/cairn".into(), true)]
        );
        session["targets"][0]["id"] = json!("replacement");
        assert_eq!(
            locations(&session),
            vec![("replacement".into(), "/home/lukas/src".into(), false)]
        );
    }
    #[test]
    fn folder_tree_merges_host_generations_and_shows_each_session_once() {
        let sessions = vec![
            json!({"id":"a","targets":[{"id":"host-old","cwd":"/workspace/a"}]}),
            json!({"id":"b","targets":[{"id":"host-new","cwd":"/workspace/a"}]}),
            json!({"id":"c","targets":[{"id":"vm","cwd":"/workspace/a"},{"id":"host-third","cwd":"/workspace/a"}]}),
            json!({"id":"d","targets":[{"id":"host-third","cwd":"/workspace/b"}]}),
        ];
        let tree = forest(&sessions);
        assert_eq!(tree.children["workspace"].children.len(), 2);
        assert_eq!(tree.children["workspace"].children["a"].sessions.len(), 3);
        assert_eq!(tree.children["workspace"].children["b"].sessions.len(), 1);
    }
}
