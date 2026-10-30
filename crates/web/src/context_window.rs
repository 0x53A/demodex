use serde_json::Value;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub value: String,
    pub bounds: Value,
    pub disabled: bool,
    pub onchange: Callback<String>,
}

#[function_component(ContextWindow)]
pub fn context_window(props: &Props) -> Html {
    let default = props.value.is_empty();
    let available = props.bounds["min"].as_i64().is_some() && props.bounds["max"].as_i64().is_some();
    let toggle = {
        let callback = props.onchange.clone();
        let initial = props.bounds["default"].as_i64().unwrap_or(0).to_string();
        Callback::from(move |event: Event| {
            callback.emit(if event.target_unchecked_into::<HtmlInputElement>().checked() { String::new() } else { initial.clone() });
        })
    };
    let slide = {
        let callback = props.onchange.clone();
        Callback::from(move |event: InputEvent| callback.emit(event.target_unchecked_into::<HtmlInputElement>().value()))
    };
    html! {<div class="context-window" role="group" aria-label="Context window">
        <span>{"Context window"}</span>
        <label class="checkbox"><input type="checkbox" aria-label="Default context window" checked={default}
            disabled={props.disabled || (default && !available)} onchange={toggle}/>{"Default"}</label>
        {if default {Html::default()} else {html!{<div class="context-window-slider">
            <input type="range" aria-label="Context window tokens" min={props.bounds["min"].to_string()}
                max={props.bounds["max"].to_string()} step="1" value={props.value.clone()}
                disabled={props.disabled || !available} oninput={slide}/>
            <output aria-label="Context window value">{&props.value}</output>
        </div>}}}
    </div>}
}
