mod ssh;
mod uploads;
mod usage;
mod targets;
mod manager;
mod orchestrator;
mod rpc;
mod store;
mod session_context;
mod controls;
mod background;
mod vm;
mod wormhole;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use manager::Manager;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Write,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, default_value = "127.0.0.1:4780")]
    bind: SocketAddr,
    #[arg(long, default_value = ".demodex")]
    data_dir: PathBuf,
    #[arg(long, default_value = "web/.rust-dist")]
    web_dir: PathBuf,
    /// Existing base image; otherwise managed environments build the pinned project image.
    #[arg(long)]
    vm_image: Option<PathBuf>,
    /// Run Codex and its executor directly on this host, using this working directory.
    #[arg(long)]
    host_workspace: Option<PathBuf>,
    /// Reuse an existing Codex profile directly; otherwise create a dedicated profile.
    #[arg(long, requires = "host_workspace")]
    codex_home: Option<PathBuf>,
    /// Additional browser origins allowed to connect directly to the daemon.
    #[arg(long)]
    allowed_origin: Vec<String>,
    /// Run the persistent session API without serving any frontend assets.
    #[arg(long)]
    api_only: bool,
    /// Accept these exact Tailscale login identities on a private proxy socket.
    /// TCP endpoints always require tokens. Serve must strip client identity headers.
    #[arg(long)]
    tailscale_user: Vec<String>,
}
#[derive(Subcommand)]
enum Command {
    Vm(vm::Args),
    /// Serve only static PWA assets; owns no sessions or Codex processes.
    Web {
        #[arg(long, default_value = "127.0.0.1:4782")]
        bind: SocketAddr,
        #[arg(long, default_value = "web/.rust-dist")]
        directory: PathBuf,
    },
}
#[derive(Clone)]
struct App {
    manager: Arc<Manager>,
    token: Arc<String>,
    orchestrator: Arc<orchestrator::Orchestrator>,
    commands: Arc<tokio::sync::Mutex<()>>,
}
struct Error(anyhow::Error);
impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{:#}",self.0)})),
        )
            .into_response()
    }
}
type Api<T> = std::result::Result<Json<T>, Error>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Vm(args)) => return vm::execute(args).await,
        Some(Command::Web { bind, directory }) => {
            anyhow::ensure!(directory.join("index.html").is_file(), "build the PWA first");
            let router = Router::new().fallback_service(tower_http::services::ServeDir::new(directory));
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!("Demodex PWA: http://{}", listener.local_addr()?);
            axum::serve(listener, router).await?;
            return Ok(());
        }
        None => {},
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cli.data_dir)?;
    anyhow::ensure!(
        std::fs::metadata(&cli.data_dir)?.permissions().mode() & 0o077 == 0,
        "data directory must be private (mode 0700); choose a dedicated directory"
    );
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(cli.data_dir.join("manager.lock"))?;
    lock.try_lock()
        .map_err(|e| anyhow::anyhow!("data directory is already in use: {e}"))?;
    let token_path = cli.data_dir.join("access-token");
    let token = if token_path.exists() {
        std::fs::read_to_string(&token_path)?.trim().to_string()
    } else {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&token_path)?;
        writeln!(file, "{token}")?;
        token
    };
    anyhow::ensure!(
        token.len() >= 32,
        "access-token must contain at least 32 characters"
    );
    let manager = Manager::new(store::Store::open(&cli.data_dir.join("state.sqlite"))?);
    let host_workspace=cli.host_workspace.map(|p|p.canonicalize()).transpose()?;
    let codex_home=cli.codex_home.map(|p|p.canonicalize()).transpose()?;
    let orchestrator=orchestrator::Orchestrator::new(manager.clone(),cli.data_dir.canonicalize()?,cli.vm_image,host_workspace,codex_home);
    if orchestrator.is_host_mode() {orchestrator.start_runtime().await?;}
    let app = App {
        manager,
        token: Arc::new(token),
        orchestrator:orchestrator.clone(),
        commands: Arc::new(tokio::sync::Mutex::new(())),
    };
    let proxy = if cli.tailscale_user.is_empty() {
        None
    } else {
        anyhow::ensure!(!cli.allowed_origin.is_empty(), "Tailscale identity requires explicit allowed origins");
        let path = cli.data_dir.join("tailscale.sock");
        // The data directory is owner-only and its manager lock is held. Only
        // replace a stale socket; never remove an unrelated file or symlink.
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            use std::os::unix::fs::FileTypeExt;
            anyhow::ensure!(metadata.file_type().is_socket(), "proxy socket path is not a socket");
            std::fs::remove_file(&path)?;
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Some((listener, wormhole::proxy_router(app.clone(), cli.allowed_origin.clone(), cli.tailscale_user)))
    };
    let mut router = Router::new().nest("/api", api(app.clone())).merge(wormhole::router(app,cli.allowed_origin));
    if !cli.api_only { router = router.fallback_service(
        tower_http::services::ServeDir::new(&cli.web_dir).not_found_service(
            tower_http::services::ServeFile::new(cli.web_dir.join("index.html")),
        ),
    ); }
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    eprintln!(
        "Demodex: http://{}\nAccess token: {}",
        listener.local_addr()?,
        token_path.display()
    );
    let watcher=orchestrator.clone();
    let monitor=tokio::spawn(async move {loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Err(error)=watcher.monitor().await {tracing::warn!("runtime monitor: {error:#}");}
    }});
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown = tokio::spawn(async move {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("register SIGTERM");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        let _ = shutdown_tx.send(true);
    });
    let proxy_shutdown = shutdown_rx.clone();
    let proxy_task = tokio::spawn(async move {
        if let Some((listener, router)) = proxy {
            axum::serve(listener, router).with_graceful_shutdown(wait_shutdown(proxy_shutdown)).await
        } else {
            Ok(())
        }
    });
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(wait_shutdown(shutdown_rx)).await;
    shutdown.abort();
    proxy_task.abort();
    monitor.abort();
    orchestrator.shutdown().await;
    served?;
    Ok(())
}

async fn wait_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    let _ = receiver.wait_for(|stopping| *stopping).await;
}

fn api(app: App) -> Router {
    Router::new()
        .route("/sessions", get(list).post(create))
        .route("/sessions/{id}", get(detail))
        .route("/sessions/{id}/connect", post(connect))
        .route("/sessions/{id}/archive", post(archive))
        .route("/sessions/{id}/sandbox", post(change_sandbox))
        .route("/sessions/{id}/targets", post(select_targets))
        .route("/targets", get(targets).post(register_target))
        .route("/targets/ssh", post(register_ssh_target))
        .route("/targets/{id}/check", post(check_ssh_target))
        .route("/targets/{id}/reconnect", post(reconnect_ssh_target))
        .route("/targets/{id}/forget", post(forget_target))
        .route("/sessions/{id}/models", get(models))
        .route("/sessions/{id}/model", post(change_model))
        .route("/sessions/{id}/goal", post(change_goal))
        .route("/sessions/{id}/messages", post(prompt))
        .route("/sessions/{id}/queue", post(queue_prompt))
        .route("/sessions/{id}/interrupt", post(interrupt))
        .route("/sessions/{id}/answer", post(answer))
        .route("/sessions/{id}/events", get(events))
        .route("/runtime",get(runtime_status))
        .route("/runtime/start",post(runtime_start))
        .route("/runtime/login",post(runtime_login))
        .route("/host/sessions",post(host_session))
        .route("/runtime/sessions",post(selected_session))
        .route("/environments",get(environments).post(environment_create))
        .route("/environments/{id}/start",post(environment_start))
        .route("/environments/{id}/stop",post(environment_stop))
        .route("/environments/{id}/sessions",post(environment_session))
        .route_layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .with_state(app)
}
async fn authenticate(State(app): State<App>, request: Request, next: Next) -> Response {
    let expected = format!("Bearer {}", app.token);
    if request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        != Some(expected.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
async fn list(State(app): State<App>) -> Api<Vec<store::Session>> {
    Ok(Json(app.manager.store.list()?))
}
#[derive(Deserialize)]
struct NewSession {
    name: String,
    endpoint: String,
    #[serde(default)]
    targets: Vec<store::Target>,
    thread_id: Option<String>,
    sandbox: Option<store::Sandbox>,
}
async fn create(State(app): State<App>, Json(input): Json<NewSession>) -> Api<store::Session> {
    if input.name.trim().is_empty()
        || !(input.endpoint.starts_with("ws://") || input.endpoint.starts_with("unix:///"))
    {
        return Err(Error(anyhow::anyhow!(
            "name and ws:// or absolute unix:// app-server endpoint are required"
        )));
    }
    let mut ids = std::collections::HashSet::new();
    for t in &input.targets {
        if t.id.is_empty()
            || !ids.insert(&t.id)
            || !t.cwd.starts_with('/')
            || !t.url.starts_with("ws://")
        {
            return Err(Error(anyhow::anyhow!(
                "targets need unique IDs, absolute working directories and ws:// executor URLs"
            )));
        }
    }
    let session = app.manager.store.create(
        input.name.trim(),
        &input.endpoint,
        &input.targets,
        input.thread_id.as_deref().filter(|s| !s.is_empty()),
    )?;
    app.manager.store.ensure_target_selection(&session.id)?;
    app.manager.changed();
    app.manager.store.sandbox(&session.id,input.sandbox)?;
    Ok(Json(app.manager.store.get(&session.id)?))
}
async fn detail(State(app): State<App>, Path(id): Path<String>) -> Api<Value> {
    app.manager.store.ensure_target_selection(&id)?;
    let session = app.manager.store.get(&id)?;
    let (queued, queue_error) = match app.manager.queued(&id).await {
        Ok(queued) => (queued, Value::Null),
        Err(error) => (Value::Null, json!(format!("{error:#}"))),
    };
    let controls = app.manager.control_snapshot(&id).await?;
    let background = app.manager.background_snapshot(&id).await;
    Ok(Json(json!({"session":session,"pending":app.manager.store.pending(&id)?,"queued":queued,"queue_error":queue_error,"controls":controls,"background":background,"target_selection":app.manager.store.target_selection(&id)?,"targets_pending":app.manager.store.targets_pending(&id)?})))
}

async fn targets(State(app): State<App>) -> Api<Value> {
    Ok(Json(app.orchestrator.targets().await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterTarget { name: String, url: String, cwd: String }
async fn register_target(State(app): State<App>, Json(input): Json<RegisterTarget>) -> Api<Value> {
    let result = app.manager.store.register_target(&input.name, &input.url, &input.cwd)?;
    app.manager.changed();
    Ok(Json(json!(result)))
}
async fn register_ssh_target(State(app): State<App>, Json(input): Json<ssh::Config>) -> Api<Value> {
    Ok(Json(app.orchestrator.register_ssh_target(input).await?))
}
async fn reconnect_ssh_target(State(app): State<App>, Path(id): Path<String>) -> Api<Value> {
    app.orchestrator.reconnect_ssh_target(&id).await?;
    Ok(Json(json!({"ok":true})))
}
async fn check_ssh_target(State(app): State<App>, Path(id): Path<String>) -> Api<Value> {
    app.orchestrator.check_ssh_target(&id).await?;
    Ok(Json(json!({"ok":true})))
}
async fn forget_target(State(app): State<App>, Path(id): Path<String>) -> Api<Value> {
    app.orchestrator.forget_target(&id).await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectTargets { targets: Vec<targets::Selection> }
async fn select_targets(State(app): State<App>, Path(id): Path<String>, Json(input): Json<SelectTargets>) -> Api<Value> {
    app.orchestrator.select_targets(&id, &input.targets).await?;
    Ok(Json(json!({"ok":true,"applies_on_next_message":true})))
}
#[derive(Deserialize)]
struct ArchiveChoice { archived: bool }
async fn archive(State(app): State<App>, Path(id): Path<String>, Json(input): Json<ArchiveChoice>) -> Api<Value> {
    app.manager.archive(&id, input.archived).await?;
    Ok(Json(json!({"archived":input.archived})))
}
async fn connect(State(app): State<App>, Path(id): Path<String>) -> Api<Value> {
    app.orchestrator.connect_session(&id).await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)] struct SandboxChoice {sandbox:Option<store::Sandbox>}
async fn change_sandbox(State(app):State<App>,Path(id):Path<String>,Json(input):Json<SandboxChoice>)->Api<Value> {
    app.manager.change_sandbox(&id,input.sandbox).await?;
    app.orchestrator.connect_session(&id).await?;
    Ok(Json(json!({"ok":true})))
}
async fn models(State(app):State<App>,Path(id):Path<String>)->Api<Value> {
    Ok(Json(app.manager.model_catalog(&id).await?))
}
async fn change_model(State(app):State<App>,Path(id):Path<String>,Json(input):Json<controls::ModelChoice>)->Api<Value> {
    Ok(Json(app.manager.change_model(&id,input).await?))
}
async fn change_goal(State(app):State<App>,Path(id):Path<String>,Json(input):Json<controls::GoalAction>)->Api<Value> {
    Ok(Json(app.manager.change_goal(&id,input).await?))
}

async fn runtime_status(State(app):State<App>)->Api<Value> {Ok(Json(app.orchestrator.runtime_status().await?))}
async fn runtime_start(State(app):State<App>)->Api<Value> {Ok(Json(app.orchestrator.start_runtime().await?))}
async fn runtime_login(State(app):State<App>)->Api<Value> {Ok(Json(app.orchestrator.login().await?))}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedSession { name: String, targets: Vec<targets::Selection>, sandbox: Option<store::Sandbox> }
async fn selected_session(State(app): State<App>, Json(input): Json<SelectedSession>) -> Api<store::Session> {
    Ok(Json(app.orchestrator.selected_session(&input.name, &input.targets, input.sandbox).await?))
}
#[derive(Deserialize)] struct HostSession {name:String,thread_id:Option<String>,sandbox:Option<store::Sandbox>,cwd:Option<String>}
async fn host_session(State(app):State<App>,Json(input):Json<HostSession>)->Api<store::Session> {
    Ok(Json(app.orchestrator.host_session(&input.name,input.thread_id.as_deref().filter(|s|!s.is_empty()),input.sandbox,input.cwd.as_deref().map(str::trim).filter(|s|!s.is_empty())).await?))
}
async fn environments(State(app):State<App>)->Api<Vec<store::Environment>> {Ok(Json(app.manager.store.environments()?))}
#[derive(Deserialize)] struct NewEnvironment {name:String,memory_mib:u32,cpus:u16,#[serde(default)] internet:bool}
async fn environment_create(State(app):State<App>,Json(input):Json<NewEnvironment>)->Api<store::Environment> {
    let environment=app.orchestrator.create(&input.name,input.memory_mib,input.cpus,input.internet)?;
    app.orchestrator.start(&environment.id).await?;
    Ok(Json(app.manager.store.environment(&environment.id)?))
}
async fn environment_start(State(app):State<App>,Path(id):Path<String>)->Api<Value> {app.orchestrator.start(&id).await?;Ok(Json(json!({"ok":true})))}
async fn environment_stop(State(app):State<App>,Path(id):Path<String>)->Api<Value> {app.orchestrator.stop(&id).await?;Ok(Json(json!({"ok":true})))}
#[derive(Deserialize)] struct SessionName {name:String,sandbox:Option<store::Sandbox>}
async fn environment_session(State(app):State<App>,Path(id):Path<String>,Json(input):Json<SessionName>)->Api<store::Session> {
    Ok(Json(app.orchestrator.session(&id,&input.name,input.sandbox).await?))
}
#[derive(Deserialize)]
struct Prompt {
    text: String,
}
async fn queue_prompt(State(app): State<App>, Path(id): Path<String>, Json(input): Json<Prompt>) -> Api<Value> {
    Ok(Json(app.manager.queue_prompt(&id, &input.text).await?))
}
async fn prompt(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Prompt>,
) -> Api<Value> {
    Ok(Json(app.manager.prompt(&id, &input.text).await?))
}
async fn interrupt(State(app): State<App>, Path(id): Path<String>) -> Api<Value> {
    Ok(Json(app.manager.interrupt(&id).await?))
}
#[derive(Deserialize)]
struct Answer {
    key: String,
    result: Value,
}
async fn answer(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Answer>,
) -> Api<Value> {
    app.manager.answer(&id, &input.key, input.result).await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct Cursor {
    #[serde(default)]
    after: i64,
}
async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(cursor): Query<Cursor>,
) -> Api<Vec<store::Event>> {
    Ok(Json(app.manager.store.events(&id, cursor.after)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[tokio::test]
    async fn api_requires_token_even_for_reads() -> Result<()> {
        let manager=Manager::new(store::Store::open(std::path::Path::new(":memory:"))?);
        let app = api(App {
            orchestrator:orchestrator::Orchestrator::new(manager.clone(),PathBuf::from("/unused"),None,None,None),
            manager,
            token: Arc::new("secret".into()),
            commands: Arc::new(tokio::sync::Mutex::new(())),
        });
        let request = |token: Option<&str>| {
            let mut r = axum::http::Request::builder().uri("/sessions");
            if let Some(t) = token {
                r = r.header("Authorization", t)
            }
            r.body(axum::body::Body::empty()).unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request(None)).await?.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(request(Some("Bearer wrong")))
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.oneshot(request(Some("Bearer secret"))).await?.status(),
            StatusCode::OK
        );
        Ok(())
    }
}
