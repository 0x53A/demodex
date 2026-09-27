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
    html! {<>{for props.items.iter().map(|item|html!{
        <MessageItem key={text(item,"id").to_owned()} item={item.clone()}/>
    })}</>}
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
    if item["success"] == false {
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

fn raw_details(item: &Value) -> Html {
    html! {<details><summary>{"Details"}</summary><pre>{serde_json::to_string_pretty(item).unwrap_or_default()}</pre></details>}
}

#[function_component(MessageItem)]
fn message_item(props: &ItemProps) -> Html {
    let item = props.item.as_ref();
    let kind = text(item, "type");
    let failed = text(item, "status") == "failed"
        || item["success"] == false
        || item["exitCode"].as_i64().is_some_and(|code| code != 0);
    let show_status = !matches!(
        kind,
        "agentMessage" | "userMessage" | "plan" | "demodexPlan" | "demodexError"
    );
    html! {<article class={(kind=="userMessage").then_some("user")} data-item-id={text(item,"id").to_owned()}>
        <div class="item-kind">{label(kind)}{if show_status{html!{<span class={classes!("item-status",failed.then_some("failed"))}>{status(item)}</span>}}else{Html::default()}}</div>
        {match kind {
            "agentMessage" | "plan"=>html!{<pre>{text(item,"text")}</pre>},
            "userMessage"=>html!{<pre>{array(&item["content"]).iter().map(|c|match text(c,"type") {"image"=>"[Attached image]", "localImage"=>text(c,"path"), _=>text(c,"text")}).collect::<Vec<_>>().join("\n")}</pre>},
            "reasoning"=>html!{<><pre>{array(&item["summary"]).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n\n")}</pre>{if !array(&item["content"]).is_empty(){html!{<details><summary>{"Reasoning details"}</summary><pre>{array(&item["content"]).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n\n")}</pre></details>}}else{Html::default()}}</>},
            "commandExecution"=>html!{<><pre class="tool-heading"><code>{text(item,"command")}</code></pre><small class="muted">{text(item,"cwd")}</small><details><summary>{"Command output"}</summary><pre>{text(item,"aggregatedOutput")}</pre></details></>},
            "fileChange"=>html!{<ul class="tool-paths">{for array(&item["changes"]).iter().map(|change|html!{<li><code>{text(change,"path")}</code>{" · "}{text(&change["kind"],"type")}<details><summary>{"Diff"}</summary><pre>{text(change,"diff")}</pre></details></li>})}</ul>},
            "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall"=>html!{<><p class="tool-heading">{text(item,"server")}{" "}{text(item,"tool")}</p>{if !item["error"].is_null(){html!{<p class="error">{text(&item["error"],"message")}</p>}}else{Html::default()}}<pre>{text(item,"_demodexProgress")}</pre>{raw_details(item)}</>},
            "webSearch"=>html!{<><pre>{text(item,"query")}</pre>{raw_details(item)}</>},
            "demodexPlan"=>html!{<><pre>{text(item,"text")}</pre><ol class="plan-steps">{for array(&item["plan"]).iter().map(|step|html!{<li><span>{match text(step,"status"){"inProgress"=>"In progress", "completed"=>"Completed", "pending"=>"Pending", other=>other}}</span>{text(step,"step")}</li>})}</ol></>},
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
        {for props.chunks.iter().enumerate().map(|(index,items)|html!{
            <MessageChunk key={index} items={items.clone()}/>
        })}
        {if !props.connected{html!{<div class="activity disconnected" role="status">{"Connection lost — activity cannot be confirmed"}</div>}}
        else if props.waiting{html!{<div class="activity waiting" role="status">{"Waiting for your input"}</div>}}
        else if props.working{html!{<div class="activity" role="status"><span class="activity-marker" aria-hidden="true"/>{"Working…"}</div>}}
        else{Html::default()}}
    </div>}
}
