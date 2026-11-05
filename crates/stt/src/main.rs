use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use demodex_stt::{Audio, Reply, Request, VERSION, read, write};
use std::{path::PathBuf, sync::Arc, time::Duration, os::unix::fs::PermissionsExt};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{UnixListener, UnixStream}, sync::Semaphore};
use transcribe_cpp::{CancelToken, Model, RunOptions};
#[derive(Parser)]
struct Cli { #[command(subcommand)] command: Command }
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long)] socket: PathBuf,
        #[arg(long)] model: PathBuf,
        /// Require this backend; never silently fall back to CPU.
        #[arg(long, default_value = "cpu")] backend: String,
    },
    /// SSH stdio bridge: no shell parsing of audio and no TCP listener.
    Connect { #[arg(long)] socket: PathBuf },
}
struct CancelOnDrop(CancelToken);
impl Drop for CancelOnDrop { fn drop(&mut self) { self.0.cancel(); } }
#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Connect { socket } => {
            let stream = UnixStream::connect(socket).await?;
            let (mut rx, mut tx) = stream.into_split();
            // EOF on SSH stdin cancels the socket, even while inference is running.
            let mut stdin = tokio::io::stdin();
            let mut stdout = tokio::io::stdout();
            tokio::select! {
                r = tokio::io::copy(&mut stdin, &mut tx) => { r?; },
                r = tokio::io::copy(&mut rx, &mut stdout) => { r?; },
            }
            Ok(())
        }
        Command::Serve { socket, model, backend } => {
            let backend = match backend.as_str() {
                "cpu" => transcribe_cpp::Backend::Cpu,
                "vulkan" => transcribe_cpp::Backend::Vulkan,
                "rocm" => transcribe_cpp::Backend::Rocm,
                _ => anyhow::bail!("Backend must be cpu, vulkan or rocm"),
            };
            let model = Model::load_with(&model, &transcribe_cpp::ModelOptions {
                backend, ..Default::default()
            })?;
            let model = Arc::new(model);
            ensure!(model.arch().contains("cohere"), "This service currently supports Cohere models");
            // systemd owns the private RuntimeDirectory. Refuse to replace files/sockets.
            let listener = UnixListener::bind(&socket)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
            eprintln!("STT ready: {} on {}", model.arch(), format!("{:?}", model.backend()));
            let slots = Arc::new(Semaphore::new(1));
            loop {
                let (mut stream, _) = listener.accept().await?;
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    let _ = tokio::time::timeout(Duration::from_secs(1), write(&mut stream, &Reply::Error("STT busy; try again shortly".into()))).await;
                    continue;
                };
                let model = model.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(360), serve(&mut stream, model, permit)).await;
                    if let Err(error) = result.unwrap_or_else(|_| Err(anyhow::anyhow!("STT request timed out"))) {
                        // Errors contain no audio or transcript.
                        let _ = tokio::time::timeout(Duration::from_secs(2), write(&mut stream, &Reply::Error(error.to_string()))).await;
                    }
                    let _ = stream.shutdown().await;
                });
            }
        }
    }
}
async fn serve(stream: &mut UnixStream, model: Arc<Model>, permit: tokio::sync::OwnedSemaphorePermit) -> Result<()> {
    let start = tokio::time::timeout(Duration::from_secs(10), read(stream)).await??;
    let Request::Start { version, language } = start else { anyhow::bail!("Expected Start") };
    ensure!(version == VERSION, "STT protocol mismatch");
    ensure!(demodex_stt::language_valid(&language), "Unsupported language");
    write(stream, &Reply::Ready { model: format!("{} ({})", model.arch(), format!("{:?}", model.backend())) }).await?;
    let mut audio = Audio::default();
    loop {
        match tokio::time::timeout(Duration::from_secs(15), read(stream)).await?? {
            Request::Audio { sequence, pcm } => {
                audio.push(sequence, &pcm)?;
                write(stream, &Reply::Ack).await?;
            }
            Request::Finish => break,
            _ => anyhow::bail!("Unexpected Start"),
        }
    }
    ensure!(audio.pcm.len() >= 1600, "Recording is too short");
    // Do not ask the model to invent text for digital silence.
    if audio.pcm.iter().all(|v| v.abs() < 0.0001) {
        write(stream, &Reply::Text(String::new())).await?;
        return Ok(());
    }
    let cancel = CancelOnDrop(CancelToken::new());
    let token = cancel.0.clone();
    let task = tokio::task::spawn_blocking(move || -> Result<String> {
        // Keep the slot until native inference actually stops, including cancellation.
        let _permit = permit;
        let mut session = model.session()?;
        session.set_cancel_token(&token);
        let result = session.run(&audio.pcm, &RunOptions { language: Some(language), ..Default::default() })?;
        ensure!(!session.was_truncated(), "Transcription was truncated; use a shorter recording");
        Ok(result.text.trim().to_owned())
    });
    let mut extra = [0u8; 1];
    let text = tokio::select! {
        result = task => result??,
        _ = stream.read(&mut extra) => anyhow::bail!("Recording disconnected or cancelled"),
    };
    ensure!(text.len() <= 32_768, "Transcript exceeds limit");
    write(stream, &Reply::Text(text)).await
}
