//! Browser/daemon contract. Bump VERSION for semantic changes, including JSON payloads.
pub const VERSION: u32 = 1;
/// SHA-256 of normalized wire declarations and pinned transport dependencies.
pub const SCHEMA_HASH: &str = env!("DEMODEX_PROTOCOL_SCHEMA");
mod wire;
pub use wire::*;

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
                | Self::Detail { .. }
                | Self::Events { .. }
                | Self::Runtime
                | Self::SavedThreads { .. }
                | Self::Environments
                | Self::Receipt { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
