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
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.chunks, &other.chunks)
            && self.working == other.working
            && self.waiting == other.waiting
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

#[function_component(MessageItem)]
fn message_item(props: &ItemProps) -> Html {
    let item = props.item.as_ref();
    let kind = text(item, "type");
    html! {<article class={(kind=="userMessage").then_some("user")} data-item-id={text(item,"id").to_owned()}>
        <div class="item-kind">{kind}</div>
        {match kind {
            "agentMessage"=>html!{<pre>{text(item,"text")}</pre>},
            "userMessage"=>html!{<pre>{array(&item["content"]).iter().map(|c|text(c,"text")).collect::<Vec<_>>().join("\n")}</pre>},
            "commandExecution"=>html!{<><code>{text(item,"command")}</code><details><summary>{"Command output"}</summary><pre>{text(item,"aggregatedOutput")}</pre></details></>},
            _=>html!{<details><summary>{kind}</summary><pre>{serde_json::to_string_pretty(item).unwrap_or_default()}</pre></details>},
        }}
    </article>}
}

#[function_component(Conversation)]
pub fn conversation(props: &Props) -> Html {
    html! {<div class="conversation">
        {for props.chunks.iter().enumerate().map(|(index,items)|html!{
            <MessageChunk key={index} items={items.clone()}/>
        })}
        {if props.waiting{html!{<div class="activity waiting" role="status">{"Waiting for your input"}</div>}}
        else if props.working{html!{<div class="activity" role="status"><span class="activity-marker" aria-hidden="true"/>{"Working…"}</div>}}
        else{Html::default()}}
    </div>}
}
