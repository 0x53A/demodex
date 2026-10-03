use crate::model::{array, text};
use demodex_protocol::Operation;
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
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by_key(|s| (s["starred"] != true, s["sort_order"].as_i64().unwrap_or(0)));
    for session in ordered {
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

pub fn title(session: &Value) -> &str {
    session["name"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or("Untitled session")
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
        "working" | "active" | "running" => "working",
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
    siblings: &[Value],
    environments: &[Value],
    selected: &str,
    select: &Callback<String>,
    edit: &Callback<String>,
    run: &Callback<Operation>,
    disabled: bool,
    reorder_mode: bool,
) -> Html {
    let id = text(session, "id").to_owned();
    let chosen = id == selected;
    let archive_id = id.clone();
    let archived = session["archived"] == true;
    let unavailable = !archived
        && !matches!(
            text(session, "status"),
            "idle" | "connected" | "disconnected"
        );
    let action = if archived { "Restore" } else { "Archive" };
    let label = format!("{action} {}", identity(session));
    let action_title = if unavailable {
        "Stop the turn and resolve pending work in Session controls before archiving."
    } else if archived {
        "Restore session without resuming work"
    } else {
        "Archive session; keep its history. Pending work must be resolved first."
    };
    let starred = session["starred"] == true;
    let star_id = id.clone();
    let star_run = run.clone();
    let edit_id = id.clone();
    let edit = edit.clone();
    let peers = siblings.iter().filter(|s| (s["starred"] == true) == starred).map(|s|(text(s,"id").to_owned(),title(s).to_owned())).collect::<Vec<_>>();
    let handle = if reorder_mode {html!{<crate::reorder::Handle id={id.clone()} name={title(session).to_owned()} {peers} {disabled} onrun={run.clone()}/>}}else{Html::default()};
    let run = run.clone();
    let select = select.clone();
    let status = text(session, "status");
    html! {<li class="tree-agent" key={id.clone()} data-session-id={id.clone()}><button class={classes!("session",chosen.then_some("chosen"))} aria-current={chosen.then_some("page")} onclick={Callback::from(move |_|select.emit(id.clone()))}>
        <strong class={classes!("session-title",status_class(status))}>{title(session)}</strong>
        <small class="agent-identity"><span class="agent-icon" aria-hidden="true">{text(&session["presentation"],"icon")}</span>{identity(session)}</small>
        <small class="session-state"><span class={classes!("agent-status",status_class(status))}>{status}</span>
        {" · "}<span class="session-executors">{if array(&session["targets"]).is_empty(){"No executors".into()}else{array(&session["targets"]).iter().map(|target|environment_label(text(target,"id"),environments)).collect::<Vec<_>>().join(" · ")}}</span></small>
        {crate::usage::context(session,false)}
        <span class="session-activity-slots"><span>
        {if session["goal"].is_object(){html!{<small class="goal-indicator" title={text(&session["goal"],"objective").to_owned()}>{format!("Goal · {}",text(&session["goal"],"status"))}</small>}}else{Html::default()}}
        </span><span>
        {match session["background_count"].as_u64(){
            Some(0)=>Html::default(),
            Some(n)=>html!{<small class="background-count">{format!("{n} background terminal{} running",if n==1{""}else{"s"})}</small>},
            None=>html!{<small class="muted background-unknown">{"Background terminals unknown"}</small>},
        }}
        </span><span>{if let Some(n)=session["active_subagents"].as_u64().filter(|n|*n>0){html!{<small class="subagent-count" title="Latest subagent activity reported by Codex">{format!("{n} active subagent{}",if n==1{""}else{"s"})}</small>}}else{Html::default()}}</span></span>
    </button><crate::ui::IconButton class="session-archive-action" label={label} title={action_title} disabled={disabled||unavailable} onclick={Callback::from(move |_|run.emit(Operation::Archive{id:archive_id.clone(),archived:!archived}))} destructive={!archived}>{if archived {"↶"} else {"×"}}</crate::ui::IconButton>
    <crate::ui::IconButton class={classes!("session-star-action",starred.then_some("starred"))} label={format!("{} {}",if starred {"Unstar"}else{"Star"},title(session))} pressed={starred.to_string()} title={if starred {"Unstar session"}else{"Star session"}} disabled={disabled} onclick={Callback::from(move |_|star_run.emit(Operation::StarSession{id:star_id.clone(),starred:!starred}))}>{if starred {"★"}else{"☆"}}</crate::ui::IconButton>
    <crate::ui::IconButton class="session-edit-action" label={format!("Edit {}",title(session))} title="Session controls" onclick={Callback::from(move |_|edit.emit(edit_id.clone()))}>{"✎"}</crate::ui::IconButton>
    {handle}</li>}
}

fn folder_view(
    mut name: String,
    mut folder: &Folder,
    environments: &[Value],
    selected: &str,
    select: &Callback<String>,
    edit: &Callback<String>,
    run: &Callback<Operation>,
    create: &Callback<MouseEvent>,
    disabled: bool,
    reorder_mode: bool,
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
        {if !folder.sessions.is_empty(){html!{<li class="folder-add-session"><crate::ui::AddButton onclick={create.clone()} {disabled}>{"+ Session"}</crate::ui::AddButton></li>}}else{Html::default()}}
        {for folder.sessions.iter().map(|s|session_view(s,&folder.sessions,environments,selected,select,edit,run,disabled,reorder_mode))}
        {for folder.children.iter().map(|(name,child)|folder_view(name.clone(),child,environments,selected,select,edit,run,create,disabled,reorder_mode))}
    </ul></li>}
}

pub fn view(
    sessions: &[Value],
    environments: &[Value],
    selected: &str,
    select: Callback<String>,
    edit: Callback<String>,
    run: Callback<Operation>,
    disabled: bool,
    reorder_mode: bool,
    flat: bool,
    create: Callback<MouseEvent>,
) -> Html {
    let tree = forest(sessions);
    if flat {
        let mut ordered=sessions.to_vec();
        ordered.sort_by_key(|s|(s["starred"]!=true,s["sort_order"].as_i64().unwrap_or(0)));
        return html!{<nav class="session-tree session-list" aria-label="Sessions as list"><ul>
            {for ordered.iter().map(|session|{
                let path=locations(session).first().map(|(_,path,_)|path.clone());
                let peers=ordered.iter().filter(|other|locations(other).first().map(|(_,path,_)|path.clone())==path).cloned().collect::<Vec<_>>();
                session_view(session,&peers,environments,selected,&select,&edit,&run,disabled,reorder_mode)
            })}
        </ul>{if sessions.is_empty(){html!{<p class="muted">{"No sessions on this server yet. Choose New Session to start one."}</p>}}else{Html::default()}}</nav>};
    }
    html! {<nav class="session-tree" aria-label="Sessions by project">
        {if sessions.is_empty(){html!{<p class="muted">{"No sessions on this server yet. Choose New Session to start one."}</p>}}else{html!{<ul>{folder_view("/".into(),&tree,environments,selected,&select,&edit,&run,&create,disabled,reorder_mode)}</ul>}}}
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
    fn stars_sort_first_and_keep_their_own_manual_order() {
        let tree = forest(&[
            json!({"id":"plain-later","starred":false,"sort_order":4}),
            json!({"id":"star-later","starred":true,"sort_order":3}),
            json!({"id":"plain-first","starred":false,"sort_order":1}),
            json!({"id":"star-first","starred":true,"sort_order":2}),
        ]);
        assert_eq!(tree.sessions.iter().map(|s|text(s,"id")).collect::<Vec<_>>(),vec!["star-first","star-later","plain-first","plain-later"]);
    }

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
