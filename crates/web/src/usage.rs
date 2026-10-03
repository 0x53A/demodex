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
            let remaining = (100.0-percent).clamp(0.0,100.0);
            let label = format!("Context {remaining:.0}% remaining");
            let title = format!(
                "Last reported context: {used} / {window} tokens. Updated when Codex reports usage; not cumulative session tokens."
            );
            html! {<span class="context-usage" title={title.clone()} aria-label={title}>
                <span>{label}</span><meter min="0" max="100" value={remaining.to_string()} aria-label="Context window remaining"/>
                {if detailed{html!{<small>{format!("{used} / {window} tokens · last reported")}</small>}}else{Html::default()}}
            </span>}
        }
        None => {
            html! {<span class="context-usage unavailable" title="Waiting for Codex to report token usage and the model context window">{"Context unavailable"}</span>}
        }
    }
}

// This is an age estimate, not cache telemetry or a prediction of billed usage.
const POSSIBLY_COLD_AFTER: i64 = 30 * 60;
pub fn cold_input_tokens(session: &Value, now: i64) -> Option<i64> {
    if matches!(text(session,"status"), "working" | "active" | "running" | "connecting") { return None; }
    let usage = &session["context_usage"];
    let reported = usage["reported_at"].as_i64().filter(|n|*n > 0)?;
    let input = usage["input_tokens"].as_i64().filter(|n|*n >= 1024)?;
    (now.saturating_sub(reported) >= POSSIBLY_COLD_AFTER).then_some(input)
}
fn token_count(tokens: i64) -> String {
    let digits = tokens.to_string();
    let mut result = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len()-index).is_multiple_of(3) { result.push(','); }
        result.push(digit);
    }
    result
}
pub fn cache_indicator(session: &Value, detailed: bool) -> Html {
    let Some(input) = cold_input_tokens(session, (js_sys::Date::now()/1000.0) as i64) else { return Html::default(); };
    let description = format!("Possibly cold · Last request: {} input tokens. Sending again may require uncached processing.", token_count(input));
    let explanation = "No token usage reported for at least 30 minutes. Cache expiry is not reported by Codex; this is an estimate, not the next request's token count.";
    if detailed {
        html!{<div class="cache-info" role="note" title={explanation}><span aria-hidden="true">{"❄ "}</span>{description}</div>}
    } else {
        html!{<span class="cache-indicator" role="img" aria-label={description.clone()} title={format!("{description} {explanation}")}>{"❄"}</span>}
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

fn remaining(window: &Value) -> Option<i64> {
    window["used_percent"].as_i64().filter(|n| (0..=100).contains(n)).map(|n| 100-n)
}
fn reset_in(window: &Value) -> String {
    let Some(reset) = window["resets_at"].as_f64() else { return "reset time unavailable".into(); };
    let minutes = ((reset - js_sys::Date::now()/1000.0).max(0.0)/60.0).ceil() as u64;
    format!("resets in {}d {}h {}m", minutes/1440, minutes%1440/60, minutes%60)
}
fn credit_label(credits: &Value) -> String {
    if credits["unlimited"] == true { return "Unlimited".into(); }
    if let Some(balance) = credits["balance"].as_str().filter(|s| !s.trim().is_empty()) { return format!("{balance} credits remaining"); }
    if credits["balance"].is_number() { return format!("{} credits remaining", credits["balance"]); }
    match credits["hasCredits"].as_bool() {
        Some(true) => "Available · balance not reported".into(),
        Some(false) => "No credits available".into(),
        None => "Not reported".into(),
    }
}
pub fn weekly(runtime: &Value, connected: bool) -> Html {
    let usage = &runtime["weekly_usage"];
    let weekly = array(&usage["windows"]);
    let windows = if usage["all_windows"].is_array() { array(&usage["all_windows"]) } else { weekly.clone() };
    let summary = if !connected { "Weekly: disconnected".into() }
        else if let Some(first) = weekly.first() {
            match remaining(first) {
                Some(left) => format!("Weekly: {left}% remaining, {}",reset_in(first)),
                None => "Weekly: unavailable".into(),
            }
        } else { "Weekly: unavailable".into() };
    html! {<details class="weekly-usage" aria-label="Server usage">
        <summary>{summary}</summary>
        <div class="weekly-details">
            {for windows.iter().map(|window|{
                let duration=window["duration_minutes"].as_i64().unwrap_or(10080);
                let period=match duration {10080=>"Weekly".into(),n if n%60==0=>format!("{}h",n/60),n=>format!("{n}m")};
                html!{<div class="weekly-window">
                    <strong>{format!("{} · {} · {}",text(window,"name"),period,remaining(window).map(|v|format!("{v}% remaining")).unwrap_or_else(||"unavailable".into()))}</strong>
                    {if let Some(left)=remaining(window){html!{<meter min="0" max="100" value={left.to_string()} aria-label={format!("Remaining {} capacity for {}",period,text(window,"name"))}/>}}else{Html::default()}}
                    <small>{reset_in(window)}</small><small>{format!("Reset: {}",timestamp(&window["resets_at"]))}</small>
                </div>}
            })}
            {if windows.is_empty(){html!{<p class="muted">{usage["error"].as_str().unwrap_or("Usage unavailable")}</p>}}else{Html::default()}}
            <div class="usage-credits"><strong>{"Credits"}</strong>
                {if array(&usage["credits"]).is_empty(){html!{<p class="muted">{"Credits not reported"}</p>}}else{html!{<>
                    {for array(&usage["credits"]).iter().map(|entry|html!{<p>{format!("{} · {}",text(entry,"name"),credit_label(&entry["credits"]))}</p>})}
                </>}}}
            </div>
            {if usage["checked_at"].is_number(){html!{<small>{format!("Last updated: {}",timestamp(&usage["checked_at"]))}</small>}}else{Html::default()}}
        </div>
    </details>}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cold_estimate_uses_last_input_and_age_and_never_marks_active_work() {
        let mut session = json!({"status":"idle","context_usage":{"input_tokens":123456,"cached_input_tokens":120000,"used_tokens":150000,"reported_at":1000}});
        assert_eq!(cold_input_tokens(&session, 2799), None);
        assert_eq!(cold_input_tokens(&session, 2800), Some(123456));
        assert_eq!(cold_input_tokens(&session, 999), None);
        session["status"] = json!("working");
        assert_eq!(cold_input_tokens(&session, 5000), None);
        session["status"] = json!("disconnected");
        assert_eq!(cold_input_tokens(&session, 5000), Some(123456));
        session["context_usage"]["input_tokens"] = Value::Null;
        assert_eq!(cold_input_tokens(&session, 5000), None);
        assert_eq!(cold_input_tokens(&json!({}), 5000), None);
        assert_eq!(token_count(123456), "123,456");
    }

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
