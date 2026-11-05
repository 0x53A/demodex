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
fn reset_count(summary: &Value) -> Option<u64> {
    summary["availableCount"].as_u64()
}
fn reset_details_note(summary: &Value) -> Option<String> {
    match summary["credits"].as_array() {
        None if reset_count(summary).is_some() => Some("Reset details not reported".into()),
        Some(rows) if reset_count(summary).is_some_and(|count| count > rows.len() as u64) =>
            Some(format!("Details reported for {} of {} available resets", rows.len(), reset_count(summary).unwrap())),
        _ => None,
    }
}
fn reset_expiry(credit: &Value) -> String {
    match credit.get("expiresAt") {
        Some(Value::Null) => "Does not expire".into(),
        Some(value) => format!("Expires: {}", timestamp(value)),
        None => "Expiry not reported".into(),
    }
}
fn reset_credits(summary: &Value) -> Html {
    html! {<div class="usage-resets">
        <strong>{"Available resets"}</strong>
        <p>{reset_count(summary).map(|count| format!("{count} available")).unwrap_or_else(|| "Not reported by Codex".into())}</p>
        {for array(&summary["credits"]).iter().map(|credit| {
            let title = credit["title"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or("Rate-limit reset");
            html!{<div class="weekly-window usage-reset">
                <strong>{title}</strong>
                {if let Some(description)=credit["description"].as_str().filter(|s| !s.trim().is_empty()) {html!{<small>{description}</small>}} else {Html::default()}}
                <small>{format!("Granted: {}",timestamp(&credit["grantedAt"]))}</small>
                <small>{reset_expiry(credit)}</small>
            </div>}
        })}
        {if let Some(note)=reset_details_note(summary) {html!{<small>{note}</small>}} else {Html::default()}}
    </div>}
}
pub fn weekly(runtime: &Value, connected: bool) -> Html {
    let usage = &runtime["weekly_usage"];
    let weekly = array(&usage["windows"]);
    let windows = if !connected { Vec::new() } else if usage["all_windows"].is_array() { array(&usage["all_windows"]) } else { weekly.clone() };
    if runtime["account"].is_null() { return Html::default(); }
    let primary: Vec<_> = windows.iter().filter(|w| w["id"] == "codex").cloned().collect();
    let overview = if primary.is_empty() { &windows } else { &primary };
    html! {<details class="weekly-usage codex-usage" aria-label="Codex usage">
        {quota_summary("Codex",overview,connected,false)}
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
            {reset_credits(&usage["reset_credits"])}
            {if usage["checked_at"].is_number(){html!{<small>{format!("Last updated: {}",timestamp(&usage["checked_at"]))}</small>}}else{Html::default()}}
        </div>
    </details>}
}

fn compact_countdown(reset: Option<f64>, now: f64) -> String {
    let Some(reset) = reset.filter(|n|n.is_finite() && *n>0.0) else {return "(?)".into();};
    let minutes=((reset-now).max(0.0)/60.0).ceil() as u64;
    let (days,hours,mins)=(minutes/1440,minutes%1440/60,minutes%60);
    let mut text=String::from("(");
    if days>0 {text.push_str(&format!("{days}d"));}
    if hours>0 {text.push_str(&format!("{hours}h"));}
    if mins>0 || minutes==0 {text.push_str(&format!("{mins}m"));}
    text.push(')');text
}
fn quota_summary(provider: &str, windows: &[Value], connected: bool, iso_reset: bool) -> Html {
    let rows: Vec<_> = [300,10080].into_iter().filter_map(|minutes|
        windows.iter().find(|w| w["duration_minutes"]==minutes).map(|w|(minutes,w))
    ).collect();
    html!{<summary class="quota-summary">
        <strong>{format!("{provider}:")}</strong>
        <span class="quota-graphs">
            {if !connected {html!{<span class="muted">{"disconnected"}</span>}}
            else if rows.is_empty(){html!{<span class="muted">{"Usage unavailable"}</span>}}
            else {html!{<>{for rows.into_iter().map(|(duration,window)| {
                let period=if duration==300 {"5h"}else{"weekly"};
                let left=claude_remaining(window);
                let reset=if iso_reset {claude_reset(window)["resets_at"].as_f64()}else{window["resets_at"].as_f64()};
                let label=format!("{provider} {period}: {}",claude_label(window));
                html!{<span class="quota-row" title={label.clone()}>
                    <span class="quota-period">{format!("{period}:")}</span>
                    {if let Some(left)=left {html!{<meter min="0" max="100" value={left.to_string()} aria-label={label}/>}}else{html!{<span class="muted" aria-label={label}>{"—"}</span>}}}
                    <span class="quota-countdown" title="Time until reported reset">{compact_countdown(reset,js_sys::Date::now()/1000.0)}</span>
                </span>}
            })}</>}}}
        </span>
    </summary>}
}

fn claude_remaining(window: &Value) -> Option<f64> {
    window["used_percent"].as_f64().filter(|n| n.is_finite() && (0.0..=100.0).contains(n)).map(|n| 100.0-n)
}
fn claude_label(window: &Value) -> String {
    claude_remaining(window).map(|n| format!("{n:.1}% remaining")).unwrap_or_else(|| "unavailable".into())
}
fn claude_reset(window: &Value) -> Value {
    let seconds = window["resets_at"].as_str().map(|s| js_sys::Date::parse(s)/1000.0)
        .filter(|n| n.is_finite() && *n > 0.0);
    serde_json::json!({"resets_at":seconds})
}
pub fn claude(usage: &Value, connected: bool) -> Html {
    if usage["connected"] != true { return Html::default(); }
    let windows = array(&usage["windows"]);
    html!{<details class="weekly-usage claude-usage" aria-label="Claude usage">
        {quota_summary("Claude",&windows,connected,true)}
        <div class="weekly-details">
            {if connected {html!{<>
                {for windows.iter().map(|window| {
                    let period=if window["duration_minutes"]==300 {"5h"} else {"Weekly"};
                    let reset=claude_reset(window);
                    html!{<div class="weekly-window">
                        <strong>{format!("Claude · {period} · {}",claude_label(window))}</strong>
                        {if let Some(left)=claude_remaining(window){html!{<meter min="0" max="100" value={left.to_string()} aria-label={format!("Remaining Claude {period} capacity")}/>}}else{Html::default()}}
                        <small>{reset_in(&reset)}</small><small>{format!("Reset: {}",timestamp(&reset["resets_at"]))}</small>
                    </div>}
                })}
                {if let Some(error)=usage["error"].as_str(){html!{<p class="muted" role="status">{error}</p>}}else{Html::default()}}
                {if usage["checked_at"].is_number(){html!{<small>{format!("Last updated: {}",timestamp(&usage["checked_at"]))}</small>}}else{Html::default()}}
                <p class="muted">{"Account-wide usage · cached for up to 5 minutes"}</p>
            </>}}else{html!{<p class="muted">{"Reconnect to see Claude usage"}</p>}}}
        </div>
    </details>}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn compact_reset_countdown_has_no_spurious_zero_units() {
        assert_eq!(compact_countdown(Some(11100.0),0.0), "(3h5m)");
        assert_eq!(compact_countdown(Some(198300.0),0.0), "(2d7h5m)");
        assert_eq!(compact_countdown(None,0.0), "(?)");
        assert_eq!(compact_countdown(Some(10.0),20.0), "(0m)");
    }
    #[test]
    fn claude_preserves_fractional_remaining_and_unknowns() {
        assert_eq!(claude_remaining(&json!({"used_percent":12.5})), Some(87.5));
        assert_eq!(claude_label(&json!({"used_percent":100})), "0.0% remaining");
        for value in [json!({}), json!({"used_percent":-1}), json!({"used_percent":101})] {
            assert_eq!(claude_label(&value), "unavailable");
        }
    }
    #[test]
    fn available_resets_use_authoritative_count_and_explain_missing_details() {
        assert_eq!(reset_count(&Value::Null), None);
        assert_eq!(reset_count(&json!({"availableCount":-1})), None);
        let zero = json!({"availableCount":0,"credits":[]});
        assert_eq!(reset_count(&zero), Some(0));
        assert_eq!(reset_details_note(&zero), None);
        let partial = json!({"availableCount":3,"credits":[{}]});
        assert_eq!(reset_count(&partial), Some(3));
        assert_eq!(reset_details_note(&partial).as_deref(), Some("Details reported for 1 of 3 available resets"));
        assert_eq!(reset_details_note(&json!({"availableCount":2,"credits":null})).as_deref(), Some("Reset details not reported"));
        assert_eq!(reset_expiry(&json!({"expiresAt":null})), "Does not expire");
        assert_eq!(reset_expiry(&json!({})), "Expiry not reported");
    }
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
