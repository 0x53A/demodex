use ractor::{ActorRef, RpcReplyPort};
use ractor_wormhole::WormholeTransmaterializable;
use serde::{Deserialize, Serialize};

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
        reply: RpcReplyPort<Result<String, String>>,
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

/// JSON is confined to extensible Codex records and existing validated form DTOs.
/// The transport itself is Wormhole: typed actor calls/reply ports and callbacks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, WormholeTransmaterializable)]
pub enum Operation {
    Sessions,
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
    HostSession {
        input: String,
    },
    ExternalSession {
        input: String,
    },
    Connect {
        id: String,
    },
    Sandbox {
        id: String,
        input: String,
    },
    Prompt {
        id: String,
        text: String,
    },
    Interrupt {
        id: String,
    },
    Answer {
        id: String,
        key: String,
        result: String,
    },
    Environments,
    CreateEnvironment {
        input: String,
    },
    StartEnvironment {
        id: String,
    },
    StopEnvironment {
        id: String,
    },
    EnvironmentSession {
        id: String,
        input: String,
    },
    Receipt {
        id: String,
    },
}
