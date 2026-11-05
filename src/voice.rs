//! Connection-scoped STT relay with private per-daemon lossless audio archives.
use anyhow::{Context, Result, ensure};
use demodex_protocol::{VoiceCommand, VoiceReply};
use demodex_stt::{Reply, Request, read, write};
use serde::Deserialize;
use crate::voice_archive::{Archives, Archive, Outcome};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::{Child, ChildStdin, ChildStdout, Command}, sync::{Mutex, Semaphore}};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub destination: String,
    pub socket: PathBuf,
    pub identity_file: Option<PathBuf>,
    pub known_hosts_file: Option<PathBuf>,
    /// Installed helper on the mini-PC (normally /run/current-system/sw/bin/demodex-stt).
    pub program: PathBuf,
}
impl Config {
    fn validate(&self) -> Result<()> {
        ensure!(!self.destination.is_empty() && !self.destination.starts_with('-') && self.destination.chars().all(|c| c.is_ascii_alphanumeric() || "@._-".contains(c)), "Invalid STT SSH destination");
        for path in [&self.socket, &self.program] {
            let path = path.to_str().context("STT paths must be UTF-8")?;
            ensure!(path.starts_with('/') && path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)), "STT paths must be absolute, without shell metacharacters");
        }
        for path in [&self.identity_file, &self.known_hosts_file].into_iter().flatten() {
            ensure!(path.is_absolute() && path.to_str().is_some_and(|s| !s.contains(['\n','\r','\0'])), "STT SSH credential paths must be absolute UTF-8 paths");
        }
        Ok(())
    }
}
pub struct Relay { config: Option<Config>, slots: Arc<Semaphore>, stop: CancellationToken, archives: Arc<Archives> }
impl Relay {
    pub fn new(config: Option<Config>, data_dir: &std::path::Path) -> Result<Self> {
        if let Some(config) = &config { config.validate()?; }
        Ok(Self { config, slots: Arc::new(Semaphore::new(2)), stop: CancellationToken::new(), archives: Archives::new(data_dir) })
    }
    pub fn recover(&self) { self.archives.recover(); }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        // Audio tasks drop their SSH children before returning their permits.
        let _ = self.slots.clone().acquire_many_owned(2).await;
        self.archives.shutdown().await;
    }
    pub fn configured(&self) -> bool { self.config.is_some() }
}
struct Stream {
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
    archive: Archive,
    samples: usize,
    sequence: u32,
}
impl Stream {
    async fn exchange(&mut self, request: Request) -> Result<Reply> {
        write(&mut self.input, &request).await?;
        let reply: Reply = read(&mut self.output).await.context("STT SSH connection closed (check service, credentials and host key)")?;
        if let Reply::Error(error) = &reply { anyhow::bail!("{error}") }
        Ok(reply)
    }
}
struct Recording { outcome: Arc<std::sync::Mutex<Outcome>>, id: String, cancel: CancellationToken, state: Arc<Mutex<Option<Stream>>> }
#[derive(Default)]
pub(crate) struct Connection { recording: Option<Recording> }
impl Drop for Connection {
    fn drop(&mut self) { if let Some(recording) = &self.recording { recording.cancel.cancel(); } }
}
impl Connection {
    pub fn command(&mut self, relay: Arc<Relay>, command: VoiceCommand, reply: ractor::RpcReplyPort<Result<VoiceReply, String>>) {
        if relay.stop.is_cancelled() { let _ = reply.send(Err("STT relay is shutting down".into())); return; }
        let id = match &command { VoiceCommand::Start {id,..} | VoiceCommand::Audio {id,..} | VoiceCommand::Finish {id} | VoiceCommand::Cancel {id} => id };
        if uuid::Uuid::parse_str(id).is_err() { let _ = reply.send(Err("Invalid recording ID".into())); return; }
        if matches!(command, VoiceCommand::Start { .. }) {
            if let Some(old) = self.recording.take() { old.cancel.cancel(); }
            self.recording = Some(Recording { outcome: Arc::new(std::sync::Mutex::new(Outcome::default())), id: id.clone(), cancel: relay.stop.child_token(), state: Arc::new(Mutex::new(None)) });
        }
        let Some(recording) = &self.recording else { let _ = reply.send(Err("No active recording".into())); return; };
        if recording.id != *id { let _ = reply.send(Err("Recording is no longer active".into())); return; }
        if matches!(command, VoiceCommand::Cancel { .. }) {
            {
                let mut outcome = recording.outcome.lock().unwrap();
                if outcome.status == "recording" { outcome.status = "cancelled".into(); }
            }
            recording.cancel.cancel();
            self.recording = None;
            let _ = reply.send(Ok(VoiceReply::Ack));
            return;
        }
        let outcome = recording.outcome.clone();
        let state = recording.state.clone();
        let cancel = recording.cancel.clone();
        if matches!(command, VoiceCommand::Start { .. }) {
            let expires = cancel.clone();
            let state = state.clone();
            tokio::spawn(async move {
                tokio::select! { _ = expires.cancelled() => {}, _ = tokio::time::sleep(Duration::from_secs(360)) => expires.cancel() }
                state.lock().await.take();
            });
        }
        let Ok(mut state) = state.try_lock_owned() else { let _ = reply.send(Err("Wait for the preceding audio chunk".into())); return; };
        tokio::spawn(async move {
            let duration = if matches!(command, VoiceCommand::Finish { .. }) { 180 } else { 15 };
            let result = tokio::select! {
                _ = cancel.cancelled() => Err(anyhow::anyhow!("Recording cancelled")),
                result = tokio::time::timeout(Duration::from_secs(duration), perform(&relay, &mut state, command, outcome.clone())) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("STT timed out"))),
            };
            if let Err(error) = &result {
                let mut outcome = outcome.lock().unwrap();
                if outcome.status == "recording" { outcome.status = if cancel.is_cancelled() {"interrupted"} else {"failed"}.into(); }
                outcome.error = Some(format!("{error:#}"));
            }
            if result.is_err() || matches!(result, Ok(VoiceReply::Text(_))) { state.take(); cancel.cancel(); }
            let _ = reply.send(result.map_err(|e| format!("{e:#}")));
        });
    }
}
async fn perform(relay: &Relay, state: &mut Option<Stream>, command: VoiceCommand, outcome: Arc<std::sync::Mutex<Outcome>>) -> Result<VoiceReply> {
    match command {
        VoiceCommand::Start { id, language, session } => {
            ensure!(demodex_stt::language_valid(&language), "Unsupported speech language");
            let config = relay.config.as_ref().context("Voice input is not configured on this server")?;
            let permit = relay.slots.clone().try_acquire_owned().context("STT relay busy")?;
            ensure!(!relay.stop.is_cancelled(), "STT relay is shutting down");
            let archive = relay.archives.begin(id, session, language.clone(), outcome.clone(), permit)?;
            let mut ssh = Command::new("ssh");
            ssh.args(["-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2", "-o", "ClearAllForwardings=yes"]);
            if let Some(path) = &config.identity_file { ssh.arg("-i").arg(path); }
            if let Some(path) = &config.known_hosts_file { ssh.arg("-o").arg(format!("UserKnownHostsFile={}", path.display())); }
            // Config paths are strictly validated. No browser value enters the remote command.
            ssh.arg(&config.destination).arg(format!("{} connect --socket {}", config.program.display(), config.socket.display()));
            let mut child = ssh.kill_on_drop(true).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().map_err(|error| {
                let mut outcome = outcome.lock().unwrap();
                outcome.status = "failed".into(); outcome.error = Some(error.to_string()); error
            })?;
            *state = Some(Stream { input: child.stdin.take().unwrap(), output: child.stdout.take().unwrap(), _child: child, archive, samples: 0, sequence: 0 });
            let stream = state.as_mut().unwrap();
            let Reply::Ready {model} = stream.exchange(Request::Start {version: demodex_stt::VERSION, language}).await? else { anyhow::bail!("Invalid STT startup response") };
            stream.archive.model(&model)?;
            Ok(VoiceReply::Ready {model})
        }
        VoiceCommand::Audio { sequence, pcm, .. } => {
            let stream = state.as_mut().context("Recording is not active")?;
            ensure!(sequence == stream.sequence && !pcm.is_empty() && pcm.len() <= demodex_stt::MAX_CHUNK, "Invalid audio sequence or size");
            ensure!(stream.samples + pcm.len() <= demodex_stt::MAX_SAMPLES, "Recording exceeds three minutes");
            stream.archive.append(&pcm)?;
            stream.samples += pcm.len(); stream.sequence += 1;
            ensure!(matches!(stream.exchange(Request::Audio { sequence, pcm }).await?, Reply::Ack), "Invalid STT acknowledgement");
            Ok(VoiceReply::Ack)
        }
        VoiceCommand::Finish { .. } => {
            let stream = state.as_mut().context("Recording is not active")?;
            let Reply::Text(text) = stream.exchange(Request::Finish).await? else { anyhow::bail!("Invalid STT transcript response") };
            ensure!(text.len() <= 32_768, "Transcript too large");
            stream.archive.completed(&text)?;
            Ok(VoiceReply::Text(text))
        }
        VoiceCommand::Cancel { .. } => unreachable!(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_command_cannot_contain_shell_input() {
        let mut c = Config { destination: "stt@stt-host".into(), socket: "/run/demodex-stt/stt.sock".into(), program: "/run/current-system/sw/bin/demodex-stt".into(), identity_file: None, known_hosts_file: None };
        c.validate().unwrap();
        c.destination = "-oProxyCommand=bad".into(); assert!(c.validate().is_err());
        c.destination = "stt-host".into(); c.socket = "/run/$(bad)".into(); assert!(c.validate().is_err());
    }
    #[tokio::test]
    async fn shutdown_cancels_idle_recordings_and_waits_for_slots() {
        let dir = tempfile::tempdir().unwrap();
        let relay = Relay::new(None, dir.path()).unwrap();
        let token = relay.stop.child_token();
        let permit = relay.slots.clone().acquire_owned().await.unwrap();
        let child = tokio::spawn(async move { token.cancelled().await; drop(permit); });
        tokio::time::timeout(Duration::from_secs(1), relay.shutdown()).await.unwrap();
        child.await.unwrap();
        assert!(relay.stop.is_cancelled());
    }

}
