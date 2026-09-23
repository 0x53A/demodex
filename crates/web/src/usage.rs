use crate::model::{array, text};
use serde_json::Value;
use yew::prelude::*;

pub fn context_numbers(session: &Value) -> Option<(i64, i64, f64)> {
    let usage = &session["context_usage"];
    let used = usage["used_tokens"].as_i64().filter(|n| *n >= 0)?;
    let window = usage["window_tokens"].as_i64().filter(|n| *n > 0)?;
    Some((used, window, used as f64 / window as f64 * 100.0))
}

pub fn context(session: &Value, detailed: bool) -> Html {
    match context_numbers(session) {
        Some((used, window, percent)) => {
            let label = format!("Context {:.0}% used", percent);
            let title = format!(
                "Last reported context: {used} / {window} tokens. Updated when Codex reports usage; not cumulative session tokens."
            );
            html! {<span class="context-usage" title={title.clone()} aria-label={title}>
                <span>{label}</span><meter min="0" max="100" value={percent.clamp(0.0,100.0).to_string()} aria-label="Context window used"/>
                {if detailed{html!{<small>{format!("{used} / {window} tokens · last reported")}</small>}}else{Html::default()}}
            </span>}
        }
        None => {
            html! {<span class="context-usage unavailable" title="Waiting for Codex to report token usage and the model context window">{"Context unavailable"}</span>}
        }
    }
}

fn timestamp(seconds: &Value) -> String {
    seconds
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| {
            js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(n * 1000.0))
                .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED)
                .as_string()
                .unwrap_or_default()
        })
        .unwrap_or_else(|| "not reported".into())
}

pub fn weekly(runtime: &Value, connected: bool) -> Html {
    let usage = &runtime["weekly_usage"];
    let windows = array(&usage["windows"]);
    let summary = if !connected {
        "Weekly usage · disconnected".into()
    } else if let Some(first) = windows.first() {
        let suffix = if windows.len() > 1 {
            format!(" · {} limits", windows.len())
        } else {
            String::new()
        };
        format!(
            "Weekly usage · {}% used{}",
            first["used_percent"].as_i64().unwrap_or(0),
            suffix
        )
    } else {
        "Weekly usage unavailable".into()
    };
    html! {<details class="weekly-usage" aria-label="Server weekly usage">
        <summary>{summary}</summary>
        <div class="weekly-details"><p class="muted">{"Reported for this server’s Codex account. Servers using the same account share its limits."}</p>
            {for windows.iter().map(|window|html!{<div class="weekly-window">
                <strong>{format!("{} · {}% used",text(window,"name"),window["used_percent"])}</strong>
                <meter min="0" max="100" value={window["used_percent"].to_string()} aria-label={format!("Weekly usage for {}",text(window,"name"))}/>
                <small>{format!("Resets {}",timestamp(&window["resets_at"]))}</small>
            </div>})}
            {if windows.is_empty(){html!{<p class="muted">{usage["error"].as_str().unwrap_or("Start the Codex runtime and sign in to see weekly usage.")}</p>}}else{Html::default()}}
            {if usage["checked_at"].is_number(){html!{<small>{format!("Checked {} · refreshes about once a minute",timestamp(&usage["checked_at"]))}</small>}}else{Html::default()}}
        </div>
    </details>}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn context_percentage_uses_reported_window_without_assuming_model_capacity() {
        assert_eq!(
            context_numbers(&json!({"context_usage":{"used_tokens":50000,"window_tokens":200000}})),
            Some((50000, 200000, 25.0))
        );
        assert!(
            context_numbers(&json!({"context_usage":{"used_tokens":50000,"window_tokens":null}}))
                .is_none()
        );
        assert!(context_numbers(&json!({})).is_none());
        assert_eq!(
            context_numbers(&json!({"context_usage":{"used_tokens":0,"window_tokens":100}}))
                .unwrap()
                .2,
            0.0
        );
    }
}
