use crate::client::Client;
use demodex_protocol::{BugReport, BugReportPage, Operation};
use std::rc::Rc;
use yew::prelude::*;

#[derive(Properties)]
pub struct Props {
    pub client: Rc<Client>,
    pub server_id: String,
    pub onseen: Callback<(String, i64)>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.client, &other.client) && self.server_id == other.server_id && self.onseen == other.onseen }
}

pub struct BugReports {
    reports: Vec<BugReport>,
    next_before: Option<i64>,
    busy: bool,
    loaded: bool,
    error: Option<String>,
}
pub enum Msg {
    Load(bool),
    Loaded(bool, Result<BugReportPage, String>),
}
impl Component for BugReports {
    type Message = Msg;
    type Properties = Props;
    fn create(_: &Context<Self>) -> Self {
        Self { reports: Vec::new(), next_before: None, busy: false, loaded: false, error: None }
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Load(older) => {
                if self.busy { return false; }
                self.busy = true;
                self.error = None;
                let before = if older { self.next_before } else { None };
                let client = ctx.props().client.clone();
                ctx.link().send_future(async move {
                    let result = match client.read(Operation::BugReports {before}).await {
                        Ok(value) => serde_json::from_value(value).map_err(|e|e.to_string()),
                        Err(error) => Err(error.to_string()),
                    };
                    Msg::Loaded(older,result)
                });
            }
            Msg::Loaded(older,result) => {
                self.busy = false;
                match result {
                    Ok(page) => {
                        if !older {
                            self.reports.clear();
                            if let Some(report) = page.reports.first() {
                                ctx.props().onseen.emit((ctx.props().server_id.clone(), report.id));
                            }
                        }
                        self.reports.extend(page.reports);
                        self.next_before = page.next_before;
                        self.loaded = true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {<crate::ui::Group class="runtime-panel bug-reports" title="Bug reports">
            <p class="muted">{"Reports filed by agents on this server, newest first. Saved for later review; filing a report does not send a notification."}</p>
            <button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Load(false))}>{if self.busy {"Loading…"} else if self.loaded {"Refresh reports"} else {"Load reports"}}</button>
            {if let Some(error)=&self.error {html!{<p class="control-warning" role="alert">{format!("Could not load reports: {error}. Displayed reports may be out of date.")}</p>}} else {Html::default()}}
            {if self.loaded && self.reports.is_empty() {html!{<p>{"No bug reports yet."}</p>}} else {Html::default()}}
            {for self.reports.iter().map(|report|html!{
                <details key={report.id} class="bug-report">
                    <summary>{format!("#{} · {}",report.id,report.title)}</summary>
                    <p class="muted">{format!("{} · {}",report.created_at,report.session_title)}</p>
                    {if let Some(project)=&report.project {html!{<p>{format!("Project: {} ({})",project.path,project.environment_id)}</p>}} else {Html::default()}}
                    <pre style="white-space:pre-wrap;overflow-wrap:anywhere">{&report.description}</pre>
                    <details><summary>{"Source session"}</summary><p>{format!("Demodex: {}",report.session_id)}</p><p>{format!("Codex: {}",report.thread_id.as_deref().unwrap_or("unavailable"))}</p></details>
                </details>
            })}
            {if self.next_before.is_some() {html!{<button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Load(true))}>{"Show older reports"}</button>}} else {Html::default()}}
        </crate::ui::Group>}
    }
}

const SEEN_PREFIX: &str = "demodex-bug-reports-seen:";
pub fn seen_key(server: &str) -> String { format!("{SEEN_PREFIX}{server}") }
pub fn is_seen_key(key: &str) -> bool { key.starts_with(SEEN_PREFIX) }
pub fn has_new(summary: &serde_json::Value) -> bool {
    let server = summary["server_id"].as_str().unwrap_or("");
    let seen = web_sys::window().and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(&seen_key(server)).ok().flatten())
        .and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    !server.is_empty() && summary["latest_id"].as_i64().unwrap_or(0) > seen
}
pub fn mark_seen(server: &str, latest: i64) {
    if server.is_empty() || latest <= 0 { return; }
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let key = seen_key(server);
        let previous = storage.get_item(&key).ok().flatten().and_then(|v|v.parse::<i64>().ok()).unwrap_or(0);
        // An older tab/page cannot move the watermark backwards.
        let _ = storage.set_item(&key, &previous.max(latest).to_string());
    }
}
