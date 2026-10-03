use crate::client::Client;
use demodex_protocol::{Operation, PushRegistration};
use serde_json::{Value, json};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=demodexPush,js_name=status,catch)]
    async fn browser_status() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexPush,js_name=enable,catch)]
    fn browser_enable(options: JsValue) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(js_namespace=demodexPush,js_name=disable,catch)]
    async fn browser_disable() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexPush,js_name=testDisplay,catch)]
    async fn browser_test_display() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexPush,js_name=watchTest,catch)]
    fn browser_watch_test(server_id: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(js_namespace=demodexPush,js_name=route,catch)]
    fn browser_route() -> Result<JsValue, JsValue>;
}
fn value(v: JsValue) -> Value {
    js_sys::JSON::stringify(&v)
        .ok()
        .and_then(|s| s.as_string())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}
fn error(v: JsValue) -> String {
    v.as_string()
        .or_else(|| {
            js_sys::Reflect::get(&v, &"message".into())
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| "Browser push operation failed".into())
}
pub fn route() -> Option<crate::model::Navigation> {
    serde_json::from_value(value(browser_route().ok()?)).ok()
}

#[derive(Properties)]
pub struct Props {
    pub client: Rc<Client>,
    pub host: String,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host && Rc::ptr_eq(&self.client, &other.client)
    }
}
pub struct Notifications {
    local: Value,
    remote: Value,
    busy: bool,
    message: String,
    hide_preview: bool,
}
pub enum Msg {
    Refresh,
    Loaded(Result<(Value, Value), String>),
    Enable,
    Disable,
    Test,
    TestDisplay,
    Preview(bool),
    Done(Result<String, String>),
}
impl Component for Notifications {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Refresh);
        Self {
            local: Value::Null,
            remote: Value::Null,
            busy: true,
            message: String::new(),
            hide_preview: false,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Props) -> bool {
        self.local = Value::Null;
        self.remote = Value::Null;
        self.message.clear();
        ctx.link().send_message(Msg::Refresh);
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Refresh => {
                self.busy = true;
                let client = ctx.props().client.clone();
                ctx.link().send_future(async move {
                    Msg::Loaded(
                        async {
                            let local = value(browser_status().await.map_err(error)?);
                            let id = local["binding"]["device_id"]
                                .as_str()
                                .unwrap_or("")
                                .to_owned();
                            let remote = client
                                .read(Operation::PushSettings { device_id: id })
                                .await
                                .map_err(|e| e.to_string())?;
                            Ok((local, remote))
                        }
                        .await,
                    )
                });
            }
            Msg::Loaded(result) => {
                self.busy = false;
                match result {
                    Ok((local, remote)) => {
                        self.hide_preview = remote["hide_preview"] == true;
                        self.local = local;
                        self.remote = remote;
                    }
                    Err(e) => self.message = e,
                }
            }
            Msg::Preview(v) => self.hide_preview = v,
            Msg::Enable => {
                self.busy = true;
                self.message.clear();
                let client = ctx.props().client.clone();
                let hide_preview = self.hide_preview;
                let options=js_sys::JSON::parse(&json!({"server_id":self.remote["server_id"],"public_key":self.remote["public_key"],"server_url":ctx.props().host}).to_string()).unwrap();
                // Invoke immediately, before scheduling, to preserve user activation.
                let pending = browser_enable(options);
                ctx.link().send_future(async move {
                    Msg::Done(
                        async {
                            let sub = value(
                                wasm_bindgen_futures::JsFuture::from(pending.map_err(error)?)
                                    .await
                                    .map_err(error)?,
                            );
                            let input = PushRegistration {
                                device_id: sub["device_id"].as_str().unwrap_or("").into(),
                                endpoint: sub["endpoint"].as_str().unwrap_or("").into(),
                                p256dh: sub["keys"]["p256dh"].as_str().unwrap_or("").into(),
                                auth: sub["keys"]["auth"].as_str().unwrap_or("").into(),
                                frontend_url: sub["frontend_url"].as_str().unwrap_or("").into(),
                                server_url: sub["server_url"].as_str().unwrap_or("").into(),
                                hide_preview,
                            };
                            command(&client, Operation::RegisterPush { input }).await?;
                            Ok("Push enabled for this server.".into())
                        }
                        .await,
                    )
                });
            }
            Msg::Disable => {
                self.busy = true;
                self.message.clear();
                let client = ctx.props().client.clone();
                let server_id = self.remote["server_id"].clone();
                ctx.link().send_future(async move {
                    Msg::Done(
                        async {
                            let old = value(browser_disable().await.map_err(error)?);
                            if old["server_id"] == server_id {
                                if let Some(id) = old["device_id"].as_str() {
                                    command(
                                        &client,
                                        Operation::RemovePush {
                                            device_id: id.into(),
                                        },
                                    )
                                    .await
                                    .map_err(|e| {
                                        format!(
                                            "Device unsubscribed; server cleanup unconfirmed: {e}"
                                        )
                                    })?;
                                }
                            }
                            Ok("Push disabled on this installation.".into())
                        }
                        .await,
                    )
                });
            }
            Msg::Test => {
                self.busy = true;
                self.message =
                    "Sending test notification; waiting for this browser to receive it…".into();
                let received = browser_watch_test(self.remote["server_id"].as_str().unwrap_or(""));
                let client = ctx.props().client.clone();
                let id = self.local["binding"]["device_id"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                ctx.link().send_future(async move {
                    Msg::Done(async {
                        let received = received.map_err(error)?;
                        let result = command(&client, Operation::TestPush { device_id: id }).await?;
                        if result["delivery"]["accepted"].as_u64().unwrap_or(0) == 0 {
                            return Ok(if result["delivery"]["expired"].as_u64().unwrap_or(0) > 0 {
                                "The push subscription expired. Disable push, then enable it again.".into()
                            } else {
                                "The push service did not confirm the test. Try again later or disable and re-enable push.".into()
                            });
                        }
                        let received = value(wasm_bindgen_futures::JsFuture::from(received).await.map_err(error)?);
                        Ok(match received["state"].as_str() {
                            Some("display-requested") => "Test received by this browser; notification display requested. If no banner appeared, check device notification settings and Do Not Disturb.".into(),
                            Some("failed") => format!("Test received, but display failed: {}", received["error"].as_str().unwrap_or("unknown browser error")),
                            _ => "Push service accepted the test, but this browser has not confirmed receipt within 20 seconds. Delivery may arrive later. Try Test notification display to check local browser and device settings.".into(),
                        })
                    }.await)
                });
            }
            Msg::TestDisplay => {
                self.busy = true;
                self.message = "Requesting a notification directly on this device…".into();
                ctx.link().send_future(async {
                    Msg::Done(browser_test_display().await.map_err(error).map(|_| {
                        "Local notification display requested. If no banner appeared, check browser and device notification settings and Do Not Disturb.".into()
                    }))
                });
            }
            Msg::Done(result) => {
                self.message = result.unwrap_or_else(|e| e);
                ctx.link().send_message(Msg::Refresh);
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let same = self.local["binding"]["server_id"] == self.remote["server_id"]
            && !self.remote["server_id"].is_null();
        let has_binding = !self.local["binding"].is_null();
        let enabled = same
            && self.local["subscribed"] == true
            && self.remote["enabled"] == true
            && self.local["permission"] == "granted";
        let available = self.local["supported"] == true;
        html! {<crate::ui::Group class="runtime-panel push-settings" label="Push notifications" title="Push notifications">

            <p class="muted">{if has_binding{format!("This installation: {}",self.local["binding"]["server_url"].as_str().unwrap_or("unknown server"))}else{"Push is off on this installation.".into()}}</p>
            {if !available && !self.busy {html!{<p class="control-warning">{"Push is unavailable here. On iPhone or iPad, install Demodex on the Home Screen first."}</p>}}else{Html::default()}}
            {if self.local["permission"]=="denied" {html!{<p class="control-warning">{"Notifications are blocked in browser or device settings."}</p>}}else{Html::default()}}
            {if has_binding && !same {html!{<p class="control-warning">{"Disable push for the previous server before enabling this one."}</p>}}else{Html::default()}}
            <label class="checkbox"><input type="checkbox" checked={self.hide_preview} disabled={self.busy||!available||(has_binding&&!same)} onchange={ctx.link().callback(|e:Event|Msg::Preview(e.target_unchecked_into::<web_sys::HtmlInputElement>().checked()))}/>{"Hide message previews"}</label>
            <div class="actions">
                <button disabled={self.busy||!available||(has_binding&&!same)||self.remote["public_key"].is_null()} onclick={ctx.link().callback(|_|Msg::Enable)}>{if enabled{"Save push settings"}else{"Enable push on this device"}}</button>
                {if has_binding {html!{<button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Disable)}>{"Disable push"}</button>}}else{Html::default()}}
                <button disabled={self.busy||!enabled} onclick={ctx.link().callback(|_|Msg::Test)}>{"Send test notification"}</button>
                <button disabled={self.busy||!available||self.local["permission"]!="granted"} onclick={ctx.link().callback(|_|Msg::TestDisplay)}>{"Test notification display"}</button>
            </div>
            {if !self.message.is_empty(){html!{<p role="status">{&self.message}</p>}}else{Html::default()}}
            <p class="muted">{"Agents can send notifications explicitly; every notification remains in chat. Agent notifications require a newly created Codex thread."}</p>
        </crate::ui::Group>}
    }
}
async fn command(client: &Client, operation: Operation) -> Result<Value, String> {
    let id = uuid::Uuid::new_v4().to_string();
    client
        .call(operation, id.clone())
        .await
        .map_err(|e| format!("{e}. Receipt: {id}. This operation was not replayed."))
}
