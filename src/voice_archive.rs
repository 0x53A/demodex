//! Lossless per-daemon voice archives. Raw PCM is durable before forwarding;
//! unfinished spools survive crashes and are finalized on the next startup.
use anyhow::{Context, Result, ensure};
use flacenc::{component::BitRepr, error::Verify};
use serde::{Deserialize, Serialize};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, os::unix::fs::{DirBuilderExt, OpenOptionsExt}, path::{Path, PathBuf}, sync::{Arc, Mutex}};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::task::TaskTracker;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub status: String,
    pub transcript: Option<String>,
    pub error: Option<String>,
}
impl Default for Outcome {
    fn default() -> Self { Self { status: "recording".into(), transcript: None, error: None } }
}
#[derive(Debug, Serialize, Deserialize)]
struct Metadata {
    version: u32,
    recording_id: String,
    session_id: String,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    language: String,
    model: Option<String>,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    samples: u64,
    audio_stage: String,
    #[serde(flatten)]
    outcome: Outcome,
    archive_error: Option<String>,
}
fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
fn private_file(path: &Path, new: bool) -> Result<File> {
    Ok(OpenOptions::new().write(true).create(!new).create_new(new).truncate(!new).mode(0o600).open(path)?)
}
fn metadata_write(dir: &Path, metadata: &Metadata) -> Result<()> {
    let mut file = private_file(&dir.join("metadata.json.tmp"), false)?;
    file.write_all(&serde_json::to_vec_pretty(metadata)?)?;
    file.sync_all()?;
    fs::rename(dir.join("metadata.json.tmp"), dir.join("metadata.json"))?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
pub struct Archives { root: PathBuf, jobs: TaskTracker }
impl Archives {
    pub fn new(data_dir: &Path) -> Arc<Self> { Arc::new(Self { root: data_dir.join("voice-recordings"), jobs: TaskTracker::new() }) }
    pub fn recover(self: &Arc<Self>) {
        // Snapshot before accepting new recordings. Never race a fresh spool.
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => { tracing::error!(%error, "voice archive discovery failed"); return; }
        };
        let mut pending = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) if entry.file_type().is_ok_and(|kind|kind.is_dir())
                    && uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_ok()
                    && entry.path().join("audio.pcm").exists() => pending.push(entry.path()),
                Err(error) => tracing::error!(%error, "voice archive discovery failed"),
                _ => {}
            }
        }
        self.jobs.spawn_blocking(move || {
            for dir in pending {
                if let Err(error) = recover_one(&dir) { tracing::error!(path=%dir.display(), %error, "voice archive recovery failed; PCM retained"); }
            }
        });
    }
    pub async fn shutdown(&self) { self.jobs.close(); self.jobs.wait().await; }
    pub fn begin(self: &Arc<Self>, recording_id: String, session_id: String, language: String, outcome: Arc<Mutex<Outcome>>, permit: OwnedSemaphorePermit) -> Result<Archive> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.root)?;
        let dir = self.root.join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        File::open(&self.root)?.sync_all()?;
        let metadata = Metadata { version: 1, recording_id, session_id, language, started_at_ms: now(), finished_at_ms: None,
            model: None, sample_rate: 16000, channels: 1, bits_per_sample: 16, samples: 0,
            audio_stage: "stt-input: browser-processed/resampled PCM16 (not raw microphone audio)".into(),
            outcome: Outcome::default(), archive_error: None };
        metadata_write(&dir, &metadata)?;
        let file = private_file(&dir.join("audio.pcm"), true)?;
        File::open(&dir)?.sync_all()?;
        Ok(Archive { data: Some(Data { dir, metadata, file, outcome, _permit: Some(permit) }), jobs: self.jobs.clone() })
    }
}
struct Data { dir: PathBuf, metadata: Metadata, file: File, outcome: Arc<Mutex<Outcome>>, _permit: Option<OwnedSemaphorePermit> }
pub struct Archive { data: Option<Data>, jobs: TaskTracker }
impl Archive {
    pub fn append(&mut self, pcm: &[i16]) -> Result<()> {
        let data = self.data.as_mut().unwrap();
        let bytes: Vec<u8> = pcm.iter().flat_map(|v|v.to_le_bytes()).collect();
        data.file.write_all(&bytes).context("Cannot save voice recording")?;
        data.file.sync_data().context("Cannot sync voice recording")?;
        data.metadata.samples += pcm.len() as u64;
        Ok(())
    }
    pub fn model(&mut self, model: &str) -> Result<()> {
        let data = self.data.as_mut().unwrap();
        data.metadata.model = Some(model.to_owned());
        metadata_write(&data.dir, &data.metadata)
    }
    pub fn completed(&mut self, text: &str) -> Result<()> {
        let data = self.data.as_mut().unwrap();
        let mut outcome = data.outcome.lock().unwrap();
        if outcome.status == "recording" { outcome.status = "completed".into(); }
        outcome.transcript = Some(text.to_owned());
        data.metadata.outcome = outcome.clone();
        data.metadata.finished_at_ms = Some(now());
        metadata_write(&data.dir, &data.metadata).context("Cannot save voice transcript metadata")
    }
}
impl Drop for Archive {
    fn drop(&mut self) {
        let Some(mut data) = self.data.take() else { return; };
        self.jobs.spawn_blocking(move || {
            data.metadata.outcome = data.outcome.lock().unwrap().clone();
            if data.metadata.outcome.status == "recording" { data.metadata.outcome.status = "interrupted".into(); }
            data.metadata.finished_at_ms = Some(now());
            if let Err(error) = finalize(&data.dir, &mut data.metadata) {
                data.metadata.archive_error = Some(format!("{error:#}"));
                let _ = metadata_write(&data.dir, &data.metadata);
                tracing::error!(path=%data.dir.display(), %error, "voice FLAC archive failed; PCM retained");
            }
            // Includes the recording permit: bound encoding jobs and drain before shutdown.
            drop(data);
        });
    }
}
fn recover_one(dir: &Path) -> Result<()> {
    ensure!(fs::metadata(dir.join("metadata.json"))?.len() <= 128 * 1024, "Archive metadata exceeds limit");
    let mut metadata: Metadata = serde_json::from_slice(&fs::read(dir.join("metadata.json"))?)?;
    ensure!(metadata.version == 1 && metadata.sample_rate == 16000 && metadata.channels == 1 && metadata.bits_per_sample == 16, "Unsupported voice spool format");
    if metadata.outcome.status == "recording" { metadata.outcome.status = "interrupted".into(); }
    metadata.finished_at_ms.get_or_insert_with(now);
    finalize(dir, &mut metadata)
}
fn finalize(dir: &Path, metadata: &mut Metadata) -> Result<()> {
    let mut pcm = Vec::new();
    File::open(dir.join("audio.pcm"))?.take((demodex_stt::MAX_SAMPLES * 2 + 1) as u64).read_to_end(&mut pcm)?;
    ensure!(pcm.len() <= demodex_stt::MAX_SAMPLES * 2, "Voice spool exceeds recording limit");
    if pcm.len() % 2 != 0 {
        metadata.outcome.error = Some("Interrupted PCM write left an incomplete final sample; original spool retained".into());
    }
    let samples: Vec<i32> = pcm.chunks_exact(2).map(|b|i16::from_le_bytes([b[0],b[1]]) as i32).collect();
    metadata.samples = samples.len() as u64;
    // Save the terminal outcome before encoding; a crash retains transcript and PCM.
    metadata_write(dir, metadata)?;
    let config = flacenc::config::Encoder::default().into_verified().map_err(|_|anyhow::anyhow!("Invalid FLAC encoder configuration"))?;
    let source = flacenc::source::MemSource::from_samples(&samples, 1, 16, 16000);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).map_err(|e|anyhow::anyhow!("FLAC encode: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e|anyhow::anyhow!("FLAC stream: {e:?}"))?;
    let mut file = private_file(&dir.join("audio.flac.tmp"), false)?;
    file.write_all(sink.as_slice())?;
    file.sync_all()?;
    fs::rename(dir.join("audio.flac.tmp"), dir.join("audio.flac"))?;
    File::open(dir)?.sync_all()?;
    metadata.archive_error = None;
    metadata_write(dir, metadata)?;
    if pcm.len() % 2 == 0 { fs::remove_file(dir.join("audio.pcm"))?; File::open(dir)?.sync_all()?; }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_audio_roundtrips_losslessly_with_private_metadata() {
        let root = tempfile::tempdir().unwrap();
        let archives = Archives::new(root.path());
        let outcome = Arc::new(Mutex::new(Outcome::default()));
        let permit = Arc::new(tokio::sync::Semaphore::new(1)).acquire_owned().await.unwrap();
        let mut recording = archives.begin("browser-id".into(), "session-id".into(), "de".into(), outcome.clone(), permit).unwrap();
        let samples: Vec<i16> = (0..10_000).map(|x|(x as i16).wrapping_mul(123)).collect();
        recording.append(&samples[..8000]).unwrap(); recording.append(&samples[8000..]).unwrap();
        outcome.lock().unwrap().status = "cancelled".into();
        drop(recording); archives.shutdown().await;
        let dir = fs::read_dir(root.path().join("voice-recordings")).unwrap().next().unwrap().unwrap().path();
        let mut flac = claxon::FlacReader::open(dir.join("audio.flac")).unwrap();
        assert_eq!(flac.streaminfo().sample_rate, 16000);
        assert_eq!(flac.samples().map(Result::unwrap).collect::<Vec<_>>(), samples.iter().map(|x|*x as i32).collect::<Vec<_>>());
        let metadata: Metadata = serde_json::from_slice(&fs::read(dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata.outcome.status, "cancelled"); assert_eq!(metadata.samples, 10_000);
        assert!(!dir.join("audio.pcm").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(dir.join("audio.flac")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    #[tokio::test]
    async fn startup_recovers_pcm_and_preserves_original_transcript() {
        let root = tempfile::tempdir().unwrap();
        let archives = Archives::new(root.path());
        let permit = Arc::new(tokio::sync::Semaphore::new(1)).acquire_owned().await.unwrap();
        let mut recording = archives.begin("browser-id".into(), "session-id".into(), "en".into(), Arc::new(Mutex::new(Outcome::default())), permit).unwrap();
        recording.append(&[1, -2, 32767, -32768]).unwrap();
        recording.completed("original transcription").unwrap();
        let dir = recording.data.as_ref().unwrap().dir.clone();
        // Simulate exit before the Drop finalizer can run.
        drop(recording.data.take()); drop(recording);
        archives.recover();
        // A recording begun after startup discovery must not be recovered mid-capture.
        let permit = Arc::new(tokio::sync::Semaphore::new(1)).acquire_owned().await.unwrap();
        let live = archives.begin("next-id".into(), "session-id".into(), "en".into(), Arc::new(Mutex::new(Outcome::default())), permit).unwrap();
        let live_dir = live.data.as_ref().unwrap().dir.clone();
        archives.jobs.close();
        archives.jobs.wait().await;
        assert!(live_dir.join("audio.pcm").exists());
        assert!(!live_dir.join("audio.flac").exists());
        drop(live); archives.shutdown().await;
        let metadata: Metadata = serde_json::from_slice(&fs::read(dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata.outcome.transcript.as_deref(), Some("original transcription"));
        assert_eq!(metadata.outcome.status, "completed");
        assert!(dir.join("audio.flac").exists());
    }
    #[tokio::test]
    async fn failed_start_with_no_audio_still_has_valid_archive() {
        let root = tempfile::tempdir().unwrap();
        let archives = Archives::new(root.path());
        let outcome = Arc::new(Mutex::new(Outcome {status: "failed".into(), transcript: None, error: Some("STT unavailable".into())}));
        let permit = Arc::new(tokio::sync::Semaphore::new(1)).acquire_owned().await.unwrap();
        let recording = archives.begin("id".into(), "session".into(), "en".into(), outcome, permit).unwrap();
        let dir = recording.data.as_ref().unwrap().dir.clone();
        drop(recording); archives.shutdown().await;
        let mut flac = claxon::FlacReader::open(dir.join("audio.flac")).unwrap();
        assert_eq!(flac.samples().count(), 0);
        let metadata: Metadata = serde_json::from_slice(&fs::read(dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata.outcome.status, "failed");
    }

}
