//! Display durable server recording times without inventing historical dates.
use serde_json::Value;
use yew::prelude::*;

pub fn view(item: &Value) -> Html {
    let at = item["_demodexAt"].as_str().unwrap_or("");
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(at));
    if !date.get_time().is_finite() {
        return html!{<span class="message-time" title="The original message time is unavailable">{"Time unknown"}</span>};
    }
    let imported = item["_demodexTimeSource"] == "imported";
    // Avoid constructing an Intl formatter for every row in large histories.
    let local = format!("{:04}-{:02}-{:02} {:02}:{:02}", date.get_full_year(), date.get_month()+1, date.get_date(), date.get_hours(), date.get_minutes());
    let full = date.to_string().as_string().unwrap_or_else(||at.into());
    let title = if imported {format!("Imported {full}. Original message time unavailable.")} else {format!("First recorded by Demodex: {full}")};
    html!{<time class="message-time" datetime={at.to_owned()} title={title}>{if imported{format!("Imported {local}")}else{local}}</time>}
}
