//! System share intake. Browser storage owns the temporary payload; mutations
//! use the same authenticated actor and explicit receipts as the composer.
use super::*;
use base64::Engine;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=demodexShares, js_name=route)]
    fn route() -> Option<String>;
    #[wasm_bindgen(js_namespace=demodexShares, js_name=error)]
    fn route_error() -> String;
    #[wasm_bindgen(js_namespace=demodexShares, js_name=clearRoute)]
    fn clear_route();
    #[wasm_bindgen(js_namespace=demodexShares, js_name=load, catch)]
    async fn load(id: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexShares, js_name=begin, catch)]
    async fn begin(id: &str, host: &str, session: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexShares, js_name=progress, catch)]
    async fn progress(id: &str, index: u32, path: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexShares, js_name=finish, catch)]
    async fn finish(id: &str, error: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace=demodexShares, js_name=remove, catch)]
    async fn remove(id: &str) -> Result<JsValue, JsValue>;
}
fn js_error(value: JsValue) -> String {
    js_sys::Reflect::get(&value, &"message".into())
        .ok()
        .and_then(|v| v.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| "Could not access shared content".into())
}
#[derive(Clone)]
pub struct Record {
    data: Value,
    files: Vec<web_sys::File>,
}
impl Record {
    fn parse(value: JsValue) -> Result<Self, String> {
        if value.is_null() || value.is_undefined() {
            return Err("This share is no longer available. It may have expired or been handled in another window.".into());
        }
        let files = js_sys::Reflect::get(&value, &"files".into()).map_err(js_error)?;
        let files = js_sys::Array::from(&files)
            .iter()
            .filter_map(|f| f.dyn_into().ok())
            .collect();
        let data = js_sys::JSON::stringify(&value)
            .ok()
            .and_then(|v| v.as_string())
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or("Invalid shared content")?;
        Ok(Self { data, files })
    }
    fn id(&self) -> &str {
        text(&self.data, "id")
    }
    fn content(&self) -> String {
        let mut parts: Vec<String> = array(&self.data["fields"])
            .into_iter()
            .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
            .collect();
        for upload in array(&self.data["uploads"]) {
            if let Some(path) = upload["path"].as_str() {
                parts.push(serde_json::to_string(path).unwrap());
            }
        }
        parts.join("\n")
    }
}
#[derive(Default)]
pub struct State {
    record: Option<Record>,
    error: String,
    working: bool,
    pub creating: bool,
    pub created: Option<String>,
}
pub enum ShareMsg {
    Loaded(Result<Record, String>),
    New,
    Choose(String, bool),
    Finished(Result<Record, String>),
    Accept,
    Discard,
    Removed(Result<(), String>),
}
pub fn startup(ctx: &Context<App>) {
    if let Some(id) = route() {
        ctx.link().send_future(async move {
            Msg::Share(ShareMsg::Loaded(
                load(&id).await.map_err(js_error).and_then(Record::parse),
            ))
        });
    } else if !route_error().is_empty() {
        ctx.link()
            .send_message(Msg::Share(ShareMsg::Loaded(Err(route_error()))));
    }
}
async fn upload(
    client: Rc<Client>,
    record: Record,
    host: String,
    session: String,
) -> Result<Record, String> {
    let id = record.id().to_owned();
    let mut record = Record::parse(begin(&id, &host, &session).await.map_err(js_error)?)?;
    let result: Result<(), String> = async {
        for (index, file) in record.files.iter().enumerate() {
            let buffer = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
                .await
                .map_err(js_error)?;
            let data = base64::engine::general_purpose::STANDARD
                .encode(js_sys::Uint8Array::new(&buffer).to_vec());
            let receipt = text(&record.data["uploads"][index], "receipt").to_owned();
            let value = client
                .call(
                    Operation::UploadFile {
                        id: session.clone(),
                        name: file.name(),
                        data,
                    },
                    receipt.clone(),
                )
                .await
                .map_err(|e| {
                    format!(
                        "{}: {e:#}. Receipt: {receipt}. No upload will be retried automatically.",
                        file.name()
                    )
                })?;
            let path = value["path"]
                .as_str()
                .ok_or("Upload returned no file path")?;
            progress(&id, index as u32, path).await.map_err(js_error)?;
        }
        Ok(())
    }
    .await;
    let error = result.err().unwrap_or_default();
    record = Record::parse(finish(&id, &error).await.map_err(js_error)?)?;
    Ok(record)
}
impl App {
    pub(super) fn share_update(&mut self, ctx: &Context<Self>, msg: ShareMsg) {
        match msg {
            ShareMsg::Loaded(result) => {
                // A cold share launch opens intake even offline or before choosing a server.
                if self.share.record.is_none() {
                    self.connections_page = false;
                    self.record_navigation(true);
                }
                match result {
                    Ok(record) => self.share.record = Some(record),
                    Err(error) => self.share.error = error,
                }
            }
            ShareMsg::New => {
                if self.busy || !self.connected {
                    return;
                }
                self.share.creating = true;
                ctx.link().send_message(Msg::NewSession(true));
            }
            ShareMsg::Choose(session, created) => {
                if self.busy || !self.connected {
                    return;
                }
                if !created
                    && !self
                        .sessions
                        .iter()
                        .any(|s| text(s, "id") == session && share_session(s))
                {
                    return;
                }
                let Some(record) = self
                    .share
                    .record
                    .clone()
                    .filter(|r| r.data["status"] == "pending")
                else {
                    return;
                };
                let Some(client) = self.client.clone() else {
                    return;
                };
                self.share.working = true;
                self.share.error.clear();
                self.share.creating = false;
                self.share.created = None;
                self.busy = true;
                let host = self.saved.host.clone();
                ctx.link().send_future(async move {
                    Msg::Share(ShareMsg::Finished(
                        upload(client, record, host, session).await,
                    ))
                });
            }
            ShareMsg::Finished(result) => {
                self.busy = false;
                self.share.working = false;
                match result {
                    Ok(record) => {
                        let ready = record.data["status"] == "ready";
                        self.share.record = Some(record);
                        if ready {
                            ctx.link().send_message(Msg::Share(ShareMsg::Accept));
                        }
                    }
                    Err(error) => {
                        self.share.error = error;
                        // Refresh local journal only; never replay the operation.
                        if let Some(record) = &self.share.record {
                            let id = record.id().to_owned();
                            ctx.link().send_future(async move {
                                Msg::Share(ShareMsg::Loaded(
                                    load(&id).await.map_err(js_error).and_then(Record::parse),
                                ))
                            });
                        }
                    }
                }
            }
            ShareMsg::Accept => {
                if self.share.working {
                    return;
                }
                let Some(record) = self
                    .share
                    .record
                    .clone()
                    .filter(|r| r.data["status"] != "pending")
                else {
                    return;
                };
                let host = text(&record.data, "host");
                let session = text(&record.data, "session");
                if host.is_empty() || session.is_empty() {
                    return;
                }
                if !self.saved.applied_shares.iter().any(|id| id == record.id()) {
                    let draft = self
                        .saved
                        .drafts
                        .entry(format!("{host}:{session}"))
                        .or_default();
                    let content = record.content();
                    if !draft.is_empty() && !content.is_empty() {
                        draft.push('\n');
                    }
                    draft.push_str(&content);
                    self.saved.applied_shares.push(record.id().to_owned());
                }
                // Persist the draft and deduplication marker together before deleting the inbox.
                if !self.persist() {
                    self.share.error = self.storage_error.clone();
                    return;
                }
                if host == self.saved.host {
                    ctx.link().send_message(Msg::Select(session.into()));
                }
                self.share_remove(ctx, record.id().to_owned());
            }
            ShareMsg::Discard => {
                if self.share.working {
                    return;
                }
                if let Some(record) = &self.share.record {
                    self.share_remove(ctx, record.id().to_owned());
                } else {
                    self.share = State::default();
                    clear_route();
                }
            }
            ShareMsg::Removed(result) => match result {
                Ok(()) => {
                    self.share = State::default();
                    clear_route();
                }
                Err(error) => {
                    self.share.working = false;
                    self.share.error = error;
                }
            },
        }
    }
    fn share_remove(&mut self, ctx: &Context<Self>, id: String) {
        self.share.working = true;
        ctx.link().send_future(async move {
            Msg::Share(ShareMsg::Removed(
                remove(&id).await.map(|_| ()).map_err(js_error),
            ))
        });
    }
    pub(super) fn share_view(&self, ctx: &Context<Self>) -> Html {
        if self.share.record.is_none() && self.share.error.is_empty() {
            return Html::default();
        }
        if self.connections_page || self.show_new_session {
            return Html::default();
        }
        let action =
            |msg: fn() -> ShareMsg| ctx.link().callback(move |_: MouseEvent| Msg::Share(msg()));
        let disabled = self.busy || self.share.working || !self.connected;
        html! {<crate::modal::Modal title="Shared content" close_disabled={self.share.working} onclose={ctx.link().callback(|_|Msg::Share(ShareMsg::Discard))}>
            {if !self.share.error.is_empty(){html!{<p class="error" role="alert">{&self.share.error}</p>}}else{Html::default()}}
            {if let Some(record)=&self.share.record {html!{<>
                <crate::ui::Group title="Received">
                    {for array(&record.data["fields"]).iter().filter_map(|v|v.as_str()).filter(|s|!s.is_empty()).map(|s|html!{<pre class="share-text">{s}</pre>})}
                    {for record.files.iter().map(|f|html!{<p class="share-file">{format!("{} · {:.1} MiB",f.name(),f.size()/1048576.0)}</p>})}
                    {if record.files.iter().map(|f|f.size()).sum::<f64>()>4.0*1048576.0{html!{<p class="control-warning" role="status">{"Large files may take a while to upload. Keep this window open until the upload finishes."}</p>}}else{Html::default()}}
                </crate::ui::Group>
                {if self.share.working {html!{<p role="status">{"Adding shared content…"}</p>}}
                else if record.data["status"]=="pending" {html!{<crate::ui::Group title="Add to session">
                    <p class="muted">{"Choose a session on the current server. Content is added to its draft; Send starts the agent."}</p>
                    <button class="primary" disabled={disabled} onclick={action(||ShareMsg::New)}>{"New session"}</button>
                    <div class="share-sessions">{for self.sessions.iter().filter(|s|share_session(s)).map(|s|{
                        let id=text(s,"id").to_owned();
                        html!{<button disabled={disabled} onclick={ctx.link().callback(move |_|Msg::Share(ShareMsg::Choose(id.clone(),false)))}><strong>{crate::overview::title(s)}</strong><small>{format!("{} · {}",crate::overview::identity(s),text(s,"status"))}</small></button>}
                    })}</div>
                    {if !self.connected {html!{<p>{"Connect to a server to choose a session."}</p>}}else if !self.sessions.iter().any(share_session){html!{<p class="muted">{"No active sessions on this server."}</p>}}else{Html::default()}}
                    <button onclick={ctx.link().callback(|_|Msg::Connections)}>{"Choose server"}</button>
                </crate::ui::Group>}}
                else {html!{<crate::ui::Group title="Upload status">
                    <p>{text(&record.data,"error")}</p>
                    {if record.data["status"]!="ready"{html!{<p class="control-warning">{"Some files were not confirmed uploaded. Uploads are never retried automatically. Shared text and confirmed file paths can still be added to the original session's draft."}</p>}}else{Html::default()}}
                    {for array(&record.data["uploads"]).iter().enumerate().map(|(i,u)|html!{<p class="share-file">{format!("{}: {}",record.files.get(i).map(|f|f.name()).unwrap_or_default(),u["path"].as_str().map(str::to_owned).unwrap_or_else(||format!("unconfirmed · receipt {}",text(u,"receipt"))))}</p>})}
                    <button onclick={action(||ShareMsg::Accept)}>{"Add available content to draft"}</button>
                </crate::ui::Group>}}}
            </>}}else{Html::default()}}
            <button disabled={self.share.working} onclick={action(||ShareMsg::Discard)}>{"Discard share"}</button>
        </crate::modal::Modal>}
    }
}
fn share_session(session: &Value) -> bool {
    session["archived"] != true
        && !matches!(
            text(session, "status"),
            "disconnected" | "notLoaded" | "error" | "systemError" | ""
        )
}
