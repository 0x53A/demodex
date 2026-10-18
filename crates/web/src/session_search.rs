use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct State {
    pub query: String,
    pub serial: u64,
    pub full: bool,
    pub loading: bool,
    pub matches: BTreeSet<String>,
    pub pending: Vec<String>,
    pub total: usize,
    pub errors: Vec<String>,
}

impl State {
    pub fn includes(&self, session: &Value) -> bool {
        crate::overview::matches_search(session, &self.query.trim().to_lowercase())
            || (self.full && self.matches.contains(text(session, "id")))
    }
    pub fn reset(&mut self, query: String) {
        let serial = self.serial + 1;
        *self = Self { query, serial, ..Self::default() };
    }
}

impl App {
    pub(super) fn search_next(&mut self, ctx: &Context<Self>, older: Option<demodex_protocol::ConversationPageCursor>) {
        let Some(id) = self.search.pending.last().cloned() else {
            self.search.loading = false;
            return;
        };
        let Some(client) = self.client.clone().filter(|_| self.connected) else {
            self.search.loading = false;
            self.search.errors.push("Connection lost; search is incomplete. Run Full text search again to retry.".into());
            return;
        };
        let generation = self.generation;
        let serial = self.search.serial;
        let query = self.search.query.clone();
        ctx.link().send_future(async move {
            let result = client.read(Operation::SearchConversation { id: id.clone(), query, older }).await.map_err(|e| format!("{e:#}"));
            Msg::SearchPage(generation, serial, id, result)
        });
    }

    pub(super) fn search_footer(&self, ctx: &Context<Self>) -> Html {
        if self.search.query.trim().is_empty() { return Html::default(); }
        let count = self.sessions.iter().filter(|s| self.search.includes(s)).count();
        html! {<div class="session-search-footer">
            <p role="status">{if self.search.loading {
                format!("Searching saved conversations… {}/{} checked · {count} matches",self.search.total-self.search.pending.len(),self.search.total)
            } else if self.search.full {
                format!("{count} matches · {}", if self.search.errors.is_empty() {"Full text search complete"} else {"Search incomplete"})
            } else { format!("{count} matches in titles and working folders") }}</p>
            {for self.search.errors.iter().map(|error| html!{<p class="error" role="alert">{error}</p>})}
            <button type="button" disabled={!self.connected||self.search.loading} onclick={ctx.link().callback(|_|Msg::SearchStart)}>{"Full text search"}</button>
            {if self.search.loading {html!{<button type="button" onclick={ctx.link().callback(|_|Msg::SearchStop)}>{"Stop search"}</button>}}else{Html::default()}}
            <small class="muted">{"Search saved messages and tool activity on this server, including archived sessions."}</small>
        </div>}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_query_invalidates_full_text_results() {
        let mut state = State::default();
        state.reset("  TITLE  ".into());
        let session = json!({"id":"s", "name":"Title"});
        assert!(state.includes(&session));
        state.full = true;
        state.loading = true;
        state.matches.insert("s".into());
        let serial = state.serial;
        state.reset("missing".into());
        assert!(state.serial > serial);
        assert!(!state.includes(&session));
        assert!(!state.loading);
        assert!(!state.full);
    }
}
