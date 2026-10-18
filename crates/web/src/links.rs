use crate::{
    client::Client,
    model::{array, text},
};
use base64::Engine;
use demodex_protocol::{LinkDestination, MessageLink, Operation};
use serde_json::Value;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = demodexRich, js_name = download, catch)]
    fn download_bytes(encoded: &str, name: &str) -> Result<(), JsValue>;
}

#[derive(Clone, Default)]
pub struct LinkContext {
    pub client: Option<Rc<Client>>,
    pub session: String,
    pub targets: Value,
    pub connected: bool,
}
impl PartialEq for LinkContext {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.targets == other.targets
            && self.connected == other.connected
            && match (&self.client, &other.client) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

pub fn icon(kind: &LinkDestination) -> &'static str {
    match kind {
        LinkDestination::Web { .. } => "↗",
        LinkDestination::File { .. } => "▤",
        _ => "⚠",
    }
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub link: MessageLink,
    pub item: String,
    pub revision: u64,
    pub onclose: Callback<()>,
}

#[function_component(LinkPopup)]
pub fn popup(props: &Props) -> Html {
    html! {<crate::modal::Modal title={props.link.title.clone()} compact=true icon_close=true dismiss_outside=true onclose={props.onclose.clone()}>
        {match &props.link.kind {
            LinkDestination::Web { url, host, warnings } => {
                // The parser serialized the href and hostname together. Link text
                // always shows exactly that href, including its full authority.
                let host_start=url.find("://").unwrap()+3;
                let host_end=host_start+host.len();
                let mismatch=matches!(demodex_protocol::links::classify(&props.link.title),LinkDestination::Web {host: label_host,..} if label_host != *host);
                html!{<div class="web-link-popup">
                    <p>{"Open website:"}</p><a class="link-destination" href={url.clone()} target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer">{&url[..host_start]}<strong>{host}</strong>{&url[host_end..]}</a>
                    {for warnings.iter().map(|warning|html!{<p class="link-warning">{warning}</p>})}
                    {if mismatch {html!{<p class="link-warning">{"The link label names a different website."}</p>}}else{Html::default()}}
                    {if url != &props.link.destination {html!{<p>{"Original destination:"}<br/><span class="link-destination">{&props.link.destination}</span></p>}}else{Html::default()}}
                </div>}
            }
            LinkDestination::File { .. } => html!{<FilePopup destination={props.link.destination.clone()} item={props.item.clone()} revision={props.revision}/>},
            LinkDestination::Unsupported { reason } => html!{<><p class="link-warning">{reason}</p><pre class="link-destination">{&props.link.destination}</pre></>},
        }}
    </crate::modal::Modal>}
}

#[derive(Properties, PartialEq)]
struct FileProps {
    destination: String,
    item: String,
    revision: u64,
}
enum Msg {
    Context(LinkContext),
    Refresh,
    Recheck,
    ChooseHost(String),
    HostsLoaded(u64, Result<Value, String>),
    SelectHost(String),
    BindHost,
    CancelHost,
    Loaded(u64, Result<Value, String>),
    Read(String, bool),
    ReadDone(u64, bool, Result<Value, String>),
    ClosePreview,
    DownloadPreview,
    Pretty,
    Tick,
}
struct FilePopup {
    context: LinkContext,
    _subscription: Option<ContextHandle<LinkContext>>,
    _clock: gloo::timers::callback::Interval,
    serial: u64,
    files: Value,
    loading: bool,
    reading: bool,
    error: String,
    preview: Option<Value>,
    pretty: bool,
    pairing: Option<String>,
    hosts: Value,
    selected_host: String,
    host_serial: u64,
}

fn date(ms: &Value) -> String {
    ms.as_f64()
        .filter(|n| n.is_finite())
        .map(|n| {
            js_sys::Date::new(&JsValue::from_f64(n))
                .to_string()
                .as_string()
                .unwrap_or_default()
        })
        .unwrap_or_else(|| "—".into())
}
fn checked(check: &Value) -> String {
    let Some(ms) = check["checked_at_ms"].as_f64() else {
        return "Not checked".into();
    };
    let secs = ((js_sys::Date::now() - ms).max(0.) / 1000.) as u64;
    let age = if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    };
    let label = match text(check, "state") {
        "timed-out" => "Check timed out",
        "error" => "Check failed",
        _ => "Checked",
    };
    format!("{label} {age} · {}", date(&check["checked_at_ms"]))
}
fn download(value: &Value) -> Result<(), String> {
    let name = text(value, "path").rsplit('/').next().unwrap_or("download");
    download_bytes(text(value, "dataBase64"), name)
        .map_err(|_| "Download could not be started".into())
}

impl Component for FilePopup {
    type Message = Msg;
    type Properties = FileProps;
    fn create(ctx: &Context<Self>) -> Self {
        let (context, subscription) = ctx
            .link()
            .context::<LinkContext>(ctx.link().callback(Msg::Context))
            .map(|(v, h)| (v, Some(h)))
            .unwrap_or_default();
        let link = ctx.link().clone();
        ctx.link().send_message(Msg::Refresh);
        Self {
            context,
            _subscription: subscription,
            _clock: gloo::timers::callback::Interval::new(1000, move || {
                link.send_message(Msg::Tick)
            }),
            serial: 0,
            files: Value::Null,
            loading: false,
            reading: false,
            error: String::new(),
            preview: None,
            pretty: false,
            pairing: None,
            hosts: Value::Null,
            selected_host: String::new(),
            host_serial: 0,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &FileProps) -> bool {
        if old.item != ctx.props().item || old.destination != ctx.props().destination { self.pairing = None; }
        ctx.link().send_message(Msg::Refresh);
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Tick => {}
            Msg::Context(value) => {
                let changed_client = match (&value.client, &self.context.client) {
                    (Some(a), Some(b)) => !Rc::ptr_eq(a,b),
                    (None, None) => false,
                    _ => true,
                };
                if value.session != self.context.session || !value.connected || changed_client { self.pairing = None; }
                self.context = value;
                self.preview = None;
                ctx.link().send_message(Msg::Refresh);
            }
            Msg::ChooseHost(executor) => {
                if self.loading || self.reading { return false; }
                self.error.clear();
                self.host_serial += 1;
                self.pairing = Some(executor);
                self.selected_host.clear();
                self.hosts = Value::Null;
                if let Some(client) = self.context.client.clone().filter(|_|self.context.connected) {
                    let serial = self.host_serial;
                    ctx.link().send_future(async move { Msg::HostsLoaded(serial, client.read(Operation::Targets).await.map_err(|e|format!("{e:#}"))) });
                }
            }
            Msg::HostsLoaded(serial, result) => {
                if serial != self.host_serial || self.pairing.is_none() { return false; }
                match result { Ok(value) => self.hosts = value, Err(error) => self.error = error }
            }
            Msg::SelectHost(target) => self.selected_host = target,
            Msg::CancelHost => self.pairing = None,
            Msg::BindHost => {
                if self.loading || self.reading || !self.context.connected || self.selected_host.is_empty() { return false; }
                if let (Some(client), Some(executor)) = (self.context.client.clone(), self.pairing.take()) {
                    self.serial += 1;
                    let serial = self.serial;
                    self.loading = true;
                    self.error.clear();
                    self.preview = None;
                    let id = self.context.session.clone();
                    let item = ctx.props().item.clone();
                    let destination = ctx.props().destination.clone();
                    let target = self.selected_host.clone();
                    ctx.link().send_future(async move {
                        Msg::Loaded(serial, client.call(Operation::BindMessageFileHost { id, item, destination, executor, target }, uuid::Uuid::new_v4().to_string()).await.map_err(|e|format!("{e:#}")))
                    });
                }
            }
            Msg::Recheck => {
                if self.loading || self.reading || (self.context.client.is_none() || !self.context.connected) { return false; }
                if let Some(client) = self.context.client.clone().filter(|_|self.context.connected) {
                    self.serial += 1;
                    let serial = self.serial;
                    self.loading = true;
                    self.preview = None;
                    self.error.clear();
                    let id = self.context.session.clone();
                    let item = ctx.props().item.clone();
                    let destination = ctx.props().destination.clone();
                    ctx.link().send_future(async move {
                        Msg::Loaded(serial, client.call(Operation::RefreshMessageFile { id, item, destination }, uuid::Uuid::new_v4().to_string()).await.map_err(|e| format!("{e:#}")))
                    });
                }
            }
            Msg::Refresh => {
                self.serial += 1;
                self.error.clear();
                self.reading = false;
                if let Some(client) = self
                    .context
                    .client
                    .clone()
                    .filter(|_|self.context.connected)
                {
                    let id = self.context.session.clone();
                    let item = ctx.props().item.clone();
                    let serial = self.serial;
                    self.loading = true;
                    ctx.link().send_future(async move {
                        Msg::Loaded(
                            serial,
                            client
                                .read(Operation::MessageFiles { id, item })
                                .await
                                .map_err(|e| format!("{e:#}")),
                        )
                    });
                } else {
                    self.loading = false;
                    self.error = "Connect to the server to view file checks".into();
                }
            }
            Msg::Loaded(serial, result) => {
                if serial != self.serial {
                    return false;
                }
                self.loading = false;
                match result {
                    Ok(value) => self.files = value,
                    Err(error) => self.error = error,
                }
            }
            Msg::Read(executor, show) => {
                if self.reading || self.loading {
                    return false;
                }
                if let Some(client) = self
                    .context
                    .client
                    .clone()
                    .filter(|_|self.context.connected)
                {
                    self.reading = true;
                    self.error.clear();
                    let id = self.context.session.clone();
                    let item = ctx.props().item.clone();
                    let destination = ctx.props().destination.clone();
                    let serial = self.serial;
                    ctx.link().send_future(async move {
                        Msg::ReadDone(
                            serial,
                            show,
                            client
                                .read(Operation::ReadMessageFile {
                                    id,
                                    item,
                                    destination,
                                    executor,
                                })
                                .await
                                .map_err(|e| format!("{e:#}")),
                        )
                    });
                }
            }
            Msg::ReadDone(serial, show, result) => {
                if serial != self.serial {
                    return false;
                }
                self.reading = false;
                match result {
                    Ok(value) => {
                        if show {
                            self.pretty = false;
                            self.preview = Some(value);
                        } else if let Err(error) = download(&value) {
                            self.error = error;
                        }
                    }
                    Err(error) => self.error = error,
                }
            }
            Msg::ClosePreview => self.preview = None,
            Msg::DownloadPreview => {
                if let Some(value) = &self.preview
                    && let Err(error) = download(value)
                {
                    self.error = error;
                }
            }
            Msg::Pretty => self.pretty = !self.pretty,
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let file = array(&self.files["files"])
            .into_iter()
            .find(|f| f["destination"] == ctx.props().destination);
        html! {<div class="file-link-popup">
            <div class="link-destination">{&ctx.props().destination}</div>
            <button type="button" disabled={self.loading || self.reading || (self.context.client.is_none() || !self.context.connected) || !file.as_ref().is_some_and(|f| array(&f["checks"]).iter().any(|c| c["available"] == true))} onclick={ctx.link().callback(|_|Msg::Recheck)}>{"Refresh"}</button>
            {if self.loading {html!{<p role="status">{"Loading file checks…"}</p>}}else{Html::default()}}
            {if !self.error.is_empty(){html!{<p class="error" role="alert">{&self.error}</p>}}else{Html::default()}}
            {if let Some(file)=file {
                html!{<>
                    {if let Some(note)=file["note"].as_str(){html!{<p>{note}</p>}}else{Html::default()}}
                    <div class="file-matches">{for array(&file["checks"]).iter().map(|check|{
                        let executor=text(check,"executor").to_owned();
                        let attached=self.context.connected && check["available"]==true;
                        let regular=check["metadata"]["isFile"]==true;
                        let enabled=attached && regular && check["state"]=="found" && !self.reading && !self.loading;
                        let show=executor.clone();let save=executor.clone();let pair=executor.clone();
                        html!{<crate::ui::Group class={classes!("file-match",(!attached).then_some("executor-unavailable"))}>
                            <div class="file-match-heading"><strong title={executor.clone()}>{check["executor_name"].as_str().filter(|name|!name.is_empty()).unwrap_or(&executor)}</strong><span class="file-actions"><crate::ui::IconButton title="Show file" label="Show file" disabled={!enabled} onclick={ctx.link().callback(move |_|Msg::Read(show.clone(),true))}>{"◉"}</crate::ui::IconButton><crate::ui::IconButton title="Download file" label="Download file" disabled={!enabled} onclick={ctx.link().callback(move |_|Msg::Read(save.clone(),false))}>{"↓"}</crate::ui::IconButton></span></div>
                            <div class="link-destination">{text(check,"path")}</div>
                            {if check["state"]=="found" {html!{<><div class="file-metadata"><span>{format!("Created: {}",date(&check["metadata"]["createdAtMs"]))}</span><span>{format!("Modified: {}",date(&check["metadata"]["modifiedAtMs"]))}</span><span>{check["metadata"]["size"].as_u64().map(|n|format!("{n} bytes")).unwrap_or_else(||"Size unknown".into())}</span></div>{if !regular{html!{<p>{"Not a regular file; preview and download unavailable"}</p>}}else{Html::default()}}</>}}
                            else{html!{<p class="file-check-state">{match text(check,"state"){"not-found"=>"Not found", "timed-out"=>"Timed out", "pending"=>"Checking…", "not-checked"=>"Not checked", _=>"Check failed"}}{if check["state"]!="not-found"{format!(" · {}",text(check,"error"))}else{String::new()}}</p>}}}
                            {if check["host"].is_null() {html!{<button type="button" disabled={self.loading || self.reading || (self.context.client.is_none() || !self.context.connected)} onclick={ctx.link().callback(move |_|Msg::ChooseHost(pair.clone()))}>{"Choose file host…"}</button>}}else{html!{<p class="muted">{"Saved host identity; session connection is not required."}</p>}}}
                            <small class="file-check-time">{checked(check)}</small>
                            {if !attached{html!{<p class="muted">{check["unavailable_reason"].as_str().unwrap_or("Original file host is unavailable.")}</p>}}else{Html::default()}}
                        </crate::ui::Group>}
                    })}</div>
                </>}
            }else if !self.loading {html!{<p>{self.files["note"].as_str().unwrap_or("No metadata snapshot for this link. Checks run when an assistant message completes.")}</p>}}else{Html::default()}}
            {if self.reading {html!{<p role="status">{"Reading current file…"}</p>}}else{Html::default()}}
            <p class="muted">{"Metadata reflects the check time. Refresh rechecks the saved file host. Host identity is verified before access. Show and Download read the current file (up to 4 MiB)."}</p>
            {if self.pairing.is_some() {html!{<crate::modal::Modal title="Choose file host" compact=true onclose={ctx.link().callback(|_|Msg::CancelHost)}>
                <p>{"This link has no saved machine identity. Choose the machine that originally held this file. Demodex will check this path there and remember its machine and account identity."}</p>
                <div class="link-destination">{&ctx.props().destination}</div>
                {if !self.error.is_empty() {html!{<p class="error" role="alert">{&self.error}</p>}}else if self.hosts.is_null() {html!{<p role="status">{"Loading hosts…"}</p>}}else{Html::default()}}
                <label>{"File host"}<select aria-label="File host" value={self.selected_host.clone()} onchange={ctx.link().callback(|e:Event|Msg::SelectHost(e.target_unchecked_into::<web_sys::HtmlSelectElement>().value()))}>
                    <option value="">{"Choose a host"}</option>
                    {for array(&self.hosts).iter().map(|host|html!{<option value={text(host,"id").to_owned()}>{format!("{} ({})",text(host,"name"),text(host,"id"))}</option>})}
                </select></label>
                <button type="button" disabled={self.selected_host.is_empty() || !self.context.connected || self.loading || self.reading} onclick={ctx.link().callback(|_|Msg::BindHost)}>{"Use this host"}</button>
            </crate::modal::Modal>}}else{Html::default()}}
            {self.preview.as_ref().map(|value|self.preview_view(ctx,value)).unwrap_or_default()}
        </div>}
    }
}

impl FilePopup {
    fn preview_view(&self, ctx: &Context<Self>, value: &Value) -> Html {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text(value, "dataBase64"))
            .unwrap_or_default();
        let image_mime = raster_mime(&bytes);
        let source = std::str::from_utf8(&bytes)
            .ok()
            .filter(|s| !s.contains('\0'));
        let json = source
            .filter(|s| s.len() <= 256 * 1024)
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let display = if self.pretty {
            json.as_ref()
                .and_then(|v| serde_json::to_string_pretty(v).ok())
        } else {
            source.map(str::to_owned)
        };
        let display = display.map(|s| s.chars().take(256 * 1024).collect::<String>());
        html! {<crate::modal::Modal title="File preview" icon_close=true dismiss_outside=true onclose={ctx.link().callback(|_|Msg::ClosePreview)}>
            <div class="link-destination">{text(value,"path")}</div><p class="muted">{format!("Read {} · {} bytes",date(&value["read_at_ms"]),bytes.len())}</p>
            <button type="button" title="Download these bytes" aria-label="Download these bytes" onclick={ctx.link().callback(|_|Msg::DownloadPreview)}>{"↓"}</button>
            {if json.is_some(){html!{<button type="button" onclick={ctx.link().callback(|_|Msg::Pretty)}>{if self.pretty{"Raw JSON"}else{"Pretty JSON"}}</button>}}else{Html::default()}}
            {if image_mime.is_none() && bytes.len()>256*1024{html!{<p>{"Preview shortened. Download contains the complete file."}</p>}}else{Html::default()}}
            {if let Some(mime)=image_mime {
                html!{<FileImage key={text(value,"dataBase64").to_owned()} src={format!("data:{mime};base64,{}",text(value,"dataBase64"))} path={text(value,"path").to_owned()}/>}
            }else if let Some(svg)=source.filter(|_| text(value,"path").to_ascii_lowercase().ends_with(".svg")) {
                html!{<crate::svg::SvgPreview source={svg.to_owned()}/>}
            }else if let Some(display)=display {html!{<pre class="file-preview">{display}</pre>}}else{html!{<p>{"Binary file. Download to inspect its contents."}</p>}}}
        </crate::modal::Modal>}
    }
}

// Sniff bytes rather than trusting the filename or a supplied MIME label.
fn raster_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

#[derive(Properties, PartialEq)]
struct FileImageProps {
    src: String,
    path: String,
}

#[function_component(FileImage)]
fn file_image(props: &FileImageProps) -> Html {
    let failed = use_state(|| false);
    let onerror = {
        let failed = failed.clone();
        Callback::from(move |_| failed.set(true))
    };
    html! {<>
        <img class="image-expanded" src={props.src.clone()} alt={props.path.clone()} {onerror}/>
        {if *failed {html!{<p role="alert">{"Image could not be decoded. Download to inspect its contents."}</p>}}else{Html::default()}}
    </>}
}
