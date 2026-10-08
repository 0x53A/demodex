//! Browser/daemon contract. Bump VERSION for semantic changes, including JSON payloads.
pub const VERSION: u32 = 37;
/// SHA-256 of normalized wire declarations and pinned transport dependencies.
pub const SCHEMA_HASH: &str = env!("DEMODEX_PROTOCOL_SCHEMA");
pub mod links;
pub mod transcript;
pub mod location;
mod wire;
pub use wire::*;

/// Recognize only Codex's explicit writer conflict for this exact thread.
pub fn active_writer_conflict(error: &str, thread: &str) -> bool {
    if thread.is_empty() { return false; }
    error.strip_prefix("Codex: ")
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .is_some_and(|reply| reply["code"] == -32600
            && reply["message"] == format!("thread {thread} already has an active writer"))
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub protocol: String,
    pub version: u32,
    pub schema: String,
}

impl Default for Hello {
    fn default() -> Self {
        Self {
            protocol: "demodex".into(),
            version: VERSION,
            schema: SCHEMA_HASH.into(),
        }
    }
}

impl Hello {
    /// This tiny JSON envelope precedes authentication and all Wormhole traffic.
    pub fn check(&self) -> Result<(), String> {
        if self.protocol != "demodex" || self.version != VERSION || self.schema != SCHEMA_HASH {
            return Err(format!(
                "Protocol mismatch: local v{} / {}, peer {} v{} / {}. Update the browser or daemon.",
                VERSION, SCHEMA_HASH, self.protocol, self.version, self.schema
            ));
        }
        Ok(())
    }
}

impl Operation {
    pub fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Self::Sessions
                | Self::PushSettings { .. }
                | Self::Detail { .. }
                | Self::Models { .. }
                | Self::Events { .. }
                | Self::Conversation { .. }
                | Self::Runtime
                | Self::RuntimeModels
                | Self::DefaultPrompt
                | Self::PromptSettings
                | Self::InstructionFiles { .. }
                | Self::SessionPromptSettings { .. }
                | Self::SavedThreads { .. }
                | Self::Environments
                | Self::BrowseDirectories { .. }
                | Self::MessageFiles { .. }
                | Self::ReadMessageFile { .. }
                | Self::ReadUploadedImage { .. }
                | Self::Targets
                | Self::Receipt { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writer_conflict_requires_exact_error_and_thread() {
        let error = r#"Codex: {"code":-32600,"message":"thread abc already has an active writer"}"#;
        assert!(active_writer_conflict(error, "abc"));
        assert!(!active_writer_conflict(error, "other"));
        assert!(!active_writer_conflict(error, ""));
        assert!(!active_writer_conflict(&error.replace("-32600", "-32603"), "abc"));
        assert!(!active_writer_conflict("connection closed", "abc"));
        assert!(!active_writer_conflict(&format!("transport: {error}"), "abc"));
        assert!(Operation::Takeover { id:"s".into(), expected_daemon:"d".into(), expected_threads:vec![] }.is_mutation());
    }
    #[test]
    fn early_hello_requires_both_version_and_schema() {
        assert!(Hello::default().check().is_ok());
        assert_eq!(SCHEMA_HASH.len(), 64);
        let mut hello = Hello::default();
        hello.version += 1;
        assert!(hello.check().is_err());
        hello = Hello::default();
        hello.schema = "different".into();
        assert!(hello.check().is_err());
        hello = Hello::default();
        hello.protocol = "different".into();
        assert!(hello.check().is_err());
    }
}
