mod background;
mod controls;
pub mod daemon;
mod http;
mod manager;
mod orchestrator;
mod rpc;
mod service;
mod session_context;
mod ssh;
mod store;
mod targets;
mod uploads;
mod usage;
mod vm;
mod wormhole;

use manager::Manager;
use std::sync::Arc;
#[derive(Clone)]
pub struct Service {
    manager: Arc<Manager>,
    _lock: Option<Arc<std::fs::File>>,
    orchestrator: Arc<orchestrator::Orchestrator>,
    commands: Arc<tokio::sync::Mutex<()>>,
    lifecycle: Arc<Lifecycle>,
}

#[derive(Default)]
struct Lifecycle {
    closing: std::sync::atomic::AtomicBool,
    calls: tokio::sync::RwLock<()>,
}
impl Lifecycle {
    async fn admit(&self) -> anyhow::Result<tokio::sync::RwLockReadGuard<'_, ()>> {
        use std::sync::atomic::Ordering;
        anyhow::ensure!(!self.closing.load(Ordering::Acquire), "service is stopping");
        let guard = self.calls.read().await;
        anyhow::ensure!(!self.closing.load(Ordering::Acquire), "service is stopping");
        Ok(guard)
    }
    fn close(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Owns executor monitoring and orderly shutdown. Await `shutdown` before leaving
/// the Tokio runtime; dropping this owner requests cleanup asynchronously.
pub struct Runtime {
    service: Service,
    stop: tokio_util::sync::CancellationToken,
    worker: Option<tokio::task::JoinHandle<()>>,
}
impl Runtime {
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        let service = Service::open(config)?;
        if let Err(error) = service.start().await {
            service.shutdown().await;
            return Err(error);
        }
        let stop = tokio_util::sync::CancellationToken::new();
        let stopped = stop.clone();
        let app = service.clone();
        let worker = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                        if let Err(error) = app.monitor().await {
                            tracing::warn!("runtime monitor: {error:#}");
                        }
                    }
                }
            }
            app.shutdown().await;
        });
        Ok(Self {
            service,
            stop,
            worker: Some(worker),
        })
    }
    pub fn service(&self) -> Service {
        self.service.clone()
    }
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.service.lifecycle.close();
        self.stop.cancel();
        if let Some(worker) = self.worker.take() {
            worker.await?;
        }
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.service.lifecycle.close();
        self.stop.cancel();
    }
}

pub(crate) type App = Service;
pub use demodex_protocol as protocol;

/// Storage and executor configuration for an embedded service. No network listener.
pub struct Config {
    pub data_dir: std::path::PathBuf,
    pub vm_image: Option<std::path::PathBuf>,
    pub host_workspace: Option<std::path::PathBuf>,
    pub codex_home: Option<std::path::PathBuf>,
}
impl Service {
    pub(crate) fn open(config: Config) -> anyhow::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        anyhow::ensure!(
            config.codex_home.is_none() || config.host_workspace.is_some(),
            "codex_home requires host_workspace"
        );
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.data_dir)?;
        anyhow::ensure!(
            std::fs::metadata(&config.data_dir)?.permissions().mode() & 0o077 == 0,
            "data directory must be private (mode 0700)"
        );
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(config.data_dir.join("manager.lock"))?;
        lock.try_lock()
            .map_err(|e| anyhow::anyhow!("data directory is already in use: {e}"))?;
        let manager = Manager::new(store::Store::open(&config.data_dir.join("state.sqlite"))?);
        let host_workspace = config
            .host_workspace
            .map(|path| path.canonicalize())
            .transpose()?;
        let codex_home = config
            .codex_home
            .map(|path| path.canonicalize())
            .transpose()?;
        let orchestrator = orchestrator::Orchestrator::new(
            manager.clone(),
            config.data_dir.canonicalize()?,
            config.vm_image,
            host_workspace,
            codex_home,
        );
        Ok(Self {
            manager,
            orchestrator,
            commands: Arc::new(tokio::sync::Mutex::new(())),
            lifecycle: Arc::new(Lifecycle::default()),
            _lock: Some(Arc::new(lock)),
        })
    }
    /// Start the configured native runtime; isolated deployments start explicitly.
    async fn start(&self) -> anyhow::Result<()> {
        if self.orchestrator.is_host_mode() {
            self.orchestrator.start_runtime().await?;
        }
        Ok(())
    }
    /// Hosts should call this periodically while the service is running.
    async fn monitor(&self) -> anyhow::Result<()> {
        self.orchestrator.monitor().await
    }
    async fn shutdown(&self) {
        self.lifecycle.close();
        let _drained = self.lifecycle.calls.write().await;
        self.orchestrator.shutdown().await;
    }
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.manager.updates.subscribe()
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    fn config(root: &std::path::Path) -> Config {
        Config {
            data_dir: root.join("state"),
            vm_image: None,
            host_workspace: None,
            codex_home: None,
        }
    }
    #[tokio::test]
    async fn shutdown_drains_admitted_calls_and_rejects_new_calls() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let runtime = Runtime::start(config(root.path())).await?;
        let service = runtime.service();
        let admitted = service.lifecycle.admit().await?;
        let shutdown = tokio::spawn(runtime.shutdown());
        tokio::task::yield_now().await;
        assert!(
            service
                .call("", protocol::Operation::Sessions)
                .await
                .is_err()
        );
        assert!(!shutdown.is_finished());
        drop(admitted);
        shutdown.await??;
        assert!(
            Runtime::start(config(root.path())).await.is_err(),
            "handles retain directory ownership"
        );
        drop(service);
        Runtime::start(config(root.path()))
            .await?
            .shutdown()
            .await?;
        Ok(())
    }
    #[tokio::test]
    async fn dropped_owner_closes_handles() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let runtime = Runtime::start(config(root.path())).await?;
        let service = runtime.service();
        drop(runtime);
        assert!(
            service
                .call("", protocol::Operation::Sessions)
                .await
                .is_err()
        );
        Ok(())
    }
}
