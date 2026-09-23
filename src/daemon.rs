use crate::{http, vm, wormhole};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::{
    io::Write,
    net::SocketAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
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
    /// Read JSON commands on stdin and emit JSON results over a native Wormhole connection.
    Call {
        #[arg(long, default_value = "ws://127.0.0.1:4780/wormhole")]
        url: String,
        #[arg(long)]
        token_file: PathBuf,
    },
    /// Serve only static PWA assets; owns no sessions or Codex processes.
    Web {
        #[arg(long, default_value = "127.0.0.1:4782")]
        bind: SocketAddr,
        #[arg(long, default_value = "web/.rust-dist")]
        directory: PathBuf,
    },
}
pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Call { url, token_file }) => {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let token = std::fs::read_to_string(token_file)?;
            let client = demodex_client::Client::connect(&url, token.trim().into()).await?;
            let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
            let mut stdout = tokio::io::stdout();
            while let Some(line) = lines.next_line().await? {
                #[derive(serde::Deserialize)]
                struct Call {
                    request_id: Option<String>,
                    operation: demodex_protocol::Operation,
                }
                let result = async {
                    let call: Call = serde_json::from_str(&line)?;
                    let id = call.request_id.unwrap_or_else(|| {
                        if call.operation.is_mutation() {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            String::new()
                        }
                    });
                    Ok::<_, anyhow::Error>(client.call(id, call.operation).await?.into_value())
                }
                .await
                .map_err(|error| format!("{error:#}"));
                let mut encoded = serde_json::to_vec(&result)?;
                encoded.push(b'\n');
                stdout.write_all(&encoded).await?;
                stdout.flush().await?;
            }
            return Ok(());
        }
        Some(Command::Vm(args)) => return vm::execute(args).await,
        Some(Command::Web { bind, directory }) => {
            anyhow::ensure!(
                directory.join("index.html").is_file(),
                "build the PWA first"
            );
            let server = http::Server::static_only(directory);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!("Demodex PWA: http://{}", listener.local_addr()?);
            server.serve_tcp(listener, std::future::pending()).await?;
            return Ok(());
        }
        None => {}
    }
    let runtime = crate::Runtime::start(crate::Config {
        data_dir: cli.data_dir.clone(),
        vm_image: cli.vm_image,
        host_workspace: cli.host_workspace,
        codex_home: cli.codex_home,
    })
    .await?;
    let app = runtime.service();
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
    let proxy = if cli.tailscale_user.is_empty() {
        None
    } else {
        anyhow::ensure!(
            !cli.allowed_origin.is_empty(),
            "Tailscale identity requires explicit allowed origins"
        );
        let path = cli.data_dir.join("tailscale.sock");
        // The data directory is owner-only and its manager lock is held. Only
        // replace a stale socket; never remove an unrelated file or symlink.
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            use std::os::unix::fs::FileTypeExt;
            anyhow::ensure!(
                metadata.file_type().is_socket(),
                "proxy socket path is not a socket"
            );
            std::fs::remove_file(&path)?;
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Some((
            listener,
            http::Server::new(
                Some(wormhole::Endpoint::new(
                    app.clone(),
                    token.clone(),
                    cli.allowed_origin.clone(),
                    cli.tailscale_user,
                )),
                None,
            ),
        ))
    };
    let server = http::Server::new(
        Some(wormhole::Endpoint::new(
            app.clone(),
            token,
            cli.allowed_origin,
            Vec::new(),
        )),
        (!cli.api_only).then_some(cli.web_dir),
    );
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    eprintln!(
        "Demodex: http://{}\nAccess token: {}",
        listener.local_addr()?,
        token_path.display()
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown = tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("register SIGTERM");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        let _ = shutdown_tx.send(true);
    });
    let proxy_shutdown = shutdown_rx.clone();
    let proxy_task = tokio::spawn(async move {
        if let Some((listener, router)) = proxy {
            router
                .serve_unix(listener, wait_shutdown(proxy_shutdown))
                .await
        } else {
            Ok(())
        }
    });
    let served = server.serve_tcp(listener, wait_shutdown(shutdown_rx)).await;
    shutdown.abort();
    proxy_task.abort();
    runtime.shutdown().await?;
    served?;
    Ok(())
}

async fn wait_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    let _ = receiver.wait_for(|stopping| *stopping).await;
}
