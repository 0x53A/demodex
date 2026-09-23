//! SFTP v3 over the server's existing SSH subsystem; no remote helper executable.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const LIMIT: usize = 32 * 1024 * 1024;
pub struct Sftp {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::process::ChildStdout,
    serial: u32,
}
struct Packet {
    bytes: Vec<u8>,
    at: usize,
}
impl Packet {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(
            n <= self.bytes.len().saturating_sub(self.at),
            "Truncated SFTP packet"
        );
        let start = self.at;
        self.at += n;
        Ok(&self.bytes[start..self.at])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn data(&mut self) -> Result<Vec<u8>> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }
    fn text(&mut self) -> Result<String> {
        Ok(String::from_utf8(self.data()?)?)
    }
    fn attrs(&mut self) -> Result<Value> {
        let flags = self.u32()?;
        ensure!(flags & !0x8000000f == 0, "Unsupported SFTP attributes");
        let size = if flags & 1 != 0 {
            Some(self.u64()?)
        } else {
            None
        };
        if flags & 2 != 0 {
            self.take(8)?;
        }
        let mode = if flags & 4 != 0 { self.u32()? } else { 0 };
        let modified = if flags & 8 != 0 {
            self.u32()?;
            Some(self.u32()? as u64 * 1000)
        } else {
            None
        };
        if flags & 0x80000000 != 0 {
            let n = self.u32()?;
            ensure!(n < 1024, "Too many SFTP attributes");
            for _ in 0..n {
                self.data()?;
                self.data()?;
            }
        }
        Ok(
            json!({"isDirectory":mode&0o170000==0o040000,"isFile":mode&0o170000==0o100000,"isSymlink":mode&0o170000==0o120000,"size":size.unwrap_or(0),"createdAtMs":null,"modifiedAtMs":modified}),
        )
    }
}
fn data(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
fn path_arg(path: &str) -> Vec<u8> {
    let mut b = Vec::new();
    data(&mut b, path.as_bytes());
    b
}
impl Sftp {
    pub async fn connect(config: &Config) -> Result<Self> {
        let mut command = config.transport();
        command
            .arg("-s")
            .arg("--")
            .arg(&config.destination)
            .arg("sftp");
        // Protocol errors remain explicit; never invoke a shell fallback for file mutations.
        command.stderr(Stdio::null());
        let mut child = command.spawn()?;
        let input = child.stdin.take().context("SFTP stdin missing")?;
        let output = child.stdout.take().context("SFTP stdout missing")?;
        let mut s = Self {
            child,
            input,
            output,
            serial: 0,
        };
        s.send(1, &3u32.to_be_bytes()).await?;
        let (kind, mut packet) = s.receive().await?;
        ensure!(
            kind == 2 && packet.u32()? == 3,
            "SSH target must provide SFTP v3"
        );
        Ok(s)
    }
    async fn send(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        self.input.write_u32((payload.len() + 1) as u32).await?;
        self.input.write_u8(kind).await?;
        self.input.write_all(payload).await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<(u8, Packet)> {
        let n = self
            .output
            .read_u32()
            .await
            .context("SFTP subsystem unavailable or disconnected")? as usize;
        ensure!((1..=1024 * 1024).contains(&n), "Invalid SFTP packet size");
        let kind = self.output.read_u8().await?;
        let mut bytes = vec![0; n - 1];
        self.output.read_exact(&mut bytes).await?;
        Ok((kind, Packet { bytes, at: 0 }))
    }
    async fn request(&mut self, kind: u8, args: Vec<u8>, expected: u8) -> Result<Option<Packet>> {
        self.serial = self.serial.checked_add(1).context("SFTP request limit")?;
        let mut p = self.serial.to_be_bytes().to_vec();
        p.extend(args);
        self.send(kind, &p).await?;
        let (response, mut p) = self.receive().await?;
        ensure!(p.u32()? == self.serial, "Mismatched SFTP response");
        if response == 101 {
            let status = p.u32()?;
            let message = p.text()?;
            if status == 1 {
                return Ok(None);
            }
            if status != 0 {
                return Err(RpcFailure {
                    code: if status == 2 { -32004 } else { -32603 },
                    message: format!("SFTP: {message} (status {status})"),
                }
                .into());
            }
            ensure!(expected == 101, "Unexpected SFTP success packet");
            return Ok(Some(p));
        }
        ensure!(response == expected, "Unexpected SFTP response {response}");
        Ok(Some(p))
    }
    async fn required(&mut self, kind: u8, args: Vec<u8>, expected: u8) -> Result<Packet> {
        self.request(kind, args, expected)
            .await?
            .context("Unexpected SFTP EOF")
    }
    pub async fn stat(&mut self, path: &str) -> Result<Value> {
        self.required(17, path_arg(path), 105).await?.attrs()
    }
    pub async fn realpath(&mut self, path: &str) -> Result<String> {
        let mut p = self.required(16, path_arg(path), 104).await?;
        ensure!(p.u32()? == 1, "Invalid SFTP realpath result");
        p.text()
    }
    pub async fn mkdir(&mut self, path: &str, mode: u32) -> Result<()> {
        let mut p = path_arg(path);
        p.extend(4u32.to_be_bytes());
        p.extend(mode.to_be_bytes());
        self.required(14, p, 101).await?;
        Ok(())
    }
    async fn open(&mut self, path: &str, flags: u32) -> Result<Vec<u8>> {
        let mut p = path_arg(path);
        p.extend(flags.to_be_bytes());
        p.extend(4u32.to_be_bytes());
        p.extend(0o600u32.to_be_bytes());
        self.required(3, p, 102).await?.data()
    }
    async fn close(&mut self, handle: &[u8]) -> Result<()> {
        let mut p = vec![];
        data(&mut p, handle);
        self.required(4, p, 101).await?;
        Ok(())
    }
    pub async fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        ensure!(
            self.stat(path).await?["isFile"] == true,
            "Only regular files can be read"
        );
        let handle = self.open(path, 1).await?;
        let mut result = Vec::new();
        loop {
            let mut p = vec![];
            data(&mut p, &handle);
            p.extend((result.len() as u64).to_be_bytes());
            p.extend(32768u32.to_be_bytes());
            let Some(mut p) = self.request(5, p, 103).await? else {
                break;
            };
            let bytes = p.data()?;
            ensure!(!bytes.is_empty(), "Empty SFTP data packet");
            ensure!(
                result.len() + bytes.len() <= LIMIT,
                "File exceeds 32 MiB; streaming handles are unsupported"
            );
            result.extend(bytes);
        }
        self.close(&handle).await?;
        Ok(result)
    }
    pub async fn write(&mut self, path: &str, bytes: &[u8], exclusive: bool) -> Result<()> {
        ensure!(bytes.len() <= LIMIT, "File exceeds 32 MiB");
        let handle = self
            .open(path, 2 | 8 | if exclusive { 32 } else { 16 })
            .await?;
        for (index, chunk) in bytes.chunks(32768).enumerate() {
            let mut p = vec![];
            data(&mut p, &handle);
            p.extend(((index * 32768) as u64).to_be_bytes());
            data(&mut p, chunk);
            self.required(6, p, 101).await?;
        }
        self.close(&handle).await
    }
    async fn directory(&mut self, path: &str) -> Result<Value> {
        let handle = self.required(11, path_arg(path), 102).await?.data()?;
        let mut entries = vec![];
        loop {
            let mut p = vec![];
            data(&mut p, &handle);
            let Some(mut p) = self.request(12, p, 104).await? else {
                break;
            };
            let n = p.u32()?;
            ensure!(
                n > 0 && entries.len() + n as usize <= 100000,
                "Invalid or oversized SFTP directory listing"
            );
            for _ in 0..n {
                let name = p.text()?;
                p.data()?;
                let attrs = p.attrs()?;
                if name != "." && name != ".." {
                    entries.push(json!({"fileName":name,"isDirectory":attrs["isDirectory"],"isFile":attrs["isFile"]}));
                }
            }
        }
        self.close(&handle).await?;
        Ok(json!({"entries":entries}))
    }
    pub async fn call(&mut self, method: &str, p: &Value) -> Result<Value> {
        use base64::Engine;
        ensure!(
            p["followSymlinks"] != false,
            "SFTP does not support the requested no-follow guarantee"
        );
        let path = uri_path(string(p, "path")?)?;
        match method {
            "fs/readFile" => Ok(
                json!({"dataBase64":base64::engine::general_purpose::STANDARD.encode(self.read(&path).await?)}),
            ),
            "fs/writeFile" => {
                self.write(
                    &path,
                    &base64::engine::general_purpose::STANDARD.decode(string(p, "dataBase64")?)?,
                    false,
                )
                .await?;
                Ok(json!({}))
            }
            "fs/getMetadata" => self.stat(&path).await,
            "fs/canonicalize" => Ok(json!({"path":path_uri(&self.realpath(&path).await?)})),
            "fs/readDirectory" => self.directory(&path).await,
            "fs/createDirectory" => {
                ensure!(
                    p["recursive"] != true,
                    "Recursive directory creation is unsupported; use a foreground command"
                );
                self.mkdir(&path, 0o700).await?;
                Ok(json!({}))
            }
            "fs/remove" => {
                ensure!(
                    p["recursive"] != true,
                    "Recursive removal is unsupported; use a foreground command"
                );
                let result = async {
                    let meta = self.required(7, path_arg(&path), 105).await?.attrs()?;
                    self.required(
                        if meta["isDirectory"] == true { 15 } else { 13 },
                        path_arg(&path),
                        101,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(json!({}))
                }
                .await;
                match result {
                    Err(e)
                        if p["force"] == true
                            && e.downcast_ref::<RpcFailure>()
                                .is_some_and(|e| e.code == -32004) =>
                    {
                        Ok(json!({}))
                    }
                    other => other,
                }
            }
            _ => unsupported(method),
        }
    }
}
impl Drop for Sftp {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
pub fn uri_path(uri: &str) -> Result<String> {
    let path = uri.strip_prefix("file://").context("Expected file URI")?;
    let path = path
        .strip_prefix("localhost/")
        .map(|p| format!("/{p}"))
        .unwrap_or_else(|| path.into());
    ensure!(
        path.starts_with('/') && !path.contains(['?', '#']),
        "Expected absolute local file URI"
    );
    let mut bytes = vec![];
    let mut i = 0;
    let raw = path.as_bytes();
    while i < raw.len() {
        if raw[i] == b'%' {
            ensure!(i + 2 < raw.len(), "Invalid URI escape");
            bytes.push(u8::from_str_radix(
                std::str::from_utf8(&raw[i + 1..i + 3])?,
                16,
            )?);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(bytes)?;
    crate::targets::validate_cwd(&path)?;
    Ok(path)
}
pub fn path_uri(path: &str) -> String {
    let mut out = "file://".to_owned();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char)
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uri_roundtrip_and_malformed_packets_fail_closed() -> Result<()> {
        let path = "/workspace/quotes ' and $(literal)/ü #?%.txt";
        assert_eq!(uri_path(&path_uri(path))?, path);
        for path in [
            "file://otherhost/tmp/x",
            "file:///tmp/%00x",
            "file:///tmp/%GG",
            "file:///tmp/%",
            "https://example/x",
        ] {
            assert!(uri_path(path).is_err(), "{path}");
        }
        let mut packet = Packet {
            bytes: vec![0, 0, 0, 8, 1],
            at: 0,
        };
        assert!(packet.data().is_err());
        let mut packet = Packet {
            bytes: vec![0xff, 0xff, 0xff, 0xff],
            at: 0,
        };
        assert!(packet.attrs().is_err());
        Ok(())
    }
}
