use serde_json::Value;
use yew::prelude::*;

use crate::model::text;

pub fn title(method: &str) -> &'static str {
    match method {
        "item/commandExecution/requestApproval" => "Approve command",
        "item/fileChange/requestApproval" => "Approve file changes",
        "item/tool/requestUserInput" => "Agent needs your input",
        _ => "Review agent request",
    }
}

pub fn status(state: &str, connected: bool) -> &str {
    match state {
        "unavailable" => "Unavailable",
        "responding" => "Sending response…",
        "delivered" => "Response sent · awaiting resolution",
        "pending" if !connected => "Disconnected · response unavailable",
        "pending" => "Waiting for you",
        _ => state,
    }
}

pub fn summary(method: &str, params: &Value) -> Html {
    let reason = text(params, "reason");
    let command = text(params, "command");
    html! {<div class="approval-summary">
        {if !reason.is_empty() {html!{<p class="approval-reason">{reason}</p>}} else {Html::default()}}
        {if method == "item/commandExecution/requestApproval" {
            if command.is_empty() {html!{<p class="muted">{"No command text supplied. Review the request details before responding."}</p>}}
            else {html!{<pre class="approval-command" tabindex="0" aria-label="Command to approve">{command}</pre>}}
        } else {Html::default()}}
        <dl class="approval-context">
            {for [("Working directory", "cwd"), ("Executor", "environmentId"), ("Requested write root", "grantRoot")]
                .into_iter().filter(|(_, field)| !text(params, field).is_empty()).map(|(label, field)| html!{
                    <><dt>{label}</dt><dd><code>{text(params, field)}</code></dd></>
                })}
        </dl>
        {for [("Requested permissions", "additionalPermissions"), ("Network access", "networkApprovalContext")]
            .into_iter().filter(|(_, field)| !params[*field].is_null()).map(|(label, field)| html!{
                <div class="approval-permissions"><h3>{label}</h3><pre tabindex="0" aria-label={label}>{serde_json::to_string_pretty(&params[field]).unwrap_or_default()}</pre></div>
            })}
    </div>}
}

pub fn details(method: &str, params: &Value) -> Html {
    html! {<details class="approval-details"><summary>{"Request details"}</summary>
        <p class="muted">{method}</p><pre tabindex="0" aria-label="Raw request">{serde_json::to_string_pretty(params).unwrap_or_default()}</pre>
    </details>}
}
