//! Transient dialog drafts and the lifetime of requests submitted from them.
use super::*;

pub struct RequestOrigin {
    pub epoch: u64,
    pub ssh: Option<demodex_protocol::SshTarget>,
}

#[derive(PartialEq)]
pub(super) struct Dialogs {
    host: String,
    session: String,
    new_session: bool,
    search: bool,
    setup: String,
    controls: bool,
    server: bool,
    connections: bool,
    editor: bool,
    background: bool,
    diagnostics: bool,
}

impl App {
    pub(super) fn dialogs(&self) -> Dialogs {
        Dialogs {
            host: self.saved.host.clone(),
            session: self.saved.selected.clone(),
            new_session: self.show_new_session,
            search: self.show_saved_search,
            setup: self.new_target_setup.clone(),
            controls: self.show_controls,
            server: self.show_server_settings,
            connections: self.connections_page,
            editor: self.connection_editor,
            background: self.show_background,
            diagnostics: self.show_diagnostics,
        }
    }

    pub(super) fn clear_setup(&mut self, kind: &str) {
        self.saved.fields.retain(|key, _| !setup_field(kind, key));
    }

    pub(super) fn finish_dialogs(&mut self, previous: Dialogs) {
        if !self.show_new_session {
            self.show_saved_search = false;
        }
        if previous.host != self.saved.host {
            discard_saved_dialogs(&mut self.saved);
            self.show_new_session = false;
            self.show_saved_search = false;
            self.new_target_setup.clear();
            self.staged_ssh = None;
        }
        let next = self.dialogs();
        if previous == next {
            return;
        }
        self.dialog_epoch += 1;
        if previous.new_session && !next.new_session {
            self.share.creating = false;
            self.saved.fields.retain(|key, _| !creation_field(key));
            self.staged_ssh = None;
        }
        if previous.search != next.search || previous.new_session != next.new_session {
            self.saved_search_serial += 1;
            self.saved_search_loading = false;
            self.saved_threads.clear();
            self.cursor = None;
            self.saved.fields.remove("search");
        }
        if previous.setup != next.setup {
            self.clear_setup(&previous.setup);
            self.clear_setup(&next.setup);
        }
        if previous.controls && (!next.controls || previous.session != next.session) {
            let control = format!("control:{}:", previous.session);
            let rename = format!("rename:{}", previous.session);
            let targets = format!("target-draft:{}", previous.session);
            self.saved.fields.retain(|key, _| {
                !key.starts_with(&control)
                    && key != &rename
                    && key != &targets
                    && key != "session_sandbox"
            });
            self.model_serial += 1;
            self.models = Value::Null;
            self.model_error.clear();
        }
        // An outstanding operation/receipt survives dismissal; old validation
        // errors and notices do not become the next dialog's initial state.
        if self.receipt.is_empty() {
            self.error.clear();
            self.error_epoch = None;
        }
        self.target_notice.clear();
    }
}

fn creation_field(key: &str) -> bool {
    key.starts_with("new_")
        || matches!(
            key,
            "resume_session" | "session_name" | "thread_id" | "cwd" | "sandbox" | "search"
        )
}

fn setup_field(kind: &str, key: &str) -> bool {
    match kind {
        "vm" => matches!(key, "environment_name" | "memory" | "cpus" | "internet"),
        "container" => key.starts_with("container_"),
        "ssh" => key.starts_with("new_ssh"),
        "session-ssh" => key.starts_with("ssh_"),
        "shared-ssh" => key.starts_with("shared_ssh_"),
        "external" => matches!(key, "target_name" | "target_url" | "target_cwd"),
        _ => false,
    }
}

// The same legacy map also contains preferences such as the tree/list layout.
// Remove only dialog fields, preserving those preferences on reload/host switch.
pub(super) fn discard_saved_dialogs(saved: &mut Saved) {
    let retain = |key: &String, _: &mut String| {
        !creation_field(key)
            && ![
                "vm",
                "container",
                "ssh",
                "session-ssh",
                "shared-ssh",
                "external",
            ]
            .iter()
            .any(|kind| setup_field(kind, key))
            && !key.starts_with("control:")
            && !key.starts_with("rename:")
            && !key.starts_with("target-draft:")
            && key != "session_sandbox"
    };
    saved.fields.retain(retain);
    for fields in saved.host_fields.values_mut() {
        fields.retain(retain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discarding_forms_preserves_preferences_conversation_and_receipts() {
        let mut saved: Saved = serde_json::from_value(json!({
            "fields": {"session_view":"flat", "new_session_name":"draft", "control:s:objective":"draft", "ssh_destination":"failed"},
            "host_fields": {"other": {"session_view":"tree", "container_image":"draft"}},
            "drafts": {"host:s":"unsent"}, "answers": {"question":"answer"}, "receipts": {"host":"pending"}
        })).unwrap();
        discard_saved_dialogs(&mut saved);
        assert_eq!(
            saved.fields,
            [("session_view".into(), "flat".into())].into()
        );
        assert_eq!(
            saved.host_fields["other"],
            [("session_view".into(), "tree".into())].into()
        );
        assert_eq!(saved.drafts["host:s"], "unsent");
        assert_eq!(saved.answers["question"], "answer");
        assert_eq!(saved.receipts["host"], "pending");
    }
}
