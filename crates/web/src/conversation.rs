use crate::model::{array, text};
use crate::transcript::{Chunk, Chunks};
use serde_json::Value;
use std::rc::Rc;
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct Props {
    pub chunks: Chunks,
    pub working: bool,
    pub waiting: bool,
    pub connected: bool,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.chunks, &other.chunks)
            && self.working == other.working
            && self.waiting == other.waiting
            && self.connected == other.connected
    }
}

#[derive(Properties)]
struct ChunkProps {
    items: Chunk,
}
impl PartialEq for ChunkProps {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.items, &other.items)
    }
}

#[derive(Properties)]
struct ItemProps {
    item: Rc<Value>,
}
impl PartialEq for ItemProps {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.item, &other.item)
    }
}

// Unchanged chunks skip their view entirely. Within a changed chunk, keyed item
// components keep tool expansion, DOM nodes and selection while only changed
// items render. Keys use stable item IDs, never event cursor or list length.
#[function_component(MessageChunk)]
fn message_chunk(props: &ChunkProps) -> Html {
    let items = html! {<>{for props.items.iter().map(|item|html!{
        <MessageItem key={text(item,"id").to_owned()} item={item.clone()}/>
    })}</>};
    if !props.items.first().is_some_and(|item| crate::transcript::is_activity(item)) { return items; }
    let failed = props.items.iter().filter(|item| is_failed(item)).count();
    let running = props.items.iter().filter(|item| status(item).starts_with("Running")).count();
    let unknown = props.items.iter().filter(|item| status(item).contains("unreported")).count();
    let stopped = props.items.iter().filter(|item| matches!(text(item,"status"),"interrupted"|"declined")).count();
    let last = props.items.last().unwrap();
    let description = match text(last,"type") { "commandExecution"=>text(last,"command"), "webSearch"=>text(last,"query"), "mcpToolCall"|"dynamicToolCall"=>text(last,"tool"), kind=>label(kind) };
    html!{<details class="tool-group" data-group-id={text(&props.items[0],"id").to_owned()}>
        <summary><span>{format!("{} activit{}",props.items.len(),if props.items.len()==1{"y"}else{"ies"})}</span>
        {if running>0{html!{<span class="activity-running">{format!(" · {running} running")}</span>}}else{Html::default()}}
        {if failed>0{html!{<span class="failed">{format!(" · {failed} failed")}</span>}}else{Html::default()}}
        {if unknown>0{html!{<span>{format!(" · {unknown} unreported")}</span>}}else{Html::default()}}
        {if stopped>0{html!{<span>{format!(" · {stopped} stopped/declined")}</span>}}else{Html::default()}}
        <code class="activity-description">{description}</code></summary>{items}
    </details>}
}

fn label(kind: &str) -> &str {
    match kind {
        "userMessage" => "You",
        "agentMessage" => "Assistant",
        "reasoning" => "Reasoning",
        "commandExecution" => "Command",
        "fileChange" => "File changes",
        "webSearch" => "Web search",
        "mcpToolCall" | "dynamicToolCall" => "Tool call",
        "collabAgentToolCall" => "Agent collaboration",
        "subAgentActivity" => "Agent activity",
        "plan" | "demodexPlan" => "Plan",
        "contextCompaction" => "Context compaction",
        "imageView" => "Image viewed",
        "imageGeneration" => "Image generation",
        "enteredReviewMode" => "Review started",
        "exitedReviewMode" => "Review finished",
        "demodexError" => "Error",
        "demodexNotification" => "Notification",
        "demodexTurnEnd" => "Turn ended",
        "sleep" => "Waiting",
        _ => kind,
    }
}

fn status(item: &Value) -> String {
    if item["_demodexLifecycle"] == "ended" && matches!(text(item, "status"), "" | "inProgress") {
        return "Turn ended · outcome unreported".into();
    }
    let state = match text(item, "status") {
        "inProgress" => "Running",
        "completed" => "Completed",
        "failed" => "Failed",
        "declined" => "Declined",
        "interrupted" => "Interrupted",
        "" => match text(item, "_demodexLifecycle") {
            "running" => "Running",
            "completed" => "Completed",
            _ => "",
        },
        state => state,
    };
    let mut state = state.to_owned();
    if is_failed(item) {
        state = "Failed".into();
    }
    if let Some(code) = item["exitCode"].as_i64() {
        state.push_str(&format!(" · exit {code}"));
    }
    if let Some(ms) = item["durationMs"].as_u64() {
        state.push_str(&format!(" · {:.1}s", ms as f64 / 1000.));
    }
    state
}

fn is_failed(item: &Value) -> bool {
    text(item,"status")=="failed" || item["success"]==false || item["exitCode"].as_i64().is_some_and(|n|n!=0)
}

fn command_output(output: &str) -> Html {
    let preview = output.lines().take(5).collect::<Vec<_>>().join("\n");
    let lines = output.lines().count();
    html!{<><pre class="command-preview">{preview}</pre><details><summary>{if lines>5{format!("Command output · {} more lines",lines-5)}else{"Command output".into()}}</summary><pre>{output}</pre></details></>}
}

fn notification_delivery(delivery: &Value) -> String {
    let accepted=delivery["accepted"].as_u64().unwrap_or_default();
    let failed=delivery["failed"].as_u64().unwrap_or_default();
    let expired=delivery["expired"].as_u64().unwrap_or_default();
    let unavailable=delivery["unavailable"].as_u64().unwrap_or_default();
    if accepted>0 { format!("Push service accepted {accepted} request(s){}",if failed+expired+unavailable>0 {format!(" · {} unavailable",failed+expired+unavailable)}else{String::new()}) }
    else if failed+expired+unavailable>0 { "Push unavailable · notification saved here".into() }
    else { "Notification saved here".into() }
}

fn raw_details(item: &Value) -> Html {
    html! {<details><summary>{"Details"}</summary><pre>{serde_json::to_string_pretty(item).unwrap_or_default()}</pre></details>}
}

#[function_component(MessageItem)]
fn message_item(props: &ItemProps) -> Html {
    let item = props.item.as_ref();
    let kind = text(item, "type");
    let failed = is_failed(item);
    let show_status = !matches!(
        kind,
        "agentMessage" | "userMessage" | "plan" | "demodexPlan" | "demodexError" | "demodexNotification"
    );
    html! {<article class={(kind=="userMessage").then_some("user")} data-item-id={text(item,"id").to_owned()}>
        <div class="item-kind">{label(kind)}{if show_status{html!{<span class={classes!("item-status",failed.then_some("failed"))}>{status(item)}</span>}}else{Html::default()}}{crate::timestamps::view(item)}</div>
        {match text(item,"_demodexSteering") {
            "waiting"=>html!{<small class="steering-status" role="status">{"Waiting for the next tool call"}</small>},
            "unconfirmed"=>html!{<small class="steering-status" role="status">{"Turn ended · message consumption unconfirmed"}</small>},
            "failed"=>html!{<small class="steering-status error" role="status">{"Steering was not confirmed. Check the command result before retrying."}</small>},
            _=>Html::default(),
        }}
        {match kind {
            "agentMessage" | "plan"=>html!{<><crate::rich_messages::Message source={text(item,"text").to_owned()} item_id={text(item,"id").to_owned()} file_revision={item["_demodexFilesRevision"].as_u64().unwrap_or_default()}/><crate::music::MessageScores text={text(item,"text").to_owned()}/></>},
            "userMessage"=>html!{<crate::rich_messages::Message source={array(&item["content"]).iter().map(|c|match text(c,"type") {"image"=>"[Attached image]", "localImage"=>text(c,"path"), _=>text(c,"text")}).collect::<Vec<_>>().join("\n")}/>},
            "reasoning"=>html!{<><pre>{array(&item["summary"]).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n\n")}</pre>{if !array(&item["content"]).is_empty(){html!{<details><summary>{"Reasoning details"}</summary><pre>{array(&item["content"]).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n\n")}</pre></details>}}else{Html::default()}}</>},
            "commandExecution"=>html!{<><pre class="tool-heading"><code>{text(item,"command")}</code></pre><small class="muted">{text(item,"cwd")}</small>{command_output(text(item,"aggregatedOutput"))}</>},
            "fileChange"=>html!{<ul class="tool-paths">{for array(&item["changes"]).iter().map(|change|html!{<li><code>{text(change,"path")}</code>{" · "}{text(&change["kind"],"type")}<details><summary>{"Diff"}</summary>{crate::diff::view(text(change,"diff"))}</details></li>})}</ul>},
            "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall"=>html!{<><p class="tool-heading">{text(item,"server")}{" "}{text(item,"tool")}</p>{if !item["error"].is_null(){html!{<p class="error">{text(&item["error"],"message")}</p>}}else{Html::default()}}<pre>{text(item,"_demodexProgress")}</pre>{raw_details(item)}</>},
            "subAgentActivity"=>html!{<p>{format!("Agent {} · {}", text(item,"agentPath"), text(item,"kind"))}</p>},
            "webSearch"=>html!{<><pre>{text(item,"query")}</pre>{raw_details(item)}</>},
            "demodexPlan"=>html!{<><crate::rich_messages::Message source={text(item,"text").to_owned()}/><ol class="plan-steps">{for array(&item["plan"]).iter().map(|step|html!{<li><span>{match text(step,"status"){"inProgress"=>"In progress", "completed"=>"Completed", "pending"=>"Pending", other=>other}}</span>{text(step,"step")}</li>})}</ol></>},
            "demodexNotification"=>html!{<div class="chat-notification"><strong>{text(item,"title")}</strong><pre>{text(item,"text")}</pre>{if item["delivery"].is_object(){html!{<small>{notification_delivery(&item["delivery"])}</small>}}else{Html::default()}}</div>},
            "demodexError"=>html!{<p class="error">{text(item,"text")}{if item["retrying"]==true {" — Codex scheduled a retry."} else {""}}</p>},
            "demodexTurnEnd"=>html!{<pre>{text(item,"text")}</pre>},
            "imageView"=>html!{<code>{text(item,"path")}</code>},
            "contextCompaction"=>html!{<p class="muted">{"Conversation context compaction"}</p>},
            "enteredReviewMode" | "exitedReviewMode"=>html!{<pre>{text(item,"review")}</pre>},
            _=>raw_details(item),
        }}
    </article>}
}

#[function_component(Conversation)]
pub fn conversation(props: &Props) -> Html {
    html! {<div class="conversation">
        <div class="message-list">{for props.chunks.iter().map(|items|html!{
            <MessageChunk key={text(&items[0],"id").to_owned()} items={items.clone()}/>
        })}</div>
        {if !props.connected{html!{<div class="activity disconnected" role="status">{"Connection lost — activity cannot be confirmed"}</div>}}
        else if props.waiting{html!{<div class="activity waiting" role="status">{"Waiting for your input"}</div>}}
        else if props.working{html!{<div class="activity" role="status"><span class="activity-marker" aria-hidden="true"/>{"Working…"}</div>}}
        else{Html::default()}}
    </div>}
}
