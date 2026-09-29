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
    let blockers = restart_blockers(&props.runtime);

    html! {
        <section class="runtime-panel feature-settings">
            <details class="feature-catalog">
                <summary><crate::ui::SectionTitle>{"Codex feature flags"}</crate::ui::SectionTitle></summary>
                <p class="muted">{"Default inherits the Codex profile. Overrides save immediately and apply after restarting Codex."}</p>
                <label>{"Filter features"}<input type="search" value={(*search).clone()} oninput={Callback::from({let search=search.clone(); move |event: InputEvent| search.set(event.target_unchecked_into::<HtmlInputElement>().value())})}/></label>
                <div class="feature-list">
                {for catalog.iter().filter(|feature| feature["stage"] != "removed" && feature["name"].as_str().unwrap_or("").contains(&filter)).map(|feature| {
                    let name = feature["name"].as_str().unwrap_or("").to_owned();
                    let input_id = format!("feature-{name}");
                    let selected = match saved[&name].as_bool() { Some(true)=>"on", Some(false)=>"off", None=>"profile" };
                    let onchange = props.onrun.reform({let name=name.clone(); move |event: Event| {
                        let value = event.target_unchecked_into::<HtmlSelectElement>().value();
                        Operation::SetRuntimeFeature {name:name.clone(), enabled:match value.as_str(){"on"=>Some(true),"off"=>Some(false),_=>None}}
                    }});
                    html!{<div class="feature-flag" key={name.clone()}>
                        <div class="feature-info">
                            <label for={input_id.clone()}><code>{&name}</code></label>
                            <small class="muted">{format!("{} · Startup: {}", feature["stage"].as_str().unwrap_or("unknown"), match feature["enabled"].as_bool() {Some(true)=>"enabled",Some(false)=>"disabled",None=>"unknown"})}</small>
                            {if let Some(description)=feature["description"].as_str(){html!{<p class="muted feature-description">{description}</p>}}else{Html::default()}}
                            {if name=="agent_message_board"{html!{<small class="muted">{"Requires multi_agent_v2 and persistent sessions."}</small>}}else{Html::default()}}
                        </div>
                        <select id={input_id} disabled={props.disabled || props.runtime["running"] != true} {onchange}>
                            <option value="profile" selected={selected=="profile"}>{"Default"}</option><option value="on" selected={selected=="on"}>{"Enabled"}</option><option value="off" selected={selected=="off"}>{"Disabled"}</option>
                        </select>
                    </div>}
                })}
                {for saved.as_object().into_iter().flat_map(|map|map.iter()).filter(|(name,_)| !catalog.iter().any(|f|f["name"].as_str()==Some(name.as_str()) && f["stage"]!="removed")).map(|(name, enabled)| {
                    let remove=props.onrun.reform({let name=name.clone();move |_|Operation::SetRuntimeFeature{name:name.clone(),enabled:None}});
                    html!{<p><code>{name}</code>{format!(" — saved: {enabled}; unavailable in this catalogue ")}<button disabled={props.disabled} onclick={remove}>{"Use default"}</button></p>}
                })}
                </div>
            <details class="runtime-restart"><summary>{"Restart Codex"}</summary>
            <p class="muted">{"Sessions disconnect on restart. Reconnect them afterward."}</p>
            {if !blockers.is_empty(){html!{<ul class="restart-blockers control-warning" role="status">{for blockers.iter().map(|blocker|html!{<li>{blocker}</li>})}</ul>}}else{Html::default()}}
            <button disabled={props.disabled || props.runtime["running"] != true || !blockers.is_empty()} onclick={restart}>{"Restart Codex and disconnect sessions"}</button>
            </details>
            </details>
            {if features["restart_required"] == true {html!{<p role="status">{"Saved changes are waiting for a Codex restart."}</p>}} else {Html::default()}}
            {if let Some(error) = features["catalog"]["error"].as_str() {html!{<p class="muted">{error}</p>}} else {Html::default()}}
        </section>
    }
}

fn restart_blockers(runtime: &Value) -> Vec<String> {
    let mut blockers = Vec::new();
    for (key, singular, plural) in [
        ("active_sessions","session working or waiting","sessions working or waiting"),
        ("active_goals","active goal","active goals"),
        ("queued_messages","queued message","queued messages"),
        ("pending_decisions","pending decision","pending decisions"),
        ("background_terminals","background terminal running","background terminals running"),
        ("unavailable_sessions","session could not be checked","sessions could not be checked"),
    ] {
        if let Some(count) = runtime["restart_blockers"][key].as_u64().filter(|n|*n>0) {
            blockers.push(format!("{count} {}",if count == 1 {singular}else{plural}));
        }
    }
    blockers
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_only_lists_current_nonzero_blockers() {
        assert!(restart_blockers(&serde_json::json!({})).is_empty());
        assert_eq!(restart_blockers(&serde_json::json!({"restart_blockers":{"active_sessions":1,"active_goals":0,"queued_messages":2,"unavailable_sessions":1}})),vec!["1 session working or waiting","2 queued messages","1 session could not be checked"]);
    }
}
