#[path = "new_session.rs"]
mod new_session;

use crate::{
    client::{Client, Wake},
    sync::{Reads, Resource, RESOURCES},
    model::*,
};
use demodex_protocol::{Notice, Operation};
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
    connection_editor: bool,
    editing_connection: Option<String>,
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
    targets: Vec<Value>,
    target_selection: Value,
    targets_pending: bool,
    target_notice: String,
    current: Value,
    controls: Value,
    models: Value,
    model_error: String,
    background: Value,
    show_background: bool,
    show_controls: bool,
    reorder_mode: bool,
    show_new_session: bool,
    show_saved_search: bool,
    new_target_setup: String,
    show_server_settings: bool,
    show_diagnostics: bool,
    pending: Vec<Value>,
    queued: Vec<Value>,
    queue_error: String,
    events: Vec<Value>,
    transcript: crate::transcript::Transcript,
    reads: Reads,
    busy: bool,
    error: String,
    storage_error: String,
    receipt: String,
    saved_threads: Vec<Value>,
    saved_search_serial: u64,
    saved_search_loading: bool,
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
    NewConnection,
    CloseConnectionEditor,
    History(Navigation),
    ConnectionName(String),
    UseConnection(String, bool),
    ForgetConnection(String),
    Connected(u64, Result<Rc<Client>, String>),
    Wake(u64, bool),
    Retry,
    Refresh,
    Read(u64, Resource, u64, String, i64, Result<Value, String>),
    Select(String),
    EditSession(String),
    Page(String),
    Controls(bool),
    ReorderMode,
    NewSession(bool),
    SavedSearch(bool),
    NewTargetSetup(String),
    StageSsh,
    ServerSettings(bool),
    Background(bool),
    Diagnostics(bool),
    LoadModels,
    ModelsLoaded(u64, String, Result<Value, String>),
    ControlField(String, String),
    EnterSends(bool),
    Field(String, String),
    TargetDraft(Value),
    Draft(String),
    ChooseImage,
    UploadImage(web_sys::File),
    ImageRead(u64, String, Result<Vec<u8>, String>),
    AnswerDraft(String, String),
    Send,
    Queue,
    Run(Operation),
    InvalidForm(String),
    Completed(u64, Operation, String, Result<Value, String>),
    LoadSaved(bool),
    SavedThreads(u64, u64, Result<Value, String>, bool),
    ChooseThread(Value),
    Answer(Value, Option<String>),
    Dismiss,
    Scroll,
    Latest,
    Resize,
    Pwa,
    ApplyUpdate,
}

impl App {
    fn start_reads(&mut self, ctx: &Context<Self>) {
        let Some(client) = self.client.clone().filter(|_| self.connected) else { return; };
        for resource in RESOURCES {
            let selected = self.saved.selected.clone();
            let Some(ticket) = self.reads.start(resource, &selected) else { continue; };
            let after = self.events.last().and_then(|v|v["seq"].as_i64()).unwrap_or(0);
            let operation = match resource {
                Resource::Sessions => Operation::Sessions,
                Resource::Runtime => Operation::Runtime,
                Resource::Environments => Operation::Environments,
                Resource::Targets => Operation::Targets,
                Resource::Detail => Operation::Detail { id: selected.clone() },
                Resource::Events => Operation::Events { id: selected.clone(), after },
            };
            let generation = self.generation;
            let client = client.clone();
            ctx.link().send_future(async move {
                Msg::Read(generation, resource, ticket, selected, after,
                    client.read(operation).await.map_err(|e|format!("{e:#}")))
            });
        }
    }

    fn navigation(&self) -> Navigation {
        Navigation {
            host: self.saved.host.clone(),
            selected: self.saved.selected.clone(),
            page: self.saved.page.clone(),
            connections: self.connections_page,
        }
    }
    fn record_navigation(&mut self, replace: bool) {
        let result = (|| {
            let state =
                wasm_bindgen::JsValue::from_str(&serde_json::to_string(&self.navigation()).ok()?);
            let history = window().history().ok()?;
            if replace {
                history.replace_state_with_url(&state, "", None).ok()?;
            } else {
                history.push_state_with_url(&state, "", None).ok()?;
            }
            Some(())
        })();
        if result.is_none() {
            self.error = "Browser navigation could not be updated.".into();
        }
    }
    fn store_connections(&mut self) {
        let ok = window()
            .local_storage()
            .ok()
            .flatten()
            .is_some_and(|storage| {
                serde_json::to_string(&self.connections)
                    .ok()
                    .is_some_and(|value| storage.set_item("demodex-connections", &value).is_ok())
            });
        self.connection_storage_error = if ok {
            String::new()
        } else {
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
    fn can_send(&self) -> bool {
        self.connected
            && !self.busy
            && !self.current.is_null()
            && self.current["archived"] != true
            && !self.saved.selected.is_empty()
            && !matches!(text(&self.current, "status"), "disconnected" | "connecting")
            && !self.saved.draft().trim().is_empty()
    }
    fn token_key(&self) -> String {
        format!("demodex-token:{}", self.saved.host)
    }
    fn close_connection_editor(&mut self) {
        self.connection_editor = false;
        // Until Save and connect is submitted, edits must not replace the
        // credentials used to reconnect the currently selected server.
        if !self.connecting {
            self.editing_connection = None;
            if let Some(connection) = self.connections.iter().find(|c| c.url == self.saved.host) {
                self.host_input = connection.url.clone();
                self.token = connection.token.clone();
                self.connection_name = connection.name.clone();
            }
        }
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
        let disabled = key == "session_sandbox"
            && (self.busy
                || !self.connected
                || active(text(&self.current, "status"))
                || self.pending.iter().any(|p| text(p, "state") == "pending"));
        html! {<><label for={key.to_owned()}>{label.to_owned()}</label><select id={key.to_owned()} disabled={disabled} onchange={ctx.link().callback(move |e:Event|Msg::Field(name.clone(),e.target_unchecked_into::<HtmlSelectElement>().value()))}>
            <option value="" selected={selected.is_empty()}>{"Codex default / saved policy"}</option>
            <option value="read-only" selected={selected=="read-only"}>{"Read-only"}</option>
            <option value="workspace-write" selected={selected=="workspace-write"}>{"Workspace-write"}</option>
            <option value="danger-full-access" selected={selected=="danger-full-access"}>{"Danger-full-access"}</option>
        </select></>}
    }
    fn button(&self, ctx: &Context<Self>, label: &str, operation: Operation) -> Html {
        html! {<button type="button" disabled={self.busy||!self.connected} onclick={ctx.link().callback(move |_|Msg::Run(operation.clone()))}>{label.to_owned()}</button>}
    }
    fn checked_button(
        &self,
        ctx: &Context<Self>,
        label: &str,
        operation: Result<Operation, String>,
    ) -> Html {
        html! {<button type="button" disabled={self.busy||!self.connected} onclick={ctx.link().callback(move |_|operation.clone().map_or_else(Msg::InvalidForm,Msg::Run))}>{label.to_owned()}</button>}
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
        let push_route=crate::notifications::route();
        if let Some(route)=&push_route {
            // A push opens a new page; keep draft state for the previous server.
            if saved.host != route.host {
                saved.switch_host(route.host.clone());
            }
            saved.selected = route.selected.clone();
        }
        saved.separate_creation_fields();
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
        let connections: Vec<Connection> = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item("demodex-connections").ok().flatten())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let connection_name = connections
            .iter()
            .find(|c| c.url == saved.host)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let foreground = ctx.link().clone();
        let pwa = ctx.link().clone();
        let navigation = ctx.link().clone();
        let resize = ctx.link().clone();
        let listeners = vec![
            EventListener::new(&window(), "resize", move |_| resize.send_message(Msg::Resize)),
            EventListener::new(&window(), "popstate", move |event| {
                if let Some(route) = event
                    .dyn_ref::<web_sys::PopStateEvent>()
                    .and_then(|e| e.state().as_string())
                    .and_then(|s| serde_json::from_str::<Navigation>(&s).ok())
                {
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
            connection_editor: false,
            editing_connection: None,
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
            targets: vec![],
            target_selection: Value::Null,
            targets_pending: false,
            target_notice: String::new(),
            current: Value::Null,
            controls: Value::Null,
            models: Value::Null,
            model_error: String::new(),
            background: Value::Null,
            show_background: false,
            show_controls: false,
            reorder_mode: false,
            show_new_session: false,
            show_saved_search: false,
            new_target_setup: String::new(),
            show_server_settings: false,
            show_diagnostics: false,
            pending: vec![],
            queued: vec![],
            queue_error: String::new(),
            events: vec![],
            transcript: crate::transcript::Transcript::default(),
            reads: Reads::default(),
            busy: false,
            error: String::new(),
            storage_error: String::new(),
            receipt: String::new(),
            saved_threads: vec![],
            saved_search_serial: 0,
            saved_search_loading: false,
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
        let restored = push_route.or_else(|| window()
            .history()
            .ok()
            .and_then(|h| h.state().ok())
            .and_then(|s| s.as_string())
            .and_then(|s| serde_json::from_str::<Navigation>(&s).ok()));
        if let Some(route) = &restored {
            if route.host == app.saved.host {
                app.connections_page = route.connections;
                app.saved.selected = route.selected.clone();
                app.saved.page = route.page.clone();
            }
        }
        // Settings are transient overlays, including for legacy saved page routes.
        app.saved.page.clear();
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
        let navigates = matches!(
            &msg,
            Msg::Connections
                | Msg::BackToHost
                | Msg::Select(_)
                | Msg::Page(_)
                | Msg::Connected(_, Ok(_))
        );
        match msg {
            Msg::History(route) => {
                self.new_target_setup.clear();
                self.show_server_settings = false;
                self.show_new_session = false;
                self.show_diagnostics = false;
                self.show_background = false;
                self.background = Value::Null;
                self.show_controls = false;
                self.controls = Value::Null;
                self.models = Value::Null;
                self.model_error.clear();
                if self.connecting {
                    self.generation += 1;
                    self.connecting = false;
                    self.retry = None;
                }
                self.close_connection_editor();
                if route.host != self.saved.host {
                    // A previous host's session IDs must never be sent to the
                    // current daemon. Return to explicit connection selection.
                    self.connections_page = true;
                    self.record_navigation(true);
                } else {
                    self.connections_page = route.connections;
                    self.saved.selected = route.selected;
                    self.saved.page.clear();
                    self.current = Value::Null;
                    self.events.clear();
                    self.reads.reset(true);
                    self.transcript = crate::transcript::Transcript::default();
                    self.pending.clear();
                    self.follow = true;
                    ctx.link().send_message(Msg::Refresh);
                }
            }
            Msg::SavedSearch(open) => {
                self.show_saved_search = open;
                if open { ctx.link().send_message(Msg::LoadSaved(false)); }
            }
            Msg::NewTargetSetup(kind) => self.new_target_setup = kind,
            Msg::StageSsh => {
                self.saved.fields.insert("new_ssh".into(), "true".into());
                self.new_target_setup.clear();
            }
            Msg::NewSession(open) => {
                self.show_saved_search = false;
                self.new_target_setup.clear();
                self.show_new_session = open;
                if open {
                    self.saved.fields.remove("thread_id");
                    self.show_controls = false;
                    self.show_background = false;
                    self.show_diagnostics = false;
                    ctx.link().send_message(Msg::Refresh);
                }
            }
            Msg::Connections => {
                self.new_target_setup.clear();
                self.show_controls = false;
                self.show_background = false;
                self.show_diagnostics = false;
                self.show_new_session = false;
                self.connections_page = true;
            }
            Msg::BackToHost => {
                self.connections_page = false;
                self.close_connection_editor();
                ctx.link().send_message(Msg::Retry);
            }
            Msg::NewConnection => {
                if self.connecting || self.busy { return false; }
                self.host_input = if self.connections.is_empty() { self.saved.host.clone() } else { String::new() };
                self.token.clear();
                self.connection_name.clear();
                self.editing_connection = None;
                self.connection_editor = true;
            }
            Msg::CloseConnectionEditor => {
                self.close_connection_editor();
                ctx.link().send_message(Msg::Retry);
            }
            Msg::ConnectionName(value) => self.connection_name = value,
            Msg::UseConnection(url, connect) => {
                if self.connecting || self.busy {
                    return false;
                }
                if let Some(connection) = self.connections.iter().find(|c| c.url == url) {
                    self.host_input = connection.url.clone();
                    self.token = connection.token.clone();
                    self.connection_name = connection.name.clone();
                    self.editing_connection = None;
                    if connect {
                        ctx.link().send_message(Msg::Connect);
                    } else {
                        self.editing_connection = Some(url);
                        self.connection_editor = true;
                    }
                }
            }
            Msg::ForgetConnection(url) => {
                if self.connecting || self.busy {
                    return false;
                }
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
                    self.reads.reset(false);
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
                if self.connection_editor { return true; }
                self.token = storage_get(&format!(
                    "demodex-token:{}",
                    self.host_input.trim_end_matches('/')
                ))
                .unwrap_or_default();
                let connection = self
                    .connections
                    .iter()
                    .find(|c| c.url == self.host_input.trim().trim_end_matches('/'));
                self.connection_name = connection.map(|c| c.name.clone()).unwrap_or_default();
                if let Some(connection) = connection {
                    self.token = connection.token.clone();
                }
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
                    self.show_diagnostics = false;
                    self.show_background = false;
                    self.background = Value::Null;
                    self.show_controls = false;
                    self.controls = Value::Null;
                    self.models = Value::Null;
                    self.model_error.clear();
                    self.saved.selected.clear();
                    self.saved.page.clear();
                    self.events.clear();
                    self.reads.reset(true);
                    self.transcript = crate::transcript::Transcript::default();
                    self.current = Value::Null;
                    self.sessions.clear();
                    self.saved_threads.clear();
                    self.pending.clear();
                    self.runtime = Value::Null;
                    self.environments.clear();
                    self.targets.clear();
                    self.target_selection = Value::Null;
                    self.receipt.clear();
                    self.target_notice.clear();
                    self.queued.clear();
                    self.queue_error.clear();
                    self.targets_pending = false;
                    self.cursor = None;
                    self.login = Value::Null;
                }
                self.saved.switch_host(host.clone());
                self.generation += 1;
                self.client = None;
                self.connected = false;
                self.connecting = true;
                self.reads.reset(false);
                self.saved_search_loading = false;
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
                        self.connection_editor = false;
                        if let Some(old_url) = self.editing_connection.take() {
                            if old_url != self.saved.host {
                                self.connections.retain(|c| c.url != old_url);
                                self.hosts.retain(|host| host != &old_url);
                                if let Ok(Some(storage)) = window().session_storage() {
                                    let _ = storage.remove_item(&format!("demodex-token:{old_url}"));
                                    if old_url == window().location().origin().unwrap_or_default() {
                                        let _ = storage.remove_item("demodex-token");
                                    }
                                }
                            }
                        }
                        let connection = Connection {
                            name: self.connection_name.trim().to_owned(),
                            url: self.saved.host.clone(),
                            token: self.token.clone(),
                        };
                        if let Some(existing) = self
                            .connections
                            .iter_mut()
                            .find(|c| c.url == connection.url)
                        {
                            *existing = connection;
                        } else {
                            self.connections.push(connection);
                        }
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
                    self.reads.reset(false);
                    self.busy = false;
                    let link = ctx.link().clone();
                    self.retry = Some(Timeout::new(3000, move || link.send_message(Msg::Retry)));
                } else if let Some(client) = &self.client {
                    for notice in client.take_notices() {
                        self.reads.notice(notice, &self.saved.selected);
                    }
                }
            }
            Msg::Retry => {
                if self.connection_editor {
                    return false;
                }
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
                self.reads.notice(Notice::Changed, &self.saved.selected);
            }
            Msg::Read(generation, resource, ticket, selected, after, result) => {
                if generation != self.generation || !self.reads.finish(resource, ticket) {
                    return false;
                }
                if resource.session() && selected != self.saved.selected { return false; }
                match result {
                    Ok(value) => match resource {
                        Resource::Sessions => {
                            self.sessions = array(&value);
                            let current = self.sessions.iter()
                                .find(|session| text(session, "id") == self.saved.selected)
                                .cloned().unwrap_or(Value::Null);
                            let previous = text(&self.current, "sandbox");
                            let next = text(&current, "sandbox");
                            if !current.is_null() && (self.current.is_null() || previous != next) {
                                self.saved.fields.insert("session_sandbox".into(), next.into());
                            }
                            // Session metadata has its own fast read. A detail response
                            // may have captured an older status before awaiting Codex.
                            self.current = current;
                        },
                        Resource::Runtime => {
                            self.runtime = value;
                            if !self.runtime["account"].is_null() { self.login = Value::Null; }
                        }
                        Resource::Environments => self.environments = array(&value),
                        Resource::Targets => self.targets = array(&value),
                        Resource::Detail => {
                            self.target_selection = value["target_selection"].clone();
                            self.targets_pending = value["targets_pending"] == true;
                            self.controls = value["controls"].clone();
                            self.background = value["background"].clone();
                            self.pending = array(&value["pending"]);
                            self.queued = array(&value["queued"]);
                            self.queue_error = text(&value, "queue_error").into();
                        }
                        Resource::Events => {
                            let cursor = self.events.last().and_then(|v|v["seq"].as_i64()).unwrap_or(0);
                            if after != cursor {
                                self.reads.invalidate(Resource::Events);
                            } else if let Some(batch) = value.as_array() {
                                let incoming: Vec<_> = batch.iter().filter(|v|v["seq"].as_i64().unwrap_or(0)>cursor).cloned().collect();
                                if !incoming.is_empty() {
                                    self.transcript.append(&incoming);
                                    self.events.extend(incoming);
                                    // Render each page before fetching the next, rather than
                                    // buffering an entire history behind one snapshot.
                                    if batch.len() == 500 { self.reads.invalidate(Resource::Events); }
                                }
                            } else { self.error = "Invalid event response".into(); }
                        }
                    },
                    Err(error) => {
                        self.reads.failed(resource);
                        self.error = error;
                    }
                }
            }
            Msg::EditSession(id) => {
                ctx.link().send_message_batch(vec![Msg::Select(id), Msg::Controls(true)]);
            }
            Msg::Select(id) => {
                self.new_target_setup.clear();
                self.show_new_session = false;
                self.show_controls = false;
                self.controls = Value::Null;
                self.background = Value::Null;
                self.models = Value::Null;
                self.model_error.clear();
                self.saved.selected = id;
                self.saved.page.clear();
                self.current = Value::Null;
                self.pending.clear();
                self.events.clear();
                self.reads.reset(true);
                self.transcript = crate::transcript::Transcript::default();
                self.follow = true;
                ctx.link().send_message(Msg::Refresh);
            }
            Msg::ServerSettings(open) => {
                self.new_target_setup.clear();
                self.show_server_settings = open && self.connected;
                self.show_new_session = false;
                self.show_controls = false;
                self.show_background = false;
                self.show_diagnostics = false;
                if open { ctx.link().send_message(Msg::Refresh); }
            }
            Msg::Page(page) => {
                self.saved.page = page;
                self.saved.selected.clear();
                self.current = Value::Null;
                self.events.clear();
                self.reads.reset(true);
                self.transcript = crate::transcript::Transcript::default();
                self.pending.clear();
                ctx.link().send_message(Msg::Refresh);
            }
            Msg::TargetDraft(value) => {
                self.saved.fields.insert(
                    format!("target-draft:{}", self.saved.selected),
                    value.to_string(),
                );
            }
            Msg::EnterSends(enabled) => self.saved.enter_sends = enabled,
            Msg::Field(name, value) => {
                if name == "search" {
                    // A cursor belongs to the submitted query. Editing invalidates
                    // both its results and any response still in flight.
                    self.saved_search_serial += 1;
                    self.saved_search_loading = false;
                    self.saved_threads.clear();
                    self.cursor = None;
                }
                if name == "resume_session" && value == "true" { self.saved.fields.remove("thread_id"); }
                self.saved.fields.insert(name, value);
            }
            Msg::Background(open) => {
                self.show_background = open;
                self.show_controls = false;
                self.show_diagnostics = false;
                if open {
                    ctx.link().send_message(Msg::Refresh);
                }
            }
            Msg::Diagnostics(open) => {
                self.show_background = false;
                self.show_diagnostics = open;
                self.show_controls = false;
            }
            Msg::ReorderMode => { self.reorder_mode = !self.reorder_mode; }
            Msg::Controls(open) => {
                self.show_diagnostics = false;
                self.show_background = false;
                self.show_controls = open;
                if open {
                    ctx.link().send_message(Msg::LoadModels);
                }
            }
            Msg::LoadModels => {
                if let Some(client) = self.client.clone().filter(|_| self.connected) {
                    let generation = self.generation;
                    let id = self.saved.selected.clone();
                    ctx.link().send_future(async move {
                        Msg::ModelsLoaded(
                            generation,
                            id.clone(),
                            client
                                .read(Operation::Models { id })
                                .await
                                .map_err(|e| format!("{e:#}")),
                        )
                    });
                } else {
                    self.model_error = "Connect to the server to load available models.".into();
                }
            }
            Msg::ModelsLoaded(generation, id, result) => {
                if generation != self.generation || id != self.saved.selected {
                    return false;
                }
                match result {
                    Ok(models) => {
                        self.models = models;
                        self.model_error.clear();
                    }
                    Err(error) => {
                        self.models = Value::Null;
                        self.model_error = error;
                    }
                }
            }
            Msg::ControlField(name, value) => {
                let prefix = format!("control:{}:", self.saved.selected);
                if name == "model"
                    && let Some(model) = array(&self.models["data"])
                        .iter()
                        .find(|m| text(m, "model") == value)
                {
                    self.saved.fields.insert(
                        format!("{prefix}effort"),
                        text(model, "defaultReasoningEffort").into(),
                    );
                    let tier = text(model, "defaultServiceTier");
                    self.saved.fields.insert(
                        format!("{prefix}tier"),
                        if tier == "default" {
                            String::new()
                        } else {
                            tier.into()
                        },
                    );
                }
                self.saved.fields.insert(format!("{prefix}{name}"), value);
            }
            Msg::ChooseImage => {
                if let Some(input) = self.image_ref.cast::<HtmlInputElement>() {
                    input.click();
                }
            }
            Msg::UploadImage(file) => {
                if self.busy || !self.connected {
                    self.error = if self.busy {
                        "Wait for the current request before attaching an image"
                    } else {
                        "Connect to the host before attaching an image"
                    }
                    .into();
                    return true;
                }
                if file.size() == 0.0 || file.size() > 4.0 * 1024.0 * 1024.0 {
                    self.error = "Images must be between 1 byte and 4 MiB".into();
                } else {
                    let draft = self.saved.draft();
                    let end = draft.encode_utf16().count() as u32;
                    let (start, end) = self
                        .prompt_ref
                        .cast::<HtmlTextAreaElement>()
                        .map(|el| {
                            (
                                el.selection_start().ok().flatten().unwrap_or(end),
                                el.selection_end().ok().flatten().unwrap_or(end),
                            )
                        })
                        .unwrap_or((end, end));
                    let id = self.saved.selected.clone();
                    self.upload_anchor = Some((id.clone(), draft, start, end));
                    self.busy = true;
                    self.error.clear();
                    let generation = self.generation;
                    ctx.link().send_future(async move {
                        let result = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
                            .await
                            .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                            .map_err(|_| "Could not read the image".to_owned());
                        Msg::ImageRead(generation, id, result)
                    });
                }
            }
            Msg::ImageRead(generation, id, result) => {
                if generation != self.generation {
                    return false;
                }
                self.busy = false;
                match result {
                    Ok(bytes) => self.request(ctx, Operation::UploadImage { id, bytes }),
                    Err(error) => {
                        self.error = error;
                        self.upload_anchor = None;
                    }
                }
            }
            Msg::Draft(value) => {
                self.saved.drafts.insert(self.saved.key(), value);
            }
            Msg::AnswerDraft(name, value) => {
                self.saved.answers.insert(name, value);
            }
            action @ (Msg::Send | Msg::Queue) => {
                let queue = matches!(action, Msg::Queue);
                let draft = self.saved.draft();
                let command = draft.split_whitespace().next().unwrap_or("");
                if queue && command.starts_with('/') && !command[1..].contains('/') {
                    self.error =
                        "Slash commands cannot be queued. Use Send to open session controls."
                            .into();
                } else if matches!(command, "/ps" | "/stop") && draft.trim() == command {
                    self.saved.drafts.remove(&self.saved.key());
                    ctx.link().send_message(Msg::Background(true));
                } else if matches!(command, "/model" | "/goal" | "/status" | "/help") {
                    if command != "/goal" && draft.trim() != command {
                        self.error = format!(
                            "Use {command} on its own to open session controls. Nothing was sent to Codex."
                        );
                    } else {
                        if command == "/goal" && draft.trim().len() > command.len() {
                            self.saved.fields.insert(
                                format!("control:{}:objective", self.saved.selected),
                                draft.trim()[command.len()..].trim().into(),
                            );
                        }
                        self.saved.drafts.remove(&self.saved.key());
                        ctx.link().send_message(Msg::Controls(true));
                    }
                } else if command.starts_with('/') && !command[1..].contains('/') {
                    self.error="This slash command is not supported here. Use /model, /goal, /status, /ps, /stop, or /help. Nothing was sent to Codex.".into();
                } else if self.can_send() {
                    if queue {
                        if active(text(&self.current, "status")) {
                            self.request(
                                ctx,
                                Operation::QueuePrompt {
                                    id: self.saved.selected.clone(),
                                    text: self.saved.draft(),
                                },
                            );
                        }
                    } else {
                        self.request(
                            ctx,
                            Operation::Prompt {
                                id: self.saved.selected.clone(),
                                text: self.saved.draft(),
                            },
                        );
                    }
                }
            }
            Msg::Run(operation) => {
                if matches!(
                    operation,
                    Operation::RegisterSshTarget { .. }
                        | Operation::RegisterSessionSshTarget { .. }
                        | Operation::CreateContainer { .. }
                        | Operation::CheckSshTarget { .. }
                        | Operation::ReconnectSshTarget { .. }
                        | Operation::ForgetTarget { .. }
                ) {
                    self.target_notice.clear();
                }
                self.request(ctx, operation)
            }
            Msg::Completed(generation, operation, id, result) => {
                if generation != self.generation {
                    return false;
                }
                self.busy = false;
                let notice = match &operation {
                    Operation::Prompt { id, .. } => Notice::Session { id: id.clone(), state: true },
                    _ => Notice::Changed,
                };
                match result {
                    Ok(value) => {
                        if operation.is_mutation() {
                            self.receipt.clear();
                            self.saved.receipts.remove(&self.saved.host);
                        }
                        match operation {
                            Operation::RenameSession { id, .. } => {
                                self.saved.fields.remove(&format!("rename:{id}"));
                            }
                            Operation::CreateSession { .. } => {
                                if self.saved.field("new_ssh") == "true" {
                                    let input = self.new_ssh_payload();
                                    for suffix in ["name","destination","port","identity","known_hosts","cwd"] {
                                        self.saved.fields.insert(format!("ssh_{suffix}"),self.saved.field(&format!("new_ssh_{suffix}")));
                                    }
                                    self.saved.fields.remove("new_ssh");
                                    ctx.link().send_message(Msg::Select(text(&value,"id").into()));
                                    ctx.link().send_message(Msg::Run(Operation::RegisterSessionSshTarget{id:text(&value,"id").into(),input}));
                                    ctx.link().send_message(Msg::Controls(true));
                                } else {
                                    ctx.link().send_message(Msg::Select(text(&value, "id").into()));
                                }
                                // Leave the independent resume draft intact.
                                self.saved
                                    .fields
                                    .insert("new_session_name".into(), String::new());
                                self.saved.fields.remove("new_targets");
                            }
                            Operation::HostSession { .. }
                            | Operation::ExternalSession { .. }
                            | Operation::EnvironmentSession { .. } => {
                                ctx.link()
                                    .send_message(Msg::Select(text(&value, "id").into()));
                                for key in ["session_name", "thread_id", "cwd"] {
                                    self.saved.fields.remove(key);
                                }
                            }
                            Operation::CreateEnvironment { .. } => {
                                if self.show_new_session && self.new_target_setup == "vm" {
                                    let mut targets = self.new_session_targets();
                                    targets.push(json!({"id":format!("vm-{}",text(&value,"id")),"cwd":"/workspace"}));
                                    self.saved.fields.insert("new_targets".into(),json!(targets).to_string());
                                    self.new_target_setup.clear();
                                }
                                self.new_target_setup.clear();
                                self.target_notice = "VM created and started. Select it in a session's Execution targets.".into();
                            }
                            Operation::CreateContainer { .. } => {
                                if self.show_new_session && self.new_target_setup == "container" {
                                    let mut targets = self.new_session_targets();
                                    targets.push(json!({"id":format!("container-{}",text(&value,"id")),"cwd":"/workspace"}));
                                    self.saved.fields.insert("new_targets".into(),json!(targets).to_string());
                                    self.new_target_setup.clear();
                                }
                                self.new_target_setup.clear();
                                self.target_notice = "Container created and started. Select it in a session's Execution targets.".into();
                                self.saved.fields.remove("container_name");
                            }
                            Operation::SelectTargets { id, .. } | Operation::ChangeTargets { id, .. } => {
                                self.saved.fields.remove(&format!("target-draft:{id}"));
                            }
                            Operation::CheckSshTarget { .. } => {
                                self.target_notice = "SSH connection verified.".into();
                            }
                            Operation::ReconnectSshTarget { .. } => {
                                self.target_notice="SSH executor replaced. Reconnect each attached session to use it.".into();
                            }
                            Operation::RegisterSshTarget { .. } => {
                                self.new_target_setup.clear();
                                self.target_notice="Shared SSH executor verified and registered.".into();
                                for key in ["name","destination","port","identity","known_hosts","cwd"] {
                                    self.saved.fields.remove(&format!("shared_ssh_{key}"));
                                }
                            }
                            Operation::RegisterSessionSshTarget { .. } => {
                                if self.new_target_setup == "session-ssh" { self.new_target_setup.clear(); }
                                self.target_notice="SSH target verified and attached. Send a message to apply it before resuming a goal or queue.".into();
                                for key in [
                                    "ssh_name",
                                    "ssh_destination",
                                    "ssh_port",
                                    "ssh_identity",
                                    "ssh_known_hosts",
                                    "ssh_cwd",
                                ] {
                                    self.saved.fields.remove(key);
                                }
                            }
                            Operation::RegisterTarget { .. } => {
                                self.new_target_setup.clear();
                                for key in ["target_name", "target_url", "target_cwd"] {
                                    self.saved.fields.remove(key);
                                }
                            }
                            Operation::Prompt { id, text }
                            | Operation::QueuePrompt { id, text } => {
                                let key = format!("{}:{id}", self.saved.host);
                                if self.saved.drafts.get(&key) == Some(&text) {
                                    self.saved.drafts.remove(&key);
                                }
                            }
                            Operation::UploadImage { id, .. } => {
                                let key = format!("{}:{id}", self.saved.host);
                                let current =
                                    self.saved.drafts.get(&key).cloned().unwrap_or_default();
                                let path = text(&value, "path");
                                let draft = match self.upload_anchor.take() {
                                    Some((session, original, start, end))
                                        if session == id && original == current =>
                                    {
                                        insert_image_path(&current, path, start, end)
                                    }
                                    _ => insert_image_path(&current, path, u32::MAX, u32::MAX),
                                };
                                self.saved.drafts.insert(key, draft);
                            }
                            Operation::Archive { id, archived: true }
                                if id == self.saved.selected =>
                            {
                                ctx.link().send_message(Msg::Page(String::new()));
                            }
                            Operation::Login => self.login = value,
                            Operation::Model { id, .. } => {
                                for field in ["model", "effort", "tier"] {
                                    self.saved.fields.remove(&format!("control:{id}:{field}"));
                                }
                            }
                            Operation::Goal { id, input } => {
                                // Status actions do not submit the objective/budget draft.
                                // Pausing or resuming must leave those unsaved edits intact.
                                if matches!(input.action.as_str(), "save" | "start") {
                                    for field in ["objective", "budget"] {
                                        self.saved.fields.remove(&format!("control:{id}:{field}"));
                                    }
                                }
                            }
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
                        if matches!(operation, Operation::UploadImage { .. }) {
                            self.upload_anchor = None;
                        }
                        self.error = error;
                        if operation.is_mutation() {
                            self.receipt = id;
                        }
                    }
                }
                self.reads.notice(notice, &self.saved.selected);
            }
            Msg::LoadSaved(more) => {
                if more && (self.saved_search_loading || self.cursor.is_none()) {
                    return false;
                }
                if let Some(client) = self.client.clone() {
                    self.saved_search_serial += 1;
                    self.saved_search_loading = true;
                    let serial = self.saved_search_serial;
                    if !more {
                        self.saved_threads.clear();
                        self.cursor = None;
                    }
                    let cursor = if more { self.cursor.clone() } else { None };
                    let search = self.saved.field("search");
                    let generation = self.generation;
                    ctx.link().send_future(async move {
                        Msg::SavedThreads(
                            generation,
                            serial,
                            client
                                .read(Operation::SavedThreads { cursor, search })
                                .await
                                .map_err(|e| format!("{e:#}")),
                            more,
                        )
                    });
                }
            }
            Msg::SavedThreads(generation, serial, result, more) => {
                if generation != self.generation || serial != self.saved_search_serial {
                    return false;
                }
                self.saved_search_loading = false;
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
                self.saved.fields.insert("cwd".into(), text(&thread,"cwd").into());
                self.show_saved_search = false;
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
                        result: demodex_protocol::CodexRecord(result),
                    },
                );
            }
            Msg::InvalidForm(error) => self.error = error,
            Msg::Dismiss => self.error.clear(),
            Msg::Resize => return self.follow,
            Msg::Latest => {
                self.follow = true;
            }
            Msg::Scroll => {
                if let Some(el) = self.transcript_ref.cast::<HtmlElement>() {
                    let follow = el.scroll_height() - el.client_height() - el.scroll_top() <= 1;
                    if self.follow != follow {
                        self.follow = follow;
                        return true;
                    }
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
        self.start_reads(ctx);
        if navigates {
            self.new_target_setup.clear();
            self.show_server_settings = false;
            self.show_background = false;
            self.show_controls = false;
            self.show_diagnostics = false;
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
        let detail = !self.saved.selected.is_empty();
        html! {<>
                    <header class="app-header">
                        <a href="./" class="brand">{"// DEMODEX"}</a>
                        <div class="connection-actions"><button onclick={ctx.link().callback(|_|Msg::Connections)}>{"Connections"}</button></div>
                        <div class="current-server"><span class={classes!("indicator",(!self.connected).then_some("disconnected"))}>{if self.connected{"connected to"}else if self.connecting{"connecting to"}else{"disconnected from"}}</span><strong title={self.saved.host.clone()}>{self.connections.iter().find(|c|c.url==self.saved.host).map(|c|if c.name.is_empty(){c.url.clone()}else{c.name.clone()}).unwrap_or_else(||if self.saved.host.is_empty(){"No server".into()}else{self.saved.host.clone()})}</strong></div>
                        {if self.client.is_some(){crate::usage::weekly(&self.runtime,self.connected)}else{Html::default()}}
                        {if self.connected{html!{<button class="server-settings-toggle" onclick={ctx.link().callback(|_|Msg::ServerSettings(true))}>{"Server settings"}</button>}}else{Html::default()}}
                    </header>
                    {if self.connections_page {html!{<crate::modal::Modal title="Connections" compact=true onclose={ctx.link().callback(|_|Msg::BackToHost)}>
                        <section class="connections">
                            <p class="muted">{"Saved on this device, including access tokens. Select a connection to connect."}</p>
                            {for self.connections.iter().map(|connection|{
                                let url = connection.url.clone(); let edit = url.clone(); let remove = url.clone();
                                html!{<div class="connection" key={connection.url.clone()}>
                                    <button class="connection-open" title={connection.url.clone()} disabled={self.connecting||self.busy} onclick={ctx.link().callback(move |_|Msg::UseConnection(url.clone(),true))}>
                                        <strong>{if connection.name.is_empty(){connection.url.clone()}else{connection.name.clone()}}</strong>
                                        <small>{connection.url.clone()}</small>
                                    </button>
                                    <button disabled={self.connecting||self.busy} title={format!("Edit {}",connection.url)} onclick={ctx.link().callback(move |_|Msg::UseConnection(edit.clone(),false))}>{"Edit"}</button>
                                    <button disabled={self.connecting||self.busy} title={format!("Delete {}",connection.url)} onclick={ctx.link().callback(move |_|Msg::ForgetConnection(remove.clone()))}>{"Delete"}</button>
                                </div>}
                            })}
                            <crate::ui::AddButton disabled={self.connecting||self.busy} onclick={ctx.link().callback(|_|Msg::NewConnection)}>{"+ connection"}</crate::ui::AddButton>
                        </section>
                        {if !self.error.is_empty() && !self.connection_editor{html!{<div class="error" role="alert"><pre>{&self.error}</pre></div>}}else{Html::default()}}
                    </crate::modal::Modal>}}else{Html::default()}}
                    {if self.connections_page && self.connection_editor {html!{<crate::modal::Modal title={if self.editing_connection.is_some(){"Edit connection"}else{"Add connection"}} compact=true onclose={ctx.link().callback(|_|Msg::CloseConnectionEditor)}>
                        <form class="connection-form" onsubmit={ctx.link().callback(|e:SubmitEvent|{e.prevent_default();Msg::Connect})}>
                            <label>{"Connection name (optional)"}<input disabled={self.connecting||self.busy} value={self.connection_name.clone()} oninput={ctx.link().callback(|e|Msg::ConnectionName(input(e)))}/></label>
                            <label>{"Host URL"}<input required=true type="url" disabled={self.connecting||self.busy} value={self.host_input.clone()} oninput={ctx.link().callback(|e|Msg::HostInput(input(e)))}/></label>
                            <label>{"Access token (optional with Tailscale)"}<input disabled={self.connecting||self.busy} type="password" value={self.token.clone()} oninput={ctx.link().callback(|e|Msg::Token(input(e)))}/></label>
                            <p class="muted">{"Leave blank to use your Tailscale identity. Connections are saved after a successful connection."}</p>
                            <button type="submit" class="primary" disabled={self.connecting||self.busy}>{if self.connecting{"Connecting…"}else{"Save and connect"}}</button>
                        </form>
                        {if !self.error.is_empty(){html!{<div class="error" role="alert"><pre>{&self.error}</pre></div>}}else{Html::default()}}
                    </crate::modal::Modal>}}else{Html::default()}}
                    {if self.show_server_settings {html!{<crate::modal::Modal title="Server settings" compact=true onclose={ctx.link().callback(|_|Msg::ServerSettings(false))}>
                        {self.environment_view(ctx)}
                        {if !self.error.is_empty(){html!{<div class="error" role="alert"><pre>{&self.error}</pre><button onclick={ctx.link().callback(|_|Msg::Dismiss)}>{"Dismiss"}</button>{if !self.receipt.is_empty(){self.button(ctx,"Check command receipt",Operation::Receipt{id:self.receipt.clone()})}else{Html::default()}}</div>}}else{Html::default()}}
                    </crate::modal::Modal>}}else{Html::default()}}
                    {self.new_session_view(ctx)}
                    {self.target_setup_view(ctx)}
                    {if !self.connection_storage_error.is_empty(){html!{<div class="app-notice" role="alert">{self.connection_storage_error.clone()}</div>}}else{Html::default()}}
                    {if self.update_available{html!{<div class="app-notice" role="status"><span>{"New version available. Your agent keeps running."}</span><button disabled={self.busy||self.updating} onclick={ctx.link().callback(|_|Msg::ApplyUpdate)}>{if self.updating{"Updating…"}else{"Update now"}}</button></div>}}else{Html::default()}}
                    {if !self.storage_error.is_empty()||!self.update_error.is_empty(){html!{<div class="app-notice" role="status">{format!("{} {}",self.storage_error,self.update_error)}</div>}}else{Html::default()}}
                    {if !self.error.is_empty() && !self.show_controls && !self.show_diagnostics && !self.show_background && !self.connections_page && !self.show_new_session && !self.show_server_settings{html!{<div class="error global-error" role="alert"><pre>{self.error.clone()}</pre><button onclick={ctx.link().callback(|_|Msg::Dismiss)}>{"Dismiss"}</button>{if !self.connected{html!{<button disabled={self.connecting} onclick={ctx.link().callback(|_|Msg::Connect)}>{if self.connecting{"Connecting…"}else{"Retry connection"}}</button>}}else{Html::default()}}{if !self.receipt.is_empty(){self.button(ctx,"Check command receipt",Operation::Receipt{id:self.receipt.clone()})}else{Html::default()}}</div>}}else{Html::default()}}
                    {if !self.connected&&!self.sessions.is_empty(){html!{<div class="app-notice" role="status">{"Connection lost — showing the last known session state. Reconnecting does not replay commands."}</div>}}else{Html::default()}}
                    <div class={classes!("layout",detail.then_some("detail"))}>
                        <aside>
                            <div class="section-title"><h2>{"Sessions"}</h2><button type="button" class="reorder-toggle" aria-pressed={self.reorder_mode.to_string()} onclick={ctx.link().callback(|_|Msg::ReorderMode)}>{if self.reorder_mode{"Done reordering"}else{"Reorder"}}</button></div>
                            <button class="new-session-nav primary" disabled={!self.connected} onclick={ctx.link().callback(|_|Msg::NewSession(true))}>{"+ New Session"}</button>
                            {crate::overview::view(&self.sessions.iter().filter(|s|s["archived"]!=true).map(|s|{let mut s=s.clone();s["goal"]=if self.connected{self.runtime["background_terminals"][text(&s,"id")]["goal"].clone()}else{Value::Null};s["active_subagents"]=if self.connected{self.runtime["background_terminals"][text(&s,"id")]["active_subagents"].clone()}else{Value::Null};s["background_count"]=if self.connected{self.runtime["background_terminals"][text(&s,"id")]["count"].clone()}else{Value::Null};s}).collect::<Vec<_>>(),&self.targets,&self.saved.selected,ctx.link().callback(Msg::Select),ctx.link().callback(Msg::EditSession),ctx.link().callback(Msg::Run),self.busy||!self.connected,self.reorder_mode)}
                            {if self.sessions.iter().any(|s|s["archived"]==true){html!{<details class="archived-sessions"><summary>{format!("Archived sessions ({})",self.sessions.iter().filter(|s|s["archived"]==true).count())}</summary>{crate::overview::view(&self.sessions.iter().filter(|s|s["archived"]==true).map(|s|{let mut s=s.clone();s["background_count"]=Value::Null;s}).collect::<Vec<_>>(),&self.targets,&self.saved.selected,ctx.link().callback(Msg::Select),ctx.link().callback(Msg::EditSession),ctx.link().callback(Msg::Run),self.busy||!self.connected,self.reorder_mode)}</details>}}else{Html::default()}}
                        </aside>
                        <main class={(!self.saved.selected.is_empty()).then_some("chat-main")}>
                            {if !self.saved.selected.is_empty(){self.chat_view(ctx)}else{html!{<section class="empty"><span class="eyebrow">{"SERVER OVERVIEW"}</span><h1>{"Your agents, by project."}</h1><p>{"Select an agent in the folder tree to open its conversation. Only folders with sessions appear."}</p><p class="muted">{"Each agent keeps its icon and generated name. Status shows who is working, waiting for you, or disconnected."}</p><button disabled={!self.connected} onclick={ctx.link().callback(|_|Msg::NewSession(true))}>{"New Session"}</button></section>}}}
                        </main>
                    </div>
                </>}
    }
}

impl App {
    fn background_panel(&self, ctx: &Context<Self>) -> Html {
        if !self.show_background {
            return Html::default();
        }
        let rows = array(&self.background["data"]);
        let generation = text(&self.background, "generation").to_owned();
        let stop = |processes| Operation::StopBackground {
            id: self.saved.selected.clone(),
            generation: generation.clone(),
            processes,
        };
        html! {<crate::modal::Modal title="Background terminals" compact=true onclose={ctx.link().callback(|_|Msg::Background(false))}>
            <button disabled={self.reads.busy()||!self.connected} onclick={ctx.link().callback(|_|Msg::Refresh)}>{"Refresh"}</button>
            {if let Some(error)=self.background["error"].as_str(){html!{<p class="error">{format!("Background terminals unavailable: {error}")}</p>}}else if self.background["data"].is_array(){html!{<>
                {if rows.is_empty(){html!{<p>{"No background terminals running."}</p>}}else{html!{<>
                    {for rows.iter().map(|row|html!{<section class="background-terminal">
                        <div class="background-command"><pre>{text(row,"command")}</pre><code>{text(row,"cwd")}</code><small class="muted">{"Running · Machine unknown"}</small></div>
                        {self.button(ctx,"Stop terminal",stop(vec![(text(row,"processId").into(),text(row,"itemId").into())]))}
                    </section>})}
                    {self.button(ctx,&format!("Stop all {} listed terminals",rows.len()),stop(rows.iter().map(|row|(text(row,"processId").into(),text(row,"itemId").into())).collect()))}
                    <details class="background-diagnostics"><summary>{"Diagnostics"}</summary>{for rows.iter().map(|row|html!{<p>{format!("{} · Process {} · Item {}",text(row,"command"),text(row,"processId"),text(row,"itemId"))}</p>})}</details>
                </>}}}
            </>}}else{html!{<p>{"Loading background terminals…"}</p>}}}
            {self.modal_error(ctx)}
        </crate::modal::Modal>}
    }

    fn modal_error(&self, ctx: &Context<Self>) -> Html {
        if self.error.is_empty() {
            return Html::default();
        }
        html! {<div class="error" role="alert"><pre>{&self.error}</pre><button onclick={ctx.link().callback(|_|Msg::Dismiss)}>{"Dismiss"}</button>{if !self.receipt.is_empty(){self.button(ctx,"Check command receipt",Operation::Receipt{id:self.receipt.clone()})}else{Html::default()}}</div>}
    }
    fn controls_view(&self, ctx: &Context<Self>, working: bool) -> Html {
        let prefix = format!("control:{}:", self.saved.selected);
        let fields = self
            .saved
            .fields
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(&prefix)
                    .map(|key| (key.to_owned(), v.clone()))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        html! {<crate::controls::SessionControls id={self.saved.selected.clone()} state={self.controls.clone()} models={self.models.clone()} model_error={self.model_error.clone()} fields={fields} disabled={self.busy||!self.connected||self.controls["connected"]!=true||self.current["archived"]==true} working={working} targets_pending={self.targets_pending} onfield={ctx.link().callback(|(name,value)|Msg::ControlField(name,value))} onrun={ctx.link().callback(Msg::Run)} onrefresh={ctx.link().callback(|_|Msg::LoadModels)}/>}
    }
    fn chat_view(&self, ctx: &Context<Self>) -> Html {
        let enter_sends = self.saved.enter_sends;
        let close = html! {
            <button type="button" class="session-close" aria-label="Close session view"
                title="Close session view" onclick={ctx.link().callback(|_|Msg::Page(String::new()))}>{"×"}</button>
        };
        if self.current.is_null() {
            return html! {<><div class="session-heading"><span>{"Loading session…"}</span>{close}</div></>};
        }
        let id = self.saved.selected.clone();
        let status = text(&self.current, "status");
        let reconnect = status == "disconnected" && self.current["archived"] != true;
        let stored_error = text(&self.current, "error");
        let error = if matches!(stored_error, "host runtime is restarting; reconnect to resume" | "Host runtime disconnected; reconnect to resume") {
            if self.runtime["running"] == true { "Host runtime restarted. Reconnect to resume." } else { "Host runtime is unavailable. Start it in Server settings, then reconnect." }
        } else if stored_error.is_empty() && reconnect { "Session disconnected." } else { stored_error };
        let working = active(status);
        let waiting =
            status == "waiting" || self.pending.iter().any(|p| text(p, "state") == "pending");
        html! {<>
            <div class="session-heading"><button class="back" onclick={ctx.link().callback(|_|Msg::Page(String::new()))}>{"← Sessions"}</button><div class="session-heading-text"><h1>{crate::overview::title(&self.current)}</h1><span class="agent-name">{crate::overview::identity(&self.current)}</span>{if !text(&self.current,"thread_id").is_empty(){html!{<code class="thread-reference" title="Codex thread UUID">{text(&self.current,"thread_id")}</code>}}else{Html::default()}}</div><span class={classes!("status",crate::overview::status_class(status))}>{status}</span>{close}</div>
            {if !error.is_empty(){html!{<div class="error session-error" role="status"><span>{error}</span>{if reconnect{self.button(ctx,"Reconnect",Operation::Connect{id:id.clone()})}else{Html::default()}}</div>}}else{Html::default()}}
            <div class="transcript-frame"><div class="transcript" ref={self.transcript_ref.clone()} onscroll={ctx.link().callback(|_|Msg::Scroll)} role="region" aria-label="Chat transcript" tabindex="0">
                <ContextProvider<crate::links::LinkContext> context={crate::links::LinkContext { client:self.client.clone(), session:id.clone(), targets:self.current["targets"].clone(), connected:self.connected }}>
                <crate::conversation::Conversation key={self.saved.key()} chunks={self.transcript.groups.clone()} {working} {waiting} connected={self.connected}/>
                </ContextProvider<crate::links::LinkContext>>
            </div>
                {if !self.follow{html!{<button class="jump-latest" onclick={ctx.link().callback(|_|Msg::Latest)}>{"Jump to latest"}</button>}}else{Html::default()}}
            </div>
            <div class="session-inbox" role="region" aria-label="Requests and queued messages">
                {if self.targets_pending {html!{<p class="muted" role="status">{if working {"Target changes saved for the next new turn. This turn keeps its current targets; Send still steers it. Image uploads are paused until it finishes."} else {"Target changes saved. Your next message starts a turn with the new selection."}}</p>}}else{Html::default()}}
                {for self.pending.iter().map(|pending|self.approval(ctx,pending))}
                {if !self.queued.is_empty() {html!{<section class="message-queue" aria-label="Queued messages"><h2>{format!("Queued messages ({})",self.queued.len())}</h2>
                    {for self.queued.iter().map(|message|html!{<div class="queued-message"><pre>{array(&message["input"]).iter().map(|part|text(part,"text")).collect::<Vec<_>>().join("\n")}</pre>{{let operation=Operation::CancelQueued{id:id.clone(),queued_id:text(message,"id").into()};html!{<button type="button" aria-label="Cancel queued message" disabled={self.busy||!self.connected} onclick={ctx.link().callback(move |_|Msg::Run(operation.clone()))}>{"Cancel"}</button>}}}</div>})}
                    {if self.targets_pending {html!{<p class="muted">{"Send a message to apply the selected execution targets before resuming this queue."}</p>}}else if !working && !waiting {self.button(ctx,"Resume queue",Operation::ResumeQueue{id:id.clone()})}else{Html::default()}}
                </section>}}else{Html::default()}}
                {if !self.queue_error.is_empty(){html!{<p class="muted">{format!("Message queue unavailable: {}",self.queue_error)}</p>}}else{Html::default()}}
            </div>
            <form class="composer" onsubmit={ctx.link().callback(|e:SubmitEvent|{e.prevent_default();Msg::Send})}><label class="sr-only" for="prompt">{"Message"}</label><textarea id="prompt" ref={self.prompt_ref.clone()} value={self.saved.draft()} placeholder="Give the agent a task…" aria-describedby="composer-shortcut" onkeydown={ctx.link().batch_callback(move |e:web_sys::KeyboardEvent| {
                if e.key()=="Enter" && e.shift_key() != enter_sends && !e.ctrl_key() && !e.alt_key() && !e.meta_key() && !e.is_composing() && e.key_code()!=229 {
                    e.prevent_default();
                    if !e.repeat() { return Some(Msg::Send); }
                }
                None
            })} oninput={ctx.link().callback(|e|Msg::Draft(input(e)))} onpaste={ctx.link().batch_callback(|e:Event| {
                let e = e.unchecked_into::<web_sys::ClipboardEvent>();
                let file = e.clipboard_data().and_then(|data|data.files()).and_then(|files|files.get(0));
                if file.is_some() { e.prevent_default(); }
                file.map(Msg::UploadImage)
            })}/><input type="file" hidden=true ref={self.image_ref.clone()} accept="image/png,image/jpeg,image/gif,image/webp" aria-label="Upload image" onchange={ctx.link().batch_callback(|e:Event| {
                let input=e.target_unchecked_into::<HtmlInputElement>();
                let file=input.files().and_then(|files|files.get(0)); input.set_value(""); file.map(Msg::UploadImage)
            })}/><div class="composer-actions"><label class="checkbox composer-enter-sends"><input type="checkbox" checked={enter_sends} onchange={ctx.link().callback(|e:Event|Msg::EnterSends(e.target_unchecked_into::<HtmlInputElement>().checked()))}/>{"Enter sends"}</label><button type="button" disabled={self.busy||!self.connected||(self.targets_pending&&working)} onclick={ctx.link().callback(|_|Msg::ChooseImage)}>{if self.busy && self.upload_anchor.is_some(){"Uploading…"}else{"Attach image"}}</button><button class="primary" title="Send now; during work, steer the current turn" disabled={!self.can_send()}>{"Send"}</button><button type="button" title="Start a separate turn after the current turn finishes" disabled={!self.can_send()||!(working||waiting)} onclick={ctx.link().callback(|_|Msg::Queue)}>{"Queue for later"}</button>{if working||waiting{self.button(ctx,"Interrupt",Operation::Interrupt{id:id.clone()})}else{html!{<button type="button" disabled=true>{"Interrupt"}</button>}}}
                <button type="button" class="controls-toggle" onclick={ctx.link().callback(|_|Msg::Controls(true))}>{"Session controls"}</button>
                {if self.controls["goal"].is_object(){html!{<button type="button" class="goal-indicator" title={text(&self.controls["goal"],"objective").to_owned()} onclick={ctx.link().callback(|_|Msg::Controls(true))}>{format!("Goal · {}{}",text(&self.controls["goal"],"status"),if self.connected{""}else{" · stale"})}</button>}}else{Html::default()}}
                <button type="button" class="background-toggle" onclick={ctx.link().callback(|_|Msg::Background(true))}>{self.background["data"].as_array().map(|rows|format!("Background terminals ({})",rows.len())).unwrap_or_else(||"Background terminals · unknown".into())}</button>
                {crate::usage::context(&self.current,false)}
                </div><p id="composer-shortcut" class="composer-shortcut">{if enter_sends {"Enter: send · Shift+Enter: newline"} else {"Enter: newline · Shift+Enter: send"}}</p></form>
            {self.background_panel(ctx)}
            {if self.show_controls {html!{<crate::modal::Modal title="Session controls" compact=true onclose={ctx.link().callback(|_|Msg::Controls(false))}>
                <div class="compact-controls">
                {self.rename_session_view(ctx)}
                {self.controls_view(ctx,working||waiting)}
                <section class="control-section" aria-label="Execution settings"><crate::ui::SectionTitle>{"Execution"}</crate::ui::SectionTitle>
                {self.target_picker(ctx,working||waiting)}
                <section class="session-settings"><h3>{"Sandbox permissions"}</h3>{for array(&self.current["targets"]).iter().map(|t|html!{<p class="muted">{"Working directory: "}<code>{text(t,"cwd")}</code></p>})}
                    <section class="runtime-panel"><p>{"Active sandbox: "}<strong>{sandbox_name(text(&self.current["effective_sandbox"],"type"))}</strong></p>{self.sandbox(ctx,"session_sandbox","Session sandbox")}
                    {if self.saved.field("session_sandbox") != text(&self.current,"sandbox") {html!{<p class="sandbox-pending" role="status">{"Selection not applied. Active sandbox remains as shown above."}</p>}}else{Html::default()}}
                    {if self.saved.field("session_sandbox").is_empty(){html!{<p class="muted">{"No override preserves the current policy on a connected session; it does not enable full access."}</p>}}else{Html::default()}}
                    {if !working && !waiting && self.current["archived"]!=true{self.checked_button(ctx,"Apply sandbox",sandbox_choice(&self.saved.field("session_sandbox")).map(|sandbox|Operation::Sandbox{id:id.clone(),input:demodex_protocol::SandboxChoice{sandbox}}))}else{html!{<p class="muted control-warning" role="status">{if self.current["archived"]==true{"Restore the session before changing sandbox settings."}else{"Session active — wait until idle to change sandbox settings."}}</p>}}}</section>
                </section>

                </section>
                <section class="control-section" aria-label="Session context"><crate::ui::SectionTitle>{"Context and identity"}</crate::ui::SectionTitle>
                    {crate::usage::context(&self.current,true)}
                    {crate::overview::context_view(&self.current,&self.targets)}
                    <button onclick={ctx.link().callback(|_|Msg::Diagnostics(true))}>{format!("Protocol events ({})",self.events.len())}</button>
                </section>
                <section class="session-archive control-section" aria-label="Archive session"><crate::ui::SectionTitle>{"Session history"}</crate::ui::SectionTitle>
                    {if self.current["archived"]==true{html!{<><p class="muted">{"Archived on this server. Restore this session to resume work."}</p>{self.button(ctx,"Restore session",Operation::Archive{id:id.clone(),archived:false})}</>}}else{html!{<><p class="muted">{"Archive a stopped session without deleting its history."}</p>{if matches!(status,"idle"|"connected"|"disconnected") && !working && !waiting && self.queued.is_empty() && self.controls["goal"]["status"]!="active" {self.button(ctx,"Archive session",Operation::Archive{id:id.clone(),archived:true})}else{html!{<><button disabled=true>{"Archive session"}</button><p class="muted control-warning" role="status">{"Stop the current turn, pause any active goal, and remove queued messages before archiving."}</p></>}}}</>}}}
                </section>

                {self.modal_error(ctx)}
                </div>
            </crate::modal::Modal>}}else{Html::default()}}
            {if self.show_diagnostics {html!{<crate::modal::Modal title="Protocol events" onclose={ctx.link().callback(|_|Msg::Diagnostics(false))}>
                <pre>{serde_json::to_string_pretty(&self.events).unwrap_or_default()}</pre>
                {self.modal_error(ctx)}
            </crate::modal::Modal>}}else{Html::default()}}
        </>}
    }
    fn approval(&self, ctx: &Context<Self>, pending: &Value) -> Html {
        let disabled = self.busy || !self.connected || text(pending, "state") != "pending";
        let key = text(pending, "key");
        let method = text(pending, "method");
        html! {<section key={key.to_owned()} class="approval" aria-label={crate::approval::title(method)}>
            <div class="approval-heading"><h2>{crate::approval::title(method)}</h2><span class="approval-status" role="status">{crate::approval::status(text(pending,"state"),self.connected)}</span></div>
            {if text(pending,"state")=="unavailable"{html!{<p>{"The connection owning this request ended. No answer was selected automatically; this request cannot be answered on a new connection."}</p>}}else{Html::default()}}
            {if matches!(method,"item/commandExecution/requestApproval"|"item/fileChange/requestApproval") {
                html!{<>{crate::approval::summary(method,&pending["params"])}
                    <div class="approval-actions">{for [("Approve once","accept","Allow this request"),("Decline","decline","Reject this request"),("Cancel turn","cancel","Stop the current turn")].into_iter().map(|(label,decision,hint)|{let p=pending.clone();html!{<button type="button" class={if decision=="cancel"{"approval-cancel"}else{""}} title={hint} disabled={disabled} onclick={ctx.link().callback(move |_|Msg::Answer(p.clone(),Some(decision.into())))}>{label}</button>}})}</div>
                    {crate::approval::details(method,&pending["params"])}
                </>}
            }else{
                let p=pending.clone();html!{<form onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Answer(p.clone(),None)})}>
                    {if method=="item/tool/requestUserInput"{html!{<>{for array(&pending["params"]["questions"]).iter().map(|q|{let name=format!("{}:{key}:{}",self.saved.host,text(q,"id"));let value=self.saved.answer(key,text(q,"id"));html!{<label>{text(q,"question")}<span class="muted">{array(&q["options"]).iter().map(|o|format!("{}: {}",text(o,"label"),text(o,"description"))).collect::<Vec<_>>().join("\n")}</span><textarea required=true disabled={disabled} value={value} oninput={ctx.link().callback(move |e|Msg::AnswerDraft(name.clone(),input(e)))}/></label>}})}</>}}else{let name=format!("{}:{key}:raw",self.saved.host);html!{<><p class="muted">{"This request needs a custom response. Review its details and enter an explicit JSON answer."}</p>{crate::approval::details(method,&pending["params"])}<label>{"Protocol response (JSON)"}<textarea required=true disabled={disabled} value={self.saved.answers.get(&name).cloned().unwrap_or_default()} oninput={ctx.link().callback(move |e|Msg::AnswerDraft(name.clone(),input(e)))}/></label></>}}}
                    <button disabled={disabled}>{if method=="item/tool/requestUserInput"{"Send answer"}else{"Send explicit response"}}</button>
                </form>}
            }}
        </section>}
    }
    fn targets_locked(&self) -> bool {
        !self.connected
            || self
                .pending
                .iter()
                .any(|p| matches!(text(p, "state"), "pending" | "responding" | "delivered"))
            || !matches!(
                text(&self.current, "status"),
                "idle" | "connected" | "working" | "active" | "running"
            )
            || self.current["archived"] == true
            || !self.queued.is_empty()
            || self.controls["goal"]["status"] == "active"
            || self.busy
    }
    fn target_picker(&self, ctx: &Context<Self>, active: bool) -> Html {
        let draft_key = format!("target-draft:{}", self.saved.selected);
        let selected = self
            .saved
            .fields
            .get(&draft_key)
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or_else(|| self.target_selection.clone());
        let chosen = array(&selected);
        let locked = self.targets_locked();
        let selection = demodex_protocol::SelectTargets {
            targets: chosen
                .iter()
                .map(|target| demodex_protocol::Selection {
                    id: text(target, "id").into(),
                    cwd: text(target, "cwd").into(),
                })
                .collect(),
        };
        let operation = if active {
            Operation::ChangeTargets {
                id: self.saved.selected.clone(),
                input: selection.clone(),
                mode: demodex_protocol::TargetChangeMode::NextTurn,
            }
        } else {
            Operation::SelectTargets {
                id: self.saved.selected.clone(),
                input: selection.clone(),
            }
        };
        let interrupt = Operation::ChangeTargets {
            id: self.saved.selected.clone(),
            input: selection,
            mode: demodex_protocol::TargetChangeMode::Interrupt,
        };
        let mut blockers = Vec::new();
        if self.current["archived"] == true { blockers.push("Restore this session to change targets."); }
        else if !self.connected || text(&self.current,"status") == "disconnected" { blockers.push("Connect this session to change targets."); }
        if self.controls["goal"]["status"] == "active" { blockers.push("Pause the active goal."); }
        if !self.queued.is_empty() { blockers.push("Clear queued messages."); }
        if self.pending.iter().any(|p| matches!(text(p,"state"),"pending"|"responding"|"delivered")) { blockers.push("Resolve pending decisions."); }
        let requires_full_access = chosen.iter().any(|target| matches!(text(target,"id").split('-').next(),Some("ssh"|"container"))) && self.current["sandbox"] != "danger-full-access";
        html! {<section class="target-picker"><h3>{"Executors and working directories"}</h3>
            {if requires_full_access{html!{<p class="muted control-warning" role="status">{"SSH and containers require danger-full-access."}</p>}}else{Html::default()}}
            {if self.targets_pending{html!{<p role="status">{if active {"Executors and directories saved for the next new turn. Current turn access is unchanged."} else {"Executors and directories saved. Send a message to apply them before resuming a goal or queue."}}</p>}}else{Html::default()}}
            {if active && self.targets_pending {html!{<div class="effective-targets"><strong>{"Current turn executors"}</strong><ul>{for array(&self.current["targets"]).iter().map(|target|html!{<li>{crate::overview::environment_label(text(target,"id"),&self.targets)}{" · "}<code>{text(target,"cwd")}</code></li>})}</ul></div>}}else{Html::default()}}
            <fieldset disabled={locked}>
                {for self.targets.iter().filter(|target|target["owner"].is_null()||text(target,"owner")==self.saved.selected).map(|target|{
                    let id=text(target,"id").to_owned();
                    let enabled=chosen.iter().any(|s|s["id"]==id);
                    let mut toggled=chosen.clone();
                    if enabled {toggled.retain(|s|s["id"]!=id);} else {toggled.push(json!({"id":id,"cwd":target["cwd"]}));}
                    html!{<label class="checkbox"><input type="checkbox" checked={enabled} disabled={!enabled && target["available"]!=true} onchange={ctx.link().callback(move |_|Msg::TargetDraft(json!(toggled)))}/>{format!("{} · {}{}",text(target,"name"),text(target,"kind"),if target["available"]==true{""}else{" · unavailable"})}</label>}
                })}
                {if !chosen.is_empty(){html!{<h4>{"Working directories"}</h4>}}else{Html::default()}}
                {for chosen.iter().enumerate().map(|(index,target)|{
                    let name=self.targets.iter().find(|t|t["id"]==target["id"]).map(|t|text(t,"name")).unwrap_or("Unavailable target");
                    let current=chosen.clone();
                    let mut removed=chosen.clone(); removed.remove(index);
                    let mut primary=chosen.clone();
                    let entry=primary.remove(index);primary.insert(0,entry);
                    html!{<div class="target-directory"><crate::ui::FieldAction><label>{format!("{}{} working directory",name,if index==0{" (primary)"}else{""})}<input value={text(target,"cwd").to_owned()} oninput={ctx.link().callback(move |e:InputEvent|{let mut next=current.clone();next[index]["cwd"]=json!(input(e));Msg::TargetDraft(json!(next))})}/></label>
                        <crate::directory_picker::DirectoryPicker key={format!("{}:{}",self.generation,text(target,"id"))} client={self.connected.then(||self.client.clone()).flatten()} target={text(target,"id").to_owned()} path={text(target,"cwd").to_owned()} onchoose={ctx.link().callback({let targets=chosen.clone();let id=text(target,"id").to_owned();move |path:String|{let mut next=targets.clone();if let Some(target)=next.iter_mut().find(|t|t["id"]==id){target["cwd"]=json!(path);}Msg::TargetDraft(json!(next))}})}/></crate::ui::FieldAction>
                        {if index>0{html!{<button type="button" onclick={ctx.link().callback(move |_|Msg::TargetDraft(json!(primary)))}>{"Make primary"}</button>}}else{Html::default()}}
                        <button type="button" onclick={ctx.link().callback(move |_|Msg::TargetDraft(json!(removed)))}>{"Remove"}</button>
                    </div>}
                })}
                {self.button(ctx,if active {"Save for next turn"} else {"Save executors and directories"},operation)}
                {if active {self.button(ctx,"Interrupt and save",interrupt)}else{Html::default()}}
            </fieldset>
            <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("session-ssh".into()))}>{"+ Add SSH executor to this session"}</crate::ui::AddButton>
            {for self.targets.iter().filter(|target|text(target,"owner")==self.saved.selected).map(|target|{
                let id=text(target,"id").to_owned();
                html!{<section class="environment-card"><h3>{text(target,"name")}</h3><p>{format!("Private SSH executor · {}",text(target,"destination"))}</p>
                    {self.button(ctx,"Check SSH connection",Operation::CheckSshTarget{id:id.clone()})}
                    {self.button(ctx,"Replace SSH executor",Operation::ReconnectSshTarget{id:id.clone()})}
                    {if array(&target["users"]).is_empty(){self.button(ctx,"Forget executor",Operation::ForgetTarget{id})}else{Html::default()}}
                </section>}
            })}
            {if !blockers.is_empty(){html!{<p class="muted control-warning" role="status">{blockers.join(" ")}</p>}}else{Html::default()}}
        </section>}
    }

    fn session_ssh_form(&self, ctx: &Context<Self>, locked: bool) -> Html {
        let payload = demodex_protocol::SshTarget {
            name: self.saved.field("ssh_name"),
            destination: self.saved.field("ssh_destination"),
            cwd: self.saved.field("ssh_cwd"),
            port: nonempty(self.saved.field("ssh_port")).map(|p| p.parse::<u16>().unwrap_or(0)),
            identity_file: nonempty(self.saved.field("ssh_identity")),
            known_hosts_file: nonempty(self.saved.field("ssh_known_hosts")),
        };
        let id = self.saved.selected.clone();
        html! {<>
            <form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::RegisterSessionSshTarget{id:id.clone(),input:payload.clone()})})}>
                <fieldset disabled={locked}>
                    {self.field(ctx,"ssh_name","SSH executor name","Build machine")}{self.field(ctx,"ssh_destination","SSH destination","user@host or SSH config alias")}{self.field(ctx,"ssh_port","SSH port (optional)","22")}{self.field(ctx,"ssh_identity","Identity file on this server (optional)","/home/user/.ssh/id_ed25519")}{self.field(ctx,"ssh_known_hosts","Known hosts file on this server (optional)","/home/user/.ssh/known_hosts")}{self.field(ctx,"ssh_cwd","Remote working directory","/workspace")}
                    {if self.current["sandbox"]!="danger-full-access"{html!{<p class="muted control-warning" role="status">{"Select danger-full-access before adding SSH."}</p>}}else{Html::default()}}
                    <button class="primary" disabled={self.current["sandbox"]!="danger-full-access"}>{"Check and add SSH executor"}</button>
                </fieldset>
            </form>
        </>}
    }

    fn shared_ssh_form(&self, ctx: &Context<Self>) -> Html {
        let payload = demodex_protocol::SshTarget {
            name: self.saved.field("shared_ssh_name"),
            destination: self.saved.field("shared_ssh_destination"),
            cwd: self.saved.field("shared_ssh_cwd"),
            port: nonempty(self.saved.field("shared_ssh_port")).map(|p| p.parse::<u16>().unwrap_or(0)),
            identity_file: nonempty(self.saved.field("shared_ssh_identity")),
            known_hosts_file: nonempty(self.saved.field("shared_ssh_known_hosts")),
        };
        html! {<>
            <form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::RegisterSshTarget{input:payload.clone()})})}>
                <fieldset disabled={self.busy||!self.connected}>
                    {self.field(ctx,"shared_ssh_name","SSH executor name","Build machine")}{self.field(ctx,"shared_ssh_destination","SSH destination","user@host or SSH config alias")}{self.field(ctx,"shared_ssh_port","SSH port (optional)","22")}{self.field(ctx,"shared_ssh_identity","Identity file on this server (optional)","/home/user/.ssh/id_ed25519")}{self.field(ctx,"shared_ssh_known_hosts","Known hosts file on this server (optional)","/home/user/.ssh/known_hosts")}{self.field(ctx,"shared_ssh_cwd","Remote working directory","/workspace")}

                    <button class="primary">{"Check and add SSH executor"}</button>
                </fieldset>
            </form>
        </>}
    }

    fn target_registry(&self, ctx: &Context<Self>) -> Html {
        html! {<section class="target-registry"><h3>{"Global executors"}</h3>{if !self.target_notice.is_empty(){html!{<p role="status">{self.target_notice.clone()}</p>}}else{Html::default()}}
            {for self.targets.iter().filter(|target|target["owner"].is_null()&&target["kind"]!="container").map(|target|{
                let users=array(&target["users"]);
                html!{<article class="environment-card"><h3>{text(target,"name")}</h3><p>{format!("{} · {}",text(target,"kind"),if matches!(text(target,"kind"),"external"|"ssh"){"registered · checked on attach"}else if target["available"]==true{"available"}else{"stopped / unavailable"})}</p><code>{text(target,"cwd")}</code>
                    {if text(target,"kind")=="ssh"{html!{<><p>{format!("SSH: {}",text(target,"destination"))}</p>{self.button(ctx,"Check SSH connection",Operation::CheckSshTarget{id:text(target,"id").into()})}{self.button(ctx,"Replace SSH executor",Operation::ReconnectSshTarget{id:text(target,"id").into()})}<p class="muted">{"Replacing this connection disconnects every session using it. Pause those sessions first, then reconnect them afterward."}</p></>}}else{Html::default()}}
                    <p>{if users.is_empty(){"No attached sessions".into()}else{format!("Used by: {}",users.iter().map(|id|self.sessions.iter().find(|s|s["id"]==*id).map(crate::overview::identity).unwrap_or_else(||id.as_str().unwrap_or("").into())).collect::<Vec<_>>().join(", "))}}</p>
                    {if matches!(text(target,"kind"),"external"|"ssh") && users.is_empty(){self.button(ctx,"Forget executor",Operation::ForgetTarget{id:text(target,"id").into()})}else{Html::default()}}
                </article>}
            })}
            <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("shared-ssh".into()))}>{"+ Add SSH executor"}</crate::ui::AddButton>
            <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("external".into()))}>{"+ Register external executor"}</crate::ui::AddButton>
        </section>}
    }

    fn external_target_form(&self, ctx: &Context<Self>) -> Html {
        let payload = demodex_protocol::RegisterTarget {
            name: self.saved.field("target_name"),
            url: self.saved.field("target_url"),
            cwd: self.saved.field("target_cwd"),
        };
        html!{<form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::RegisterTarget{input:payload.clone()})})}>
                {self.field(ctx,"target_name","Executor name","Build machine")}{self.field(ctx,"target_url","Executor WebSocket URL","ws://127.0.0.1:4501")}{self.field(ctx,"target_cwd","Default working directory","/workspace")}
                <button class="primary" disabled={self.busy||!self.connected}>{"Register executor"}</button>
            </form>}
    }

    pub(super) fn create_container_form(&self, ctx: &Context<Self>) -> Html {
        let container = demodex_protocol::NewContainer {
            name: self.saved.field("container_name"),
            engine: match self.saved.field("container_engine").as_str() { "podman" => "podman", _ => "docker" }.into(),
            image: self.saved.field("container_image"),
            memory_mib: self.saved.field("container_memory").parse().unwrap_or(4096),
            cpus: self.saved.field("container_cpus").parse().unwrap_or(2),
        };
        html! {
                    <form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::CreateContainer{input:container.clone()})})}>
                        {self.field(ctx,"container_name","Container name","Work container")}
                        <label for="container_engine">{"Container engine"}</label><select id="container_engine" onchange={ctx.link().callback(|e:Event|Msg::Field("container_engine".into(),e.target_unchecked_into::<HtmlSelectElement>().value()))}>
                            <option value="docker" selected={self.saved.field("container_engine")!="podman"}>{"Docker"}</option>
                            <option value="podman" selected={self.saved.field("container_engine")=="podman"}>{"Podman"}</option>
                        </select>
                        {self.field(ctx,"container_image","Local container image","ubuntu:24.04")}
                        <div class="resource-fields"><label>{"Container memory (MiB)"}<input type="number" min="256" max="65536" value={self.saved.field("container_memory")} placeholder="4096" oninput={ctx.link().callback(|e:InputEvent|Msg::Field("container_memory".into(),input(e)))}/></label>
                        <label>{"Container CPUs"}<input type="number" min="1" max="32" value={self.saved.field("container_cpus")} placeholder="2" oninput={ctx.link().callback(|e:InputEvent|Msg::Field("container_cpus".into(),input(e)))}/></label></div>

                        <button class="primary" disabled={self.busy||!self.connected}>{"Create and start container"}</button>
                    </form>
        }
    }

    fn environment_view(&self, ctx: &Context<Self>) -> Html {
        html! {<div class="server-settings">
            <section class="runtime-panel account-status"><crate::ui::SectionTitle>{"Codex account"}</crate::ui::SectionTitle>{if self.runtime["running"].as_bool()==Some(true){if !self.runtime["account"].is_null(){html!{<p>{format!("Signed in {} · {} profile",text(&self.runtime["account"],"email"),text(&self.runtime,"profile"))}</p>}}else{self.button(ctx,"Sign in with ChatGPT",Operation::Login)}}else{self.button(ctx,"Start Codex runtime",Operation::StartRuntime)}}
                {if !self.login.is_null(){html!{<><a href={text(&self.login,"verificationUrl").to_owned()} target="_blank" rel="noopener noreferrer">{"Continue sign-in in your browser"}</a><p>{"Device code: "}<strong>{text(&self.login,"userCode")}</strong></p></>}}else{Html::default()}}
                {if let Some(error)=self.runtime["error"].as_str(){html!{<p class="muted">{error}</p>}}else{Html::default()}}
            </section>
            {if let Some(client)=self.client.clone(){html!{<crate::notifications::Notifications key={format!("{}:{}",self.saved.host,self.generation)} client={client} host={self.saved.host.clone()}/>}}else{Html::default()}}
            <crate::runtime_features::RuntimeFeatures runtime={self.runtime.clone()} disabled={self.busy||!self.connected} onrun={ctx.link().callback(Msg::Run)}/>
            <section class="settings-targets"><crate::ui::SectionTitle>{"Executors"}</crate::ui::SectionTitle>
            {self.target_registry(ctx)}
            <div class="target-management-grid">
            <section class="container-management"><h3>{"Containers"}</h3>
                {for self.targets.iter().filter(|target|target["kind"]=="container").map(|target|{
                    let id=text(target,"id").strip_prefix("container-").unwrap_or_default().to_owned();
                    html!{<section class="environment-card"><h3>{text(target,"name")}</h3><p>{format!("{} · {} · {}",text(target,"engine"),text(target,"image"),text(target,"status"))}</p>
                        <div class="environment-actions">{if target["status"]=="running"{self.button(ctx,"Stop · keep workspace",Operation::StopContainer{id})}else{self.button(ctx,"Start container",Operation::StartContainer{id})}}</div>
                        {if let Some(error)=target["error"].as_str(){html!{<p class="error">{error}</p>}}else{Html::default()}}
                    </section>}
                })}
                <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("container".into()))}>{"+ Create container"}</crate::ui::AddButton>
            </section>
            <section class="vm-management"><h3>{"Virtual machines"}</h3>
                {for self.environments.iter().map(|env|{let id=text(env,"id").to_owned();let status=text(env,"status");html!{<section class="environment-card"><h3>{text(env,"name")}</h3><p>{status}</p><div class="environment-actions">{if status=="running"{self.button(ctx,"Stop · keep disk",Operation::StopEnvironment{id})}else{self.button(ctx,"Start",Operation::StartEnvironment{id})}}</div>{if let Some(error)=env["error"].as_str(){html!{<p class="error">{error}</p>}}else{Html::default()}}</section>}})}
                <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("vm".into()))}>{"+ Create VM"}</crate::ui::AddButton>
            </section>
            </div></section>
        </div>}
    }
}
fn nonempty(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

fn sandbox_choice(value: &str) -> Result<Option<demodex_protocol::Sandbox>, String> {
    use demodex_protocol::Sandbox;
    match value {
        "" => Ok(None),
        "read-only" => Ok(Some(Sandbox::ReadOnly)),
        "workspace-write" => Ok(Some(Sandbox::WorkspaceWrite)),
        "danger-full-access" => Ok(Some(Sandbox::DangerFullAccess)),
        _ => Err("Select a valid sandbox policy before submitting.".into()),
    }
}

#[cfg(test)]
mod form_tests {
    #[test]
    fn malformed_saved_sandbox_never_becomes_default_access() {
        assert_eq!(super::sandbox_choice(""), Ok(None));
        assert_eq!(
            super::sandbox_choice("read-only"),
            Ok(Some(demodex_protocol::Sandbox::ReadOnly))
        );
        assert!(super::sandbox_choice("corrupted-setting").is_err());
    }
}
