use ractor::{ActorRef, RpcReplyPort};
use ractor_wormhole::WormholeTransmaterializable;
use serde::{Deserialize, Serialize};

/// Extensible upstream Codex data, kept intact across daemon and client versions.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct CodexRecord(pub serde_json::Value);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub thread_id: Option<String>,
    pub targets: Vec<Target>,
    pub status: String,
    pub error: Option<String>,
    pub archived: bool,
    pub sandbox: Option<Sandbox>,
    pub effective_sandbox: Option<serde_json::Value>,
    pub context_usage: serde_json::Value,
    pub presentation: Presentation,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Presentation {
    pub name: String,
    pub icon: String,
    pub context_reporting: bool,
    pub context: Option<UserVisibleContext>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UserVisibleContext {
    pub environment_id: String,
    pub path: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub at: String,
    pub message: serde_json::Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub key: String,
    pub method: String,
    pub params: serde_json::Value,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Environment {
    pub id: String,
    pub name: String,
    pub memory_mib: u32,
    pub cpus: u16,
    pub internet: bool,
    pub status: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionDetail {
    pub session: Session,
    pub pending: Vec<Pending>,
    pub queued: serde_json::Value,
    pub queue_error: serde_json::Value,
    pub controls: serde_json::Value,
    pub background: serde_json::Value,
    pub target_selection: Option<Vec<Selection>>,
    pub targets_pending: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Response {
    Sessions(Vec<Session>),
    Session(Session),
    Detail(SessionDetail),
    Events(Vec<Event>),
    Environments(Vec<Environment>),
    Environment(Environment),
    /// Codex runtime/catalog/command records remain extensible.
    Record(CodexRecord),
}

// Serialize only the data records through serde; actor references and reply ports
// still use Wormhole's context-aware transport. No JSON strings in the public API.
macro_rules! serde_wire {
    ($ty:ty) => {
        #[async_trait::async_trait]
        impl ractor_wormhole::transmaterialization::ContextTransmaterializable for $ty {
            async fn immaterialize(
                self,
                _: &ractor_wormhole::transmaterialization::TransmaterializationContext,
            ) -> ractor_wormhole::transmaterialization::TransmaterializationResult<Vec<u8>> {
                Ok(serde_json::to_vec(&self)?)
            }
            async fn rematerialize(
                _: &ractor_wormhole::transmaterialization::TransmaterializationContext,
                data: &[u8],
            ) -> ractor_wormhole::transmaterialization::TransmaterializationResult<Self> {
                Ok(serde_json::from_slice(data)?)
            }
        }
    };
}
serde_wire!(CodexRecord);
serde_wire!(Response);

impl Response {
    /// Presentation adapter for dynamic renderers and persisted legacy receipts.
    pub fn into_value(self) -> serde_json::Value {
        match self {
            Self::Sessions(value) => serde_json::json!(value),
            Self::Session(value) => serde_json::json!(value),
            Self::Detail(value) => serde_json::json!(value),
            Self::Events(value) => serde_json::json!(value),
            Self::Environments(value) => serde_json::json!(value),
            Self::Environment(value) => serde_json::json!(value),
            Self::Record(value) => value.0,
        }
    }
    pub fn from_value(
        operation: &Operation,
        value: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        Ok(match operation {
            Operation::Sessions => Self::Sessions(serde_json::from_value(value)?),
            Operation::CreateSession { .. }
            | Operation::ExternalSession { .. }
            | Operation::HostSession { .. }
            | Operation::EnvironmentSession { .. } => Self::Session(serde_json::from_value(value)?),
            Operation::Detail { .. } => Self::Detail(serde_json::from_value(value)?),
            Operation::Events { .. } => Self::Events(serde_json::from_value(value)?),
            Operation::Environments => Self::Environments(serde_json::from_value(value)?),
            Operation::CreateEnvironment { .. } => {
                Self::Environment(serde_json::from_value(value)?)
            }
            _ => Self::Record(CodexRecord(value)),
        })
    }
}

#[derive(Debug, WormholeTransmaterializable)]
pub enum Login {
    Authenticate {
        version: u32,
        token: String,
        reply: RpcReplyPort<Result<ActorRef<Api>, String>>,
    },
}

#[derive(Debug, WormholeTransmaterializable)]
pub enum Api {
    Call {
        request_id: String,
        operation: Operation,
        reply: RpcReplyPort<Result<Response, String>>,
    },
    Watch {
        sink: ActorRef<Notice>,
        reply: RpcReplyPort<()>,
    },
}

#[derive(Debug, WormholeTransmaterializable)]
pub enum Notice {
    Changed,
}

/// Commands use shared domain types; JSON is confined to extensible Codex records.
/// The transport itself is Wormhole: typed actor calls/reply ports and callbacks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
pub enum Operation {
    Sessions,
    StopBackground {
        id: String,
        generation: String,
        processes: Vec<(String, String)>,
    },
    Detail {
        id: String,
    },
    Events {
        id: String,
        after: i64,
    },
    Runtime,
    StartRuntime,
    Login,
    SavedThreads {
        cursor: Option<String>,
        search: String,
    },
    CreateSession {
        input: SelectedSession,
    },
    HostSession {
        input: HostSession,
    },
    ExternalSession {
        input: NewSession,
    },
    Connect {
        id: String,
    },
    Archive {
        id: String,
        archived: bool,
    },
    Sandbox {
        id: String,
        input: SandboxChoice,
    },
    Models {
        id: String,
    },
    Model {
        id: String,
        input: ModelChoice,
    },
    Goal {
        id: String,
        input: GoalAction,
    },
    Prompt {
        id: String,
        text: String,
    },
    QueuePrompt {
        id: String,
        text: String,
    },
    CancelQueued {
        id: String,
        queued_id: String,
    },
    ResumeQueue {
        id: String,
    },
    UploadImage {
        id: String,
        bytes: Vec<u8>,
    },
    Interrupt {
        id: String,
    },
    Answer {
        id: String,
        key: String,
        result: CodexRecord,
    },
    Environments,
    Targets,
    RegisterTarget {
        input: RegisterTarget,
    },
    RegisterSshTarget {
        input: SshTarget,
    },
    CheckSshTarget {
        id: String,
    },
    ReconnectSshTarget {
        id: String,
    },
    ForgetTarget {
        id: String,
    },
    SelectTargets {
        id: String,
        input: SelectTargets,
    },
    CreateEnvironment {
        input: NewEnvironment,
    },
    StartEnvironment {
        id: String,
    },
    StopEnvironment {
        id: String,
    },
    EnvironmentSession {
        id: String,
        input: SessionName,
    },
    Receipt {
        id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(default)]
#[derive(Default)]
pub struct NewSession {
    pub name: String,
    pub endpoint: String,
    pub targets: Vec<Target>,
    pub thread_id: Option<String>,
    pub sandbox: Option<Sandbox>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields)]
pub struct RegisterTarget {
    pub name: String,
    pub url: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields)]
pub struct SelectTargets {
    pub targets: Vec<Selection>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
pub struct SandboxChoice {
    pub sandbox: Option<Sandbox>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields)]
pub struct SelectedSession {
    pub name: String,
    pub targets: Vec<Selection>,
    pub sandbox: Option<Sandbox>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
pub struct HostSession {
    pub name: String,
    pub thread_id: Option<String>,
    pub sandbox: Option<Sandbox>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(default)]
#[derive(Default)]
pub struct NewEnvironment {
    pub name: String,
    pub memory_mib: u32,
    pub cpus: u16,
    pub internet: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
pub struct SessionName {
    pub name: String,
    pub sandbox: Option<Sandbox>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
pub struct Target {
    pub id: String,
    pub url: String,
    pub cwd: String,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, WormholeTransmaterializable,
)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub id: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelChoice {
    pub model: String,
    pub effort: String,
    pub service_tier: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GoalAction {
    pub action: String,
    pub objective: Option<String>,
    pub token_budget: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
#[serde(deny_unknown_fields)]
pub struct SshTarget {
    pub name: String,
    pub destination: String,
    pub cwd: String,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub known_hosts_file: Option<String>,
}
