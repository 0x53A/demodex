use demodex_protocol::Operation;
use serde_json::Value;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub runtime: Value,
    pub disabled: bool,
    pub onrun: Callback<Operation>,
}

#[function_component(RuntimeFeatures)]
pub fn runtime_features(props: &Props) -> Html {
    let search = use_state(String::new);
    let features = &props.runtime["features"];
    let saved = &features["saved"];
    let catalog = features["catalog"]["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let filter = search.to_lowercase();
    let restart = props.onrun.reform(|_| Operation::RestartRuntime);
    html! {
        <section class="runtime-panel">
            <h2>{"Codex feature flags"}</h2>
            <p>{"Overrides apply to this server’s managed Codex runtime. Profile uses the existing Codex configuration. Changes are saved immediately and take effect after restarting Codex."}</p>
            {if features["restart_required"] == true {html!{<p role="status">{"Saved changes are waiting for a Codex restart."}</p>}} else {Html::default()}}
            {if let Some(error) = features["catalog"]["error"].as_str() {html!{<p class="muted">{error}</p>}} else {Html::default()}}
            <details>
                <summary>{"Configure feature flags"}</summary>
                <label>{"Filter features"}<input type="search" value={(*search).clone()} oninput={Callback::from({let search=search.clone(); move |event: InputEvent| search.set(event.target_unchecked_into::<HtmlInputElement>().value())})}/></label>
                {for catalog.iter().filter(|feature| feature["stage"] != "removed" && feature["name"].as_str().unwrap_or("").contains(&filter)).map(|feature| {
                    let name = feature["name"].as_str().unwrap_or("").to_owned();
                    let selected = match saved[&name].as_bool() { Some(true)=>"on", Some(false)=>"off", None=>"profile" };
                    let onchange = props.onrun.reform({let name=name.clone(); move |event: Event| {
                        let value = event.target_unchecked_into::<HtmlSelectElement>().value();
                        Operation::SetRuntimeFeature {name:name.clone(), enabled:match value.as_str(){"on"=>Some(true),"off"=>Some(false),_=>None}}
                    }});
                    html!{<div class="feature-flag">
                        <label><code>{&name}</code><select value={selected} disabled={props.disabled || props.runtime["running"] != true} {onchange}>
                            <option value="profile">{"Profile"}</option><option value="on">{"Enabled"}</option><option value="off">{"Disabled"}</option>
                        </select></label>
                        <p class="muted">{format!("{} · At startup: {}", feature["stage"].as_str().unwrap_or("unknown"), if feature["enabled"] == true {"enabled"}else{"disabled"})}</p>
                        {if let Some(description)=feature["description"].as_str(){html!{<p>{description}</p>}}else{Html::default()}}
                        {if name=="agent_message_board"{html!{<p>{"Requires multi_agent_v2 and persistent sessions."}</p>}}else{Html::default()}}
                    </div>}
                })}
                {for saved.as_object().into_iter().flat_map(|map|map.iter()).filter(|(name,_)| !catalog.iter().any(|f|f["name"].as_str()==Some(name.as_str()) && f["stage"]!="removed")).map(|(name, enabled)| {
                    let remove=props.onrun.reform({let name=name.clone();move |_|Operation::SetRuntimeFeature{name:name.clone(),enabled:None}});
                    html!{<p><code>{name}</code>{format!(" — saved: {enabled}; unavailable in this catalogue ")}<button disabled={props.disabled} onclick={remove}>{"Use profile"}</button></p>}
                })}
            </details>
            <p class="muted">{"Restart disconnects managed sessions and replaces the host executor. Sessions must be idle, with goals paused, queues empty, decisions resolved and background terminals stopped. Reconnect sessions afterward."}</p>
            <button disabled={props.disabled || props.runtime["running"] != true} onclick={restart}>{"Restart Codex and disconnect sessions"}</button>
        </section>
    }
}
