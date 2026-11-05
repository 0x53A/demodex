//! Private STT protocol. Audio is 16 kHz mono signed PCM; nothing is persisted.
use anyhow::{Result, ensure};
use serde::{Serialize, Deserialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub const VERSION: u32 = 1;
pub const MAX_SAMPLES: usize = 16_000 * 180;
pub const MAX_CHUNK: usize = 8_000;
pub const MAX_FRAME: usize = 128 * 1024;
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Start { version: u32, language: String },
    Audio { sequence: u32, pcm: Vec<i16> },
    Finish,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Reply { Ready { model: String }, Ack, Text(String), Error(String) }
pub async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(io: &mut R) -> Result<T> {
    let length = io.read_u32().await? as usize;
    ensure!(length <= MAX_FRAME, "STT frame too large");
    let mut data = vec![0; length];
    io.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}
pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(io: &mut W, value: &T) -> Result<()> {
    let data = serde_json::to_vec(value)?;
    ensure!(data.len() <= MAX_FRAME, "STT frame too large");
    io.write_u32(data.len() as u32).await?;
    io.write_all(&data).await?;
    io.flush().await?;
    Ok(())
}
#[derive(Default)]
pub struct Audio {
    pub pcm: Vec<f32>,
    next: u32,
}
impl Audio {
    pub fn push(&mut self, sequence: u32, pcm: &[i16]) -> Result<()> {
        ensure!(sequence == self.next, "Audio chunk out of order; recording cancelled");
        ensure!(!pcm.is_empty() && pcm.len() <= MAX_CHUNK, "Invalid audio chunk size");
        ensure!(self.pcm.len() + pcm.len() <= MAX_SAMPLES, "Recording exceeds three minutes");
        self.pcm.extend(pcm.iter().map(|v| *v as f32 / 32768.0));
        self.next += 1;
        Ok(())
    }
}
pub fn language_valid(language: &str) -> bool {
    ["en","de","fr","es","it","pt","nl","pl","el","ar","ja","zh","vi","ko"].contains(&language)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_and_bounds() {
        let mut a = Audio::default();
        assert!(a.push(1, &[1]).is_err());
        assert!(a.push(0, &[]).is_err());
        assert!(a.push(0, &vec![0; MAX_CHUNK + 1]).is_err());
        a.push(0, &[i16::MIN, i16::MAX]).unwrap();
        assert_eq!(a.pcm[0], -1.0);
        assert!(a.push(0, &[1]).is_err());
        a.pcm.resize(MAX_SAMPLES, 0.0);
        assert!(a.push(1, &[1]).is_err());
    }
    #[tokio::test]
    async fn frame_limit_before_allocation() {
        let (mut tx, mut rx) = tokio::io::duplex(8);
        tx.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(read::<_, Request>(&mut rx).await.is_err());
    }
}
