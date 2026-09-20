use crate::{
    client::{Client, Snapshot, Wake},
    model::*,
};
use demodex_protocol::Operation;
use gloo::{events::EventListener, timers::callback::Timeout};
use serde_json::{Value, json};
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

fn window() -> web_sys::Window {
    web_sys::window().unwrap()
}
fn default_host() -> String {
    if option_env!("DEMODEX_STANDALONE") == Some("1") {
        String::new()
    } else {
        window().location().origin().unwrap_or_default()
    }
}
fn storage_get(key: &str) -> Option<String> {
    window()
        .session_storage()
        .ok()
        .flatten()?
        .get_item(key)
        .ok()
        .flatten()
}
fn storage_set(key: &str, value: &str) -> bool {
    window()
        .session_storage()
        .ok()
        .flatten()
        .is_some_and(|s| s.set_item(key, value).is_ok())
}
fn input(event: InputEvent) -> String {
    let target = event.target().unwrap();
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        input.value()
    } else {
        target.unchecked_into::<HtmlTextAreaElement>().value()
    }
}

pub struct App {
    saved: Saved,
    host_input: String,
    token: String,
    hosts: Vec<String>,
    connections: Vec<Connection>,
    connection_name: String,
    connections_page: bool,
    connection_storage_error: String,
    client: Option<Rc<Client>>,
    generation: u64,
    connected: bool,
    connecting: bool,
    retry: Option<Timeout>,
    _listeners: Vec<EventListener>,
    sessions: Vec<Value>,
    runtime: Value,
    environments: Vec<Value>,
    current: Value,
    pending: Vec<Value>,
    events: Vec<Value>,
    refreshing: bool,
    refresh_again: bool,
    busy: bool,
    error: String,
    storage_error: String,
    receipt: String,
    saved_threads: Vec<Value>,
    cursor: Option<String>,
    login: Value,
    transcript_ref: NodeRef,
    prompt_ref: NodeRef,
    image_ref: NodeRef,
    upload_anchor: Option<(String, String, u32, u32)>,
    follow: bool,
    update_available: bool,
    updating: bool,
    update_error: String,
}

pub enum Msg {
    HostInput(String),
    Token(String),
    Connect,
    Connections,
    BackToHost,
    History(Navigation),
    Back,
    ConnectionName(String),
    UseConnection(String, bool),
    ForgetConnection(String),
    Connected(u64, Result<Rc<Client>, String>),
    Wake(u64, bool),
    Retry,
    Refresh,
    Snapshot(u64, Result<Snapshot, String>),
    Select(String),
    Page(String),
    Field(String, String),
    Draft(String),
    ChooseImage,
    UploadImage(web_sys::File),
    ImageRead(u64, String, Result<Vec<u8>, String>),
    AnswerDraft(String, String),
    Run(Operation),
    Completed(u64, Operation, String, Result<Value, String>),
    LoadSaved(bool),
    SavedThreads(u64, Result<Value, String>, bool),
    ChooseThread(Value),
    Answer(Value, Option<String>),
    Dismiss,
    Scroll,
    Pwa,
    ApplyUpdate,
}

impl App {
    fn navigation(&self) -> Navigation {
        Navigation { host: self.saved.host.clone(), selected: self.saved.selected.clone(),
            page: self.saved.page.clone(), connections: self.connections_page }
    }
    fn record_navigation(&mut self, replace: bool) {
        let result = (|| {
            let state = wasm_bindgen::JsValue::from_str(&serde_json::to_string(&self.navigation()).ok()?);
            let history = window().history().ok()?;
            if replace { history.replace_state_with_url(&state, "", None).ok()?; }
            else { history.push_state_with_url(&state, "", None).ok()?; }
            Some(())
        })();
        if result.is_none() { self.error = "Browser navigation could not be updated.".into(); }
    }
    fn store_connections(&mut self) {
        let ok = window().local_storage().ok().flatten().is_some_and(|storage| {
            serde_json::to_string(&self.connections).ok().is_some_and(|value| {
                storage.set_item("demodex-connections", &value).is_ok()
            })
        });
        self.connection_storage_error = if ok { String::new() } else {
            "Connections could not be saved on this device. They will be lost when you close the app.".into()
        };
    }
    fn persist(&mut self) -> bool {
        let ok = serde_json::to_string(&self.saved)
            .ok()
            .is_some_and(|s| storage_set("demodex-rust-view", &s));
        self.storage_error = if ok {
            String::new()
        } else {
            "Drafts could not be saved. Copy them before reloading.".into()
        };
        ok
    }
    fn token_key(&self) -> String {
        format!("demodex-token:{}", self.saved.host)
    }
    fn request(&mut self, ctx: &Context<Self>, operation: Operation) {
        let Some(client) = self.client.clone().filter(|_| self.connected) else {
            self.error = "Connect to a host first".into();
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        self.error.clear();
        let generation = self.generation;
        let id = uuid::Uuid::new_v4().to_string();
        if operation.is_mutation() {
            self.receipt = id.clone();
            self.saved
                .receipts
                .insert(self.saved.host.clone(), id.clone());
            self.persist();
        }
        ctx.link().send_future(async move {
            let result = client
                .call(operation.clone(), id.clone())
                .await
                .map_err(|e| format!("{e:#}"));
            Msg::Completed(generation, operation, id, result)
        });
    }
    fn pwa(&mut self) {
        let state = js_sys::Reflect::get(&window(), &"demodexPwaState".into()).unwrap_or_default();
        self.update_available = js_sys::Reflect::get(&state, &"available".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.updating = js_sys::Reflect::get(&state, &"applying".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.update_error = js_sys::Reflect::get(&state, &"error".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
    }
    fn field(&self, ctx: &Context<Self>, name: &str, label: &str, placeholder: &str) -> Html {
        let key = name.to_owned();
        html! {<label>{label.to_owned()}<input value={self.saved.field(name)} placeholder={placeholder.to_owned()} oninput={ctx.link().callback(move |e|Msg::Field(key.clone(),input(e)))}/></label>}
    }
    fn sandbox(&self, ctx: &Context<Self>, key: &str, label: &str) -> Html {
        let name = key.to_owned();
        let selected = self.saved.field(key);
        html! {<><label>{label.to_owned()}<select value={selected.clone()} onchange={ctx.link().callback(move |e:Event|Msg::Field(name.clone(),e.target_unchecked_into::<HtmlSelectElement>().value()))}>
            <option value="">{"Codex default / saved policy"}</option><option value="read-only">{"Read-only"}</option><option value="workspace-write">{"Workspace-write"}</option><option value="danger-full-access">{"Danger-full-access"}</option>
        </select></label>{if selected=="danger-full-access" {html!{<p class="muted">{"Full access to this execution environment. Host mode uses the service user's permissions."}</p>}}else{Html::default()}}</>}
    }
    fn button(&self, ctx: &Context<Self>, label: &str, operation: Operation) -> Html {
        html! {<button type="button" disabled={self.busy||!self.connected} onclick={ctx.link().callback(move |_|Msg::Run(operation.clone()))}>{label.to_owned()}</button>}
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();
    fn create(ctx: &Context<Self>) -> Self {
        let mut saved: Saved = storage_get("demodex-rust-view")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| {
                Saved::legacy(
                    storage_get("demodex-view")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    default_host(),
                )
            });
        if saved.host.is_empty() {
            saved.host = default_host();
        }
        // Retain the existing token when migrating a same-origin Svelte installation.
        let token = storage_get(&format!("demodex-token:{}", saved.host))
            .or_else(|| {
                if saved.host == window().location().origin().unwrap_or_default() {
                    storage_get("demodex-token")
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let hosts = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item("demodex-hosts").ok().flatten())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let connections: Vec<Connection> = window().local_storage().ok().flatten()
            .and_then(|s| s.get_item("demodex-connections").ok().flatten())
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let connection_name = connections.iter().find(|c| c.url == saved.host)
            .map(|c| c.name.clone()).unwrap_or_default();
        let foreground = ctx.link().clone();
        let pwa = ctx.link().clone();
        let navigation = ctx.link().clone();
        let listeners = vec![
            EventListener::new(&window(), "popstate", move |event| {
                if let Some(route) = event.dyn_ref::<web_sys::PopStateEvent>()
                    .and_then(|e| e.state().as_string())
                    .and_then(|s| serde_json::from_str::<Navigation>(&s).ok()) {
                    navigation.send_message(Msg::History(route));
                }
            }),
            EventListener::new(&window(), "online", move |_| {
                foreground.send_message(Msg::Retry)
            }),
            EventListener::new(&window(), "demodex-update-state", move |_| {
                pwa.send_message(Msg::Pwa)
            }),
        ];
        let mut app = Self {
            host_input: saved.host.clone(),
            saved,
            token,
            hosts,
            connections,
            connection_name,
            connections_page: true,
            connection_storage_error: String::new(),
            client: None,
            generation: 0,
            connected: false,
            connecting: false,
            retry: None,
            _listeners: listeners,
            sessions: vec![],
            runtime: Value::Null,
            environments: vec![],
            current: Value::Null,
            pending: vec![],
            events: vec![],
            refreshing: false,
            refresh_again: false,
            busy: false,
            error: String::new(),
            storage_error: String::new(),
            receipt: String::new(),
            saved_threads: vec![],
            cursor: None,
            login: Value::Null,
            transcript_ref: NodeRef::default(),
            prompt_ref: NodeRef::default(),
            image_ref: NodeRef::default(),
            upload_anchor: None,
            follow: true,
            update_available: false,
            updating: false,
            update_error: String::new(),
        };
        app.pwa();
        let restored = window().history().ok().and_then(|h| h.state().ok())
            .and_then(|s| s.as_string()).and_then(|s| serde_json::from_str::<Navigation>(&s).ok());
        if let Some(route) = &restored {
            if route.host == app.saved.host {
                app.connections_page = route.connections;
                app.saved.selected = route.selected.clone();
                app.saved.page = route.page.clone();
            }
        }
        app.record_navigation(true);
        if let Some(id) = app.saved.receipts.get(&app.saved.host) {
            app.receipt = id.clone();
            app.error="A previous command may have been accepted. Check its receipt before sending it again.".into();
        }
        if !app.saved.host.is_empty() && restored.as_ref().is_none_or(|route| !route.connections) {
            ctx.link().send_message(Msg::Connect);
        }
        app
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        let previous_navigation = self.navigation();
        let navigates = matches!(&msg, Msg::Connections | Msg::BackToHost | Msg::Select(_) | Msg::Page(_) | Msg::Connected(_, Ok(_)));
        match msg {
            Msg::Back => {
                if window().history().and_then(|h| h.back()).is_err() {
                    self.error = "Browser back navigation failed.".into();
                }
            }
            Msg::History(route) => {
                if self.connecting {
                    self.generation += 1;
                    self.connecting = false;
                    self.retry = None;
                }
                if route.host != self.saved.host {
                    // A previous host's session IDs must never be sent to the
                    // current daemon. Return to explicit connection selection.
                    self.connections_page = true;
                    self.record_navigation(true);
                } else {
                    self.connections_page = route.connections;
                    self.saved.selected = route.selected;
                    self.saved.page = route.page;
                    self.current = Value::Null;
                    self.events.clear();
                    self.pending.clear();
                    self.follow = true;
                    ctx.link().send_message(Msg::Refresh);
                }
            }
            Msg::Connections => self.connections_page = true,
            Msg::BackToHost => self.connections_page = false,
            Msg::ConnectionName(value) => self.connection_name = value,
            Msg::UseConnection(url, connect) => {
                if self.connecting || self.busy { return false; }
                if let Some(connection) = self.connections.iter().find(|c| c.url == url) {
                    self.host_input = connection.url.clone();
                    self.token = connection.token.clone();
                    self.connection_name = connection.name.clone();
                    if connect { ctx.link().send_message(Msg::Connect); }
                }
            }
            Msg::ForgetConnection(url) => {
                if self.connecting || self.busy { return false; }
                self.connections.retain(|c| c.url != url);
                self.hosts.retain(|host| host != &url);
                if let Ok(Some(storage)) = window().session_storage() {
                    let _ = storage.remove_item(&format!("demodex-token:{url}"));
                    if url == window().location().origin().unwrap_or_default() {
                        let _ = storage.remove_item("demodex-token");
                    }
                }
                if url == self.saved.host {
                    self.generation += 1;
                    self.retry = None;
                    self.client = None;
                    self.connected = false;
                    self.refreshing = false;
                }
                if url == self.host_input.trim_end_matches('/') {
                    self.token.clear();
                    self.connection_name.clear();
                }
                self.store_connections();
            }
            Msg::HostInput(value) => {
                if self.connecting || self.busy {
                    return false;
                }
                self.host_input = value;
                self.token = storage_get(&format!(
                    "demodex-token:{}",
                    self.host_input.trim_end_matches('/')
                ))
                .unwrap_or_default();
                let connection = self.connections.iter().find(|c| c.url == self.host_input.trim().trim_end_matches('/'));
                self.connection_name = connection.map(|c| c.name.clone()).unwrap_or_default();
                if let Some(connection) = connection { self.token = connection.token.clone(); }
            }
            Msg::Token(value) => {
                if self.connecting || self.busy {
                    return false;
                }
                self.token = value;
            }
            Msg::Connect => {
                if self.connecting {
                    return false;
                }
                let host = self.host_input.trim().trim_end_matches('/').to_owned();
                if host != self.saved.host {
                    self.saved.selected.clear();
                    self.saved.page.clear();
                    self.events.clear();
                    self.current = Value::Null;
                    self.sessions.clear();
                    self.saved_threads.clear();
                    self.pending.clear();
                    self.runtime = Value::Null;
                    self.environments.clear();
                    self.receipt.clear();
                    self.saved.fields.clear();
                    self.cursor = None;
                    self.login = Value::Null;
                }
                self.saved.host = host.clone();
                self.generation += 1;
                self.client = None;
                self.connected = false;
                self.connecting = true;
                self.refreshing = false;
                self.refresh_again = false;
                self.busy = false;
                self.upload_anchor = None;
                self.retry = None;
                let generation = self.generation;
                let token = self.token.clone();
                let link = ctx.link().clone();
                ctx.link().send_future(async move {
                    match Client::connect(&host, token).await {
                        Ok((client, mut rx)) => {
                            wasm_bindgen_futures::spawn_local(async move {
                                while let Some(wake) = rx.recv().await {
                                    let closed = matches!(wake, Wake::Closed);
                                    link.send_message(Msg::Wake(generation, closed));
                                    if closed {
                                        break;
                                    }
                                }
                            });
                            Msg::Connected(generation, Ok(client))
                        }
                        Err(error) => Msg::Connected(generation, Err(format!("{error:#}"))),
                    }
                });
            }
            Msg::Connected(generation, result) => {
                if generation != self.generation {
                    return false;
                }
                self.connecting = false;
                match result {
                    Ok(client) => {
                        self.connections_page = false;
                        let connection = Connection {
                            name: self.connection_name.trim().to_owned(),
                            url: self.saved.host.clone(), token: self.token.clone(),
                        };
                        if let Some(existing) = self.connections.iter_mut().find(|c| c.url == connection.url) {
                            *existing = connection;
                        } else { self.connections.push(connection); }
                        self.store_connections();
                        if self.receipt.is_empty() {
                            self.error.clear();
                        }
                        self.client = Some(client);
                        self.connected = true;
                        storage_set(&self.token_key(), &self.token);
                        if !self.hosts.contains(&self.saved.host) {
                            self.hosts.push(self.saved.host.clone());
                            if let Ok(Some(storage)) = window().local_storage() {
                                let _ = storage.set_item(
                                    "demodex-hosts",
                                    &serde_json::to_string(&self.hosts).unwrap(),
                                );
                            }
                        }
                        ctx.link().send_message(Msg::Refresh);
                    }
                    Err(error) => {
                        if self.hosts.contains(&self.saved.host)
                            && !error.contains("Access token")
                            && !error.contains("Protocol mismatch")
                        {
                            let link = ctx.link().clone();
                            self.retry =
                                Some(Timeout::new(5000, move || link.send_message(Msg::Retry)));
                        }
                        self.error = error;
                    }
                }
            }
            Msg::Wake(generation, closed) => {
                if generation != self.generation {
                    return false;
                }
                if closed {
                    if self.error.is_empty() {
                        self.error = "Connection to the host was lost. Reconnecting automatically; commands will not be replayed. Check Tailscale and the browser's local-network permissions if it cannot reconnect.".into();
                    }
                    self.connected = false;
                    self.client = None;
                    self.refreshing = false;
                    self.busy = false;
                    let link = ctx.link().clone();
                    self.retry = Some(Timeout::new(3000, move || link.send_message(Msg::Retry)));
                } else {
                    ctx.link().send_message(Msg::Refresh);
                }
            }
            Msg::Retry => {
                if self.host_input.trim_end_matches('/') != self.saved.host {
                    return false;
                }
                if !self.connected && !self.connecting {
                    ctx.link().send_message(Msg::Connect)
                } else if self.connected {
                    ctx.link().send_message(Msg::Refresh)
                }
            }
            Msg::Refresh => {
                if self.refreshing {
                    self.refresh_again = true;
                    return false;
                }
                let Some(client) = self.client.clone().filter(|_| self.connected) else {
                    return false;
                };
                self.refreshing = true;
                self.refresh_again = false;
                let generation = self.generation;
                let id = self.saved.selected.clone();
                let after = self
                    .events
                    .last()
                    .and_then(|v| v["seq"].as_i64())
                    .unwrap_or(0);
                ctx.link().send_future(async move {
                    Msg::Snapshot(
                        generation,
                        client
                            .snapshot(id, after)
                            .await
                            .map_err(|e| format!("{e:#}")),
                    )
                });
            }
            Msg::Snapshot(generation, result) => {
                if generation != self.generation {
                    return false;
                }
                self.refreshing = false;
                match result {
                    Ok(snapshot) => {
                        self.sessions = array(&snapshot.sessions);
                        self.runtime = snapshot.runtime;
                        self.environments = array(&snapshot.environments);
                        if !self.runtime["account"].is_null() {
                            self.login = Value::Null;
                        }
                        if snapshot.selected == self.saved.selected {
                            let previous = text(&self.current, "sandbox");
                            let next = text(&snapshot.detail["session"], "sandbox");
                            if self.current.is_null() || previous != next {
                                self.saved
                                    .fields
                                    .insert("session_sandbox".into(), next.into());
                            }
                            if snapshot.detail.is_null() && !self.saved.selected.is_empty() {
                                self.saved.selected.clear();
                                self.events.clear();
                            }
                            self.current = snapshot.detail["session"].clone();
                            self.pending = array(&snapshot.detail["pending"]);
                            let cursor = self
                                .events
                                .last()
                                .and_then(|v| v["seq"].as_i64())
                                .unwrap_or(0);
                            self.events.extend(
                                snapshot
                                    .events
                                    .into_iter()
                                    .filter(|v| v["seq"].as_i64().unwrap_or(0) > cursor),
                            );
                        }
                    }
                    Err(error) => {
                        self.error = error;
                    }
                }
                if self.refresh_again {
                    ctx.link().send_message(Msg::Refresh)
                }
            }
            Msg::Select(id) => {
                self.saved.selected = id;
                self.saved.page.clear();
                self.current = Value::Null;
                self.pending.clear();
                self.events.clear();
                self.follow = true;
                ctx.link().send_message(Msg::Refresh);
            }
            Msg::Page(page) => {
                self.saved.page = page;
                self.saved.selected.clear();
                self.current = Value::Null;
                self.events.clear();
                self.pending.clear();
                ctx.link().send_message(Msg::Refresh);
            }
            Msg::Field(name, value) => {
                self.saved.fields.insert(name, value);
            }
            Msg::ChooseImage => {
                if let Some(input) = self.image_ref.cast::<HtmlInputElement>() { input.click(); }
            }
            Msg::UploadImage(file) => {
                if self.busy || !self.connected {
                    self.error = if self.busy { "Wait for the current request before attaching an image" }
                        else { "Connect to the host before attaching an image" }.into();
                    return true;
                }
                if file.size() == 0.0 || file.size() > 4.0 * 1024.0 * 1024.0 {
                    self.error = "Images must be between 1 byte and 4 MiB".into();
                } else {
                    let draft = self.saved.draft();
                    let end = draft.encode_utf16().count() as u32;
                    let (start, end) = self.prompt_ref.cast::<HtmlTextAreaElement>()
                        .map(|el| (el.selection_start().ok().flatten().unwrap_or(end), el.selection_end().ok().flatten().unwrap_or(end)))
                        .unwrap_or((end, end));
                    let id = self.saved.selected.clone();
                    self.upload_anchor = Some((id.clone(), draft, start, end));
                    self.busy = true;
                    self.error.clear();
                    let generation = self.generation;
                    ctx.link().send_future(async move {
                        let result = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await
                            .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                            .map_err(|_| "Could not read the image".to_owned());
                        Msg::ImageRead(generation, id, result)
                    });
                }
            }
            Msg::ImageRead(generation, id, result) => {
                if generation != self.generation { return false; }
                self.busy = false;
                match result {
                    Ok(bytes) => self.request(ctx, Operation::UploadImage { id, bytes }),
                    Err(error) => { self.error = error; self.upload_anchor = None; }
                }
            }
            Msg::Draft(value) => {
                self.saved.drafts.insert(self.saved.key(), value);
            }
            Msg::AnswerDraft(name, value) => {
                self.saved.answers.insert(name, value);
            }
            Msg::Run(operation) => self.request(ctx, operation),
            Msg::Completed(generation, operation, id, result) => {
                if generation != self.generation {
                    return false;
                }
                self.busy = false;
                match result {
                    Ok(value) => {
                        if operation.is_mutation() {
                            self.receipt.clear();
                            self.saved.receipts.remove(&self.saved.host);
                        }
                        match operation {
                            Operation::HostSession { .. }
                            | Operation::ExternalSession { .. }
                            | Operation::EnvironmentSession { .. } => {
                                ctx.link()
                                    .send_message(Msg::Select(text(&value, "id").into()));
                                for key in ["session_name", "thread_id", "cwd"] {
                                    self.saved.fields.remove(key);
                                }
                            }
                            Operation::Prompt { id, text } => {
                                let key = format!("{}:{id}", self.saved.host);
                                if self.saved.drafts.get(&key) == Some(&text) {
                                    self.saved.drafts.remove(&key);
                                }
                            }
                            Operation::UploadImage { id, .. } => {
                                let key = format!("{}:{id}", self.saved.host);
                                let current = self.saved.drafts.get(&key).cloned().unwrap_or_default();
                                let path = text(&value, "path");
                                let draft = match self.upload_anchor.take() {
                                    Some((session, original, start, end)) if session == id && original == current =>
                                        insert_image_path(&current, path, start, end),
                                    _ => insert_image_path(&current, path, u32::MAX, u32::MAX),
                                };
                                self.saved.drafts.insert(key, draft);
                            }
                            Operation::Login => self.login = value,
                            Operation::Receipt { .. } => {
                                if value["state"] == "completed" {
                                    self.saved.receipts.remove(&self.saved.host);
                                    self.receipt.clear();
                                }
                                self.error =
                                    serde_json::to_string_pretty(&value).unwrap_or_default()
                            }
                            _ => {}
                        }
                    }
                    Err(error) => {
                        if matches!(operation, Operation::UploadImage { .. }) { self.upload_anchor = None; }
                        self.error = error;
                        if operation.is_mutation() {
                            self.receipt = id;
                        }
                    }
                }
                ctx.link().send_message(Msg::Refresh);
            }
            Msg::LoadSaved(more) => {
                if let Some(client) = self.client.clone() {
                    let cursor = if more { self.cursor.clone() } else { None };
                    let search = self.saved.field("search");
                    let generation = self.generation;
                    ctx.link().send_future(async move {
                        Msg::SavedThreads(
                            generation,
                            client
                                .read(Operation::SavedThreads { cursor, search })
                                .await
                                .map_err(|e| format!("{e:#}")),
                            more,
                        )
                    });
                }
            }
            Msg::SavedThreads(generation, result, more) => {
                if generation != self.generation {
                    return false;
                }
                match result {
                    Ok(value) => {
                        if !more {
                            self.saved_threads.clear();
                        }
                        self.saved_threads.extend(array(&value["data"]));
                        self.cursor = value["nextCursor"].as_str().map(String::from);
                    }
                    Err(error) => self.error = error,
                }
            }
            Msg::ChooseThread(thread) => {
                self.saved
                    .fields
                    .insert("thread_id".into(), text(&thread, "id").into());
                let name = thread["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| text(&thread, "preview"));
                self.saved
                    .fields
                    .insert("session_name".into(), name.chars().take(120).collect());
                self.saved.fields.remove("cwd");
            }
            Msg::Answer(pending, decision) => {
                let key = text(&pending, "key").to_owned();
                let result = if let Some(decision) = decision {
                    json!({"decision":decision})
                } else if text(&pending, "method") == "item/tool/requestUserInput" {
                    let mut answers = serde_json::Map::new();
                    for question in array(&pending["params"]["questions"]) {
                        let id = text(&question, "id");
                        let value = self.saved.answer(&key, id);
                        if value.trim().is_empty() {
                            self.error = "Answer each question explicitly before sending".into();
                            return true;
                        }
                        answers.insert(id.into(), json!({"answers":[value]}));
                    }
                    json!({"answers":answers})
                } else {
                    match serde_json::from_str(
                        &self
                            .saved
                            .answers
                            .get(&format!("{}:{key}:raw", self.saved.host))
                            .cloned()
                            .unwrap_or_default(),
                    ) {
                        Ok(v) => v,
                        Err(error) => {
                            self.error = error.to_string();
                            return true;
                        }
                    }
                };
                self.request(
                    ctx,
                    Operation::Answer {
                        id: self.saved.selected.clone(),
                        key,
                        result: result.to_string(),
                    },
                );
            }
            Msg::Dismiss => self.error.clear(),
            Msg::Scroll => {
                if let Some(el) = self.transcript_ref.cast::<HtmlElement>() {
                    self.follow = el.scroll_height() - el.client_height() - el.scroll_top() < 80;
                }
                return false;
            }
            Msg::Pwa => self.pwa(),
            Msg::ApplyUpdate => {
                if !self.busy
                    && self.persist()
                    && let Ok(function) =
                        js_sys::Reflect::get(&window(), &"demodexApplyUpdate".into())
                    && let Some(function) = function.dyn_ref::<js_sys::Function>()
                {
                    let _ = function.call0(&window());
                }
            }
        }
        if navigates && self.navigation() != previous_navigation {
            self.record_navigation(false);
        }
        self.persist();
        true
    }
    fn rendered(&mut self, _ctx: &Context<Self>, _first: bool) {
        if self.follow
            && let Some(el) = self.transcript_ref.cast::<HtmlElement>()
        {
            el.set_scroll_top(el.scroll_height());
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let detail = !self.saved.selected.is_empty() || !self.saved.page.is_empty();
        html! {<>
            <header><a href="./" class="brand">{"DEMODEX"}<span>{" / operator console"}</span></a><button onclick={ctx.link().callback(|_|Msg::Connections)}>{"Connections"}</button><span class="indicator">{if self.connected{"CONNECTED"}else if self.connecting{"CONNECTING"}else{"DISCONNECTED"}}</span></header>
            {if !self.connection_storage_error.is_empty(){html!{<div class="app-notice" role="alert">{self.connection_storage_error.clone()}</div>}}else{Html::default()}}
            {if self.update_available{html!{<div class="app-notice" role="status"><span>{"New version available. Your agent keeps running."}</span><button disabled={self.busy||self.updating} onclick={ctx.link().callback(|_|Msg::ApplyUpdate)}>{if self.updating{"Updating…"}else{"Update now"}}</button></div>}}else{Html::default()}}
            {if !self.storage_error.is_empty()||!self.update_error.is_empty(){html!{<div class="app-notice" role="status">{format!("{} {}",self.storage_error,self.update_error)}</div>}}else{Html::default()}}
            {if !self.error.is_empty(){html!{<div class="error global-error" role="alert"><pre>{self.error.clone()}</pre><button onclick={ctx.link().callback(|_|Msg::Dismiss)}>{"Dismiss"}</button>{if !self.connected{html!{<button disabled={self.connecting} onclick={ctx.link().callback(|_|Msg::Connect)}>{if self.connecting{"Connecting…"}else{"Retry connection"}}</button>}}else{Html::default()}}{if !self.receipt.is_empty(){self.button(ctx,"Check command receipt",Operation::Receipt{id:self.receipt.clone()})}else{Html::default()}}</div>}}else{Html::default()}}
            {if !self.connected&&!self.sessions.is_empty(){html!{<div class="app-notice" role="status">{"Connection lost — showing the last known session state. Reconnecting does not replay commands."}</div>}}else{Html::default()}}
            <div class={classes!("layout",detail.then_some("detail"),self.connections_page.then_some("connections-page"))}>
                <aside>
                    {if self.connections_page {html!{<section class="connections"><h1>{"Your connections"}</h1><p class="muted">{"Saved on this device, including access tokens. Connect once to save or update an entry."}</p>{if self.connected{html!{<button onclick={ctx.link().callback(|_|Msg::BackToHost)}>{"Back to current host"}</button>}}else{Html::default()}}{for self.connections.iter().map(|connection|{
                        let url = connection.url.clone(); let edit = url.clone(); let remove = url.clone();
                        html!{<article class="connection"><button class="connection-open" disabled={self.connecting||self.busy} onclick={ctx.link().callback(move |_|Msg::UseConnection(url.clone(),true))}><strong>{if connection.name.is_empty(){connection.url.clone()}else{connection.name.clone()}}</strong><small>{connection.url.clone()}</small></button><div><button disabled={self.connecting||self.busy} onclick={ctx.link().callback(move |_|Msg::UseConnection(edit.clone(),false))}>{"Edit"}</button><button disabled={self.connecting||self.busy} onclick={ctx.link().callback(move |_|Msg::ForgetConnection(remove.clone()))}>{"Forget"}</button></div></article>}
                    })}<h2>{"Add or edit connection"}</h2></section>}}else{Html::default()}}
                    <label>{"Connection name (optional)"}<input disabled={self.connecting||self.busy} value={self.connection_name.clone()} oninput={ctx.link().callback(|e|Msg::ConnectionName(input(e)))}/></label>
                    <details class="host-picker" open={self.connections_page||self.client.is_none()}><summary>{"Target host"}</summary><label>{"Host URL"}<input disabled={self.connecting||self.busy} list="hosts" value={self.host_input.clone()} oninput={ctx.link().callback(|e|Msg::HostInput(input(e)))}/></label><datalist id="hosts">{for self.hosts.iter().map(|host|html!{<option value={host.clone()}/>})}</datalist><label>{"Access token (optional with Tailscale)"}<input disabled={self.connecting||self.busy} type="password" value={self.token.clone()} oninput={ctx.link().callback(|e|Msg::Token(input(e)))}/></label><p class="muted">{"Leave blank to use your Tailscale identity, or enter this host's access token."}</p><button disabled={self.connecting||self.busy} onclick={ctx.link().callback(|_|Msg::Connect)}>{"Connect host"}</button></details>
                    <button class="environment-nav" onclick={ctx.link().callback(|_|Msg::Page("environments".into()))}>{"Environments"}</button>
                    <div class="section-title"><h2>{"Sessions"}</h2><button onclick={ctx.link().callback(|_|Msg::Page("external".into()))}>{"+ External"}</button></div>
                    {for self.sessions.iter().map(|session|{let id=text(session,"id").to_owned();html!{<button class={classes!("session",(id==self.saved.selected).then_some("chosen"))} onclick={ctx.link().callback(move |_|Msg::Select(id.clone()))}><strong>{text(session,"name")}</strong><span>{text(session,"status")}</span><small>{text(&session["targets"][0],"cwd")}</small></button>}})}
                </aside>
                <main class={(!self.saved.selected.is_empty()).then_some("chat-main")}>
                    {if !self.saved.page.is_empty(){html!{<button class="back" onclick={ctx.link().callback(|_|Msg::Back)}>{"← Back"}</button>}}else{Html::default()}}
                    {match self.saved.page.as_str(){"environments"=>self.environment_view(ctx),"external"=>self.external_view(ctx),_=>if !self.saved.selected.is_empty(){self.chat_view(ctx)}else{html!{<section class="empty"><h1>{"A small operator."}<br/>{"Your machines."}</h1><p>{"Connect a host, then select a session or create an environment."}</p><button onclick={ctx.link().callback(|_|Msg::Page("environments".into()))}>{"Open environments"}</button></section>}}}}
                </main>
            </div>
        </>}
    }
}

impl App {
    fn chat_view(&self, ctx: &Context<Self>) -> Html {
        if self.current.is_null() {
            return html! {<p>{"Loading session…"}</p>};
        }
        let id = self.saved.selected.clone();
        let status = text(&self.current, "status");
        let working = active(status);
        let waiting =
            status == "waiting" || self.pending.iter().any(|p| text(p, "state") == "pending");
        let send = Operation::Prompt {
            id: id.clone(),
            text: self.saved.draft(),
        };
        html! {<>
            <div class="session-heading"><button class="back" onclick={ctx.link().callback(|_|Msg::Back)}>{"← Back"}</button><h1>{text(&self.current,"name")}</h1><span class="status">{status}</span>{if status=="disconnected"{self.button(ctx,"Connect / resume",Operation::Connect{id:id.clone()})}else{Html::default()}}</div>
            <div class="transcript" ref={self.transcript_ref.clone()} onscroll={ctx.link().callback(|_|Msg::Scroll)} role="region" aria-label="Chat transcript" tabindex="0">
                {if let Some(error)=self.current["error"].as_str(){html!{<p class="error">{error}</p>}}else{Html::default()}}
                <details class="session-settings"><summary>{format!("Session settings · {}",sandbox_name(text(&self.current["effective_sandbox"],"type")))}</summary>{for array(&self.current["targets"]).iter().map(|t|html!{<p class="muted">{"Working directory: "}<code>{text(t,"cwd")}</code></p>})}
                    <section class="runtime-panel"><p>{"Active sandbox: "}<strong>{sandbox_name(text(&self.current["effective_sandbox"],"type"))}</strong></p>{self.sandbox(ctx,"session_sandbox","Session sandbox")}{if !working{self.button(ctx,"Apply sandbox",Operation::Sandbox{id:id.clone(),input:json!({"sandbox":nonempty(self.saved.field("session_sandbox"))}).to_string()})}else{html!{<p class="muted">{"Sandbox settings can be changed when the session is idle."}</p>}}}</section>
                </details>
                <div class="conversation">{for transcript(&self.events).iter().map(|item|{
                    let kind=text(item,"type");html!{<article class={(kind=="userMessage").then_some("user")}><div class="item-kind">{kind}</div>{match kind{
                        "agentMessage"=>html!{<pre>{text(item,"text")}</pre>},
                        "userMessage"=>html!{<pre>{array(&item["content"]).iter().map(|c|text(c,"text")).collect::<Vec<_>>().join("\n")}</pre>},
                        "commandExecution"=>html!{<><code>{text(item,"command")}</code><details><summary>{"Command output"}</summary><pre>{text(item,"aggregatedOutput")}</pre></details></>},
                        _=>html!{<details><summary>{kind}</summary><pre>{serde_json::to_string_pretty(item).unwrap_or_default()}</pre></details>},
                    }}</article>}
                })}{if waiting{html!{<div class="activity waiting" role="status">{"Waiting for your input"}</div>}}else if working{html!{<div class="activity" role="status"><span class="activity-marker" aria-hidden="true"/>{"Working…"}</div>}}else{Html::default()}}</div>
                {for self.pending.iter().map(|pending|self.approval(ctx,pending))}
                <details class="diagnostics"><summary>{format!("Protocol events ({})",self.events.len())}</summary><pre>{serde_json::to_string_pretty(&self.events).unwrap_or_default()}</pre></details>
            </div>
            <form class="composer" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(send.clone())})}><label class="sr-only" for="prompt">{"Message"}</label><textarea id="prompt" ref={self.prompt_ref.clone()} value={self.saved.draft()} placeholder="Give the agent a task…" oninput={ctx.link().callback(|e|Msg::Draft(input(e)))} onpaste={ctx.link().batch_callback(|e:Event| {
                let e = e.unchecked_into::<web_sys::ClipboardEvent>();
                let file = e.clipboard_data().and_then(|data|data.files()).and_then(|files|files.get(0));
                if file.is_some() { e.prevent_default(); }
                file.map(Msg::UploadImage)
            })}/><input type="file" hidden=true ref={self.image_ref.clone()} accept="image/png,image/jpeg,image/gif,image/webp" aria-label="Upload image" onchange={ctx.link().batch_callback(|e:Event| {
                let input=e.target_unchecked_into::<HtmlInputElement>();
                let file=input.files().and_then(|files|files.get(0)); input.set_value(""); file.map(Msg::UploadImage)
            })}/><div><button type="button" disabled={self.busy||!self.connected} onclick={ctx.link().callback(|_|Msg::ChooseImage)}>{if self.busy && self.upload_anchor.is_some(){"Uploading…"}else{"Attach image"}}</button><button class="primary" disabled={self.busy||!self.connected||working||waiting||status=="disconnected"||status=="connecting"||self.saved.draft().trim().is_empty()}>{"Send"}</button>{if working||waiting{self.button(ctx,"Interrupt",Operation::Interrupt{id})}else{html!{<button type="button" disabled=true>{"Interrupt"}</button>}}}</div></form>
        </>}
    }
    fn approval(&self, ctx: &Context<Self>, pending: &Value) -> Html {
        let disabled = self.busy || !self.connected || text(pending, "state") != "pending";
        let key = text(pending, "key");
        let method = text(pending, "method");
        html! {<section class="approval"><h2>{"Agent needs your input"}</h2><p class="muted">{format!("{method} · {}",text(pending,"state"))}</p>
            {if text(pending,"state")=="unavailable"{html!{<p>{"The connection owning this request ended. No answer was selected automatically; this request cannot be answered on a new connection."}</p>}}else{Html::default()}}
            {if matches!(method,"item/commandExecution/requestApproval"|"item/fileChange/requestApproval") {
                html!{<><pre>{serde_json::to_string_pretty(&pending["params"]).unwrap_or_default()}</pre>{for [("Approve once","accept"),("Decline","decline"),("Cancel turn","cancel")].into_iter().map(|(label,decision)|{let p=pending.clone();html!{<button disabled={disabled} onclick={ctx.link().callback(move |_|Msg::Answer(p.clone(),Some(decision.into())))}>{label}</button>}})}</>}
            }else{
                let p=pending.clone();html!{<form onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Answer(p.clone(),None)})}>
                    {if method=="item/tool/requestUserInput"{html!{<>{for array(&pending["params"]["questions"]).iter().map(|q|{let name=format!("{}:{key}:{}",self.saved.host,text(q,"id"));let value=self.saved.answer(key,text(q,"id"));html!{<label>{text(q,"question")}<span class="muted">{array(&q["options"]).iter().map(|o|format!("{}: {}",text(o,"label"),text(o,"description"))).collect::<Vec<_>>().join("\n")}</span><textarea required=true disabled={disabled} value={value} oninput={ctx.link().callback(move |e|Msg::AnswerDraft(name.clone(),input(e)))}/></label>}})}</>}}else{let name=format!("{}:{key}:raw",self.saved.host);html!{<><pre>{serde_json::to_string_pretty(&pending["params"]).unwrap_or_default()}</pre><label>{"Protocol response (JSON)"}<textarea required=true disabled={disabled} value={self.saved.answers.get(&name).cloned().unwrap_or_default()} oninput={ctx.link().callback(move |e|Msg::AnswerDraft(name.clone(),input(e)))}/></label></>}}}
                    <button disabled={disabled}>{if method=="item/tool/requestUserInput"{"Send answer"}else{"Send explicit response"}}</button>
                </form>}
            }}
        </section>}
    }
    fn environment_view(&self, ctx: &Context<Self>) -> Html {
        let host = text(&self.runtime, "mode") == "host";
        let input=json!({"name":self.saved.field("session_name"),"thread_id":nonempty(self.saved.field("thread_id")),"cwd":nonempty(self.saved.field("cwd")),"sandbox":nonempty(self.saved.field("sandbox"))}).to_string();
        html! {<>
            <h1>{"Environments"}</h1>
            <details><summary>{"Install Demodex on this device"}</summary><p>{"Use the PWA's HTTPS address and your browser's Install app / Add to Home Screen action. Updates download automatically and notify you before reloading an active page."}</p></details>
            <section class="runtime-panel"><h2>{"Codex account"}</h2>{if self.runtime["running"].as_bool()==Some(true){if !self.runtime["account"].is_null(){html!{<p>{format!("Signed in {} · {} profile",text(&self.runtime["account"],"email"),text(&self.runtime,"profile"))}</p>}}else{self.button(ctx,"Sign in with ChatGPT",Operation::Login)}}else{self.button(ctx,"Start Codex runtime",Operation::StartRuntime)}}
                {if !self.login.is_null(){html!{<><a href={text(&self.login,"verificationUrl").to_owned()} target="_blank" rel="noopener noreferrer">{"Continue sign-in in your browser"}</a><p>{"Device code: "}<strong>{text(&self.login,"userCode")}</strong></p></>}}else{Html::default()}}
                {if let Some(error)=self.runtime["error"].as_str(){html!{<p class="muted">{error}</p>}}else{Html::default()}}
            </section>
            {if host{html!{<>
                <form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::HostSession{input:input.clone()})})}><h2>{"This host"}</h2><p>{"Tools run with the service user's file and hardware access."}</p><p class="muted">{format!("Default working directory: {}",text(&self.runtime,"workspace"))}</p>
                    {self.field(ctx,"session_name","Session name","Hardware workspace")}{self.field(ctx,"thread_id","Existing Codex thread ID (optional)","Resume a saved Codex session")}{self.field(ctx,"cwd","Working directory (optional)","Keep default / saved directory")}{self.sandbox(ctx,"sandbox","Sandbox")}<p class="muted">{"Exit an external CLI session before attaching its saved thread here."}</p><button class="primary" disabled={self.busy||!self.connected||self.runtime["account"].is_null()||self.saved.field("session_name").trim().is_empty()}>{if self.saved.field("thread_id").trim().is_empty(){"New host session"}else{"Resume host session"}}</button>
                </form>
                <section class="saved-threads"><h2>{"Saved Codex sessions"}</h2>{self.field(ctx,"search","Search saved sessions","Title contains…")}<button disabled={!self.connected||self.busy} onclick={ctx.link().callback(|_|Msg::LoadSaved(false))}>{"Find saved sessions"}</button>{for self.saved_threads.iter().map(|thread|{let t=thread.clone();let name=thread["name"].as_str().filter(|s|!s.is_empty()).unwrap_or_else(||text(thread,"preview"));html!{<button class="session" onclick={ctx.link().callback(move |_|Msg::ChooseThread(t.clone()))}><strong>{name}</strong><span>{text(thread,"cwd")}</span><small>{text(thread,"id")}</small></button>}})}{if self.cursor.is_some(){html!{<button onclick={ctx.link().callback(|_|Msg::LoadSaved(true))}>{"Load more"}</button>}}else{Html::default()}}</section>
            </>}}else{
                let input=json!({"name":self.saved.field("environment_name"),"memory_mib":self.saved.field("memory").parse::<u32>().unwrap_or(4096),"cpus":self.saved.field("cpus").parse::<u16>().unwrap_or(2),"internet":self.saved.field("internet")=="true"}).to_string();
                html!{<><form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::CreateEnvironment{input:input.clone()})})}><h2>{"Create a work VM"}</h2>{self.field(ctx,"environment_name","Environment name","Scratch workspace")}{self.field(ctx,"memory","Memory (MiB)","4096")}{self.field(ctx,"cpus","CPUs","2")}<label class="checkbox"><input type="checkbox" checked={self.saved.field("internet")=="true"} onchange={ctx.link().callback(|e:Event|Msg::Field("internet".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Internet and LAN access"}</label><button disabled={self.busy||!self.connected}>{"Create and start"}</button></form>
                {self.sandbox(ctx,"sandbox","Sandbox for new VM sessions")}{for self.environments.iter().map(|env|{let id=text(env,"id").to_owned();let status=text(env,"status");html!{<section class="environment-card"><h2>{text(env,"name")}</h2><p>{status}</p><div class="environment-actions">{if status=="running"{html!{<>{if !self.runtime["account"].is_null(){self.button(ctx,"New session",Operation::EnvironmentSession{id:id.clone(),input:json!({"name":text(env,"name"),"sandbox":nonempty(self.saved.field("sandbox"))}).to_string()})}else{html!{<button disabled=true>{"New session"}</button>}}}{self.button(ctx,"Stop · keep disk",Operation::StopEnvironment{id})}</>}}else{self.button(ctx,"Start",Operation::StartEnvironment{id})}}</div>{if let Some(error)=env["error"].as_str(){html!{<p class="error">{error}</p>}}else{Html::default()}}</section>}})}</>}
            }}
        </>}
    }
    fn external_view(&self, ctx: &Context<Self>) -> Html {
        let targets = self.saved.field("targets");
        let parsed = if targets.trim().is_empty() {
            json!([])
        } else {
            serde_json::from_str::<Value>(&targets).unwrap_or(Value::Null)
        };
        let payload=json!({"name":self.saved.field("external_name"),"endpoint":self.saved.field("endpoint"),"thread_id":nonempty(self.saved.field("external_thread")),"targets":parsed,"sandbox":nonempty(self.saved.field("sandbox"))}).to_string();
        html! {<form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::ExternalSession{input:payload.clone()})})}><h1>{"Connect an agent"}</h1>{self.field(ctx,"external_name","Name","Workspace")}{self.field(ctx,"endpoint","App-server WebSocket","ws://127.0.0.1:4500")}{self.field(ctx,"external_thread","Existing thread ID (optional)","")}{self.sandbox(ctx,"sandbox","Sandbox")}<label>{"Execution environments (JSON array)"}<textarea value={targets} placeholder={r#"[{"id":"host","url":"ws://127.0.0.1:4501","cwd":"/workspace"}]"#} oninput={ctx.link().callback(|e|Msg::Field("targets".into(),input(e)))}/></label><button disabled={self.busy||!self.connected}>{"Create session"}</button><button type="button" onclick={ctx.link().callback(|_|Msg::Page(String::new()))}>{"Cancel"}</button></form>}
    }
}
fn nonempty(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}
