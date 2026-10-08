//! Bounded invalidation queues and independent reads. Never queue mutations here.
use demodex_protocol::Notice;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Notices {
    full: bool,
    runtime: bool,
    sessions: BTreeMap<String, bool>,
}
impl Notices {
    pub fn push(&mut self, notice: Notice) {
        if self.full {
            return;
        }
        match notice {
            Notice::Changed => {
                self.full = true;
                self.sessions.clear();
                self.runtime = false;
            }
            Notice::Runtime => self.runtime = true,
            Notice::Session { id, state } => {
                *self.sessions.entry(id).or_default() |= state;
                if self.sessions.len() > 64 {
                    self.push(Notice::Changed);
                }
            }
        }
    }
    pub fn take(&mut self) -> Vec<Notice> {
        let old = std::mem::take(self);
        if old.full {
            return vec![Notice::Changed];
        }
        let mut result = Vec::new();
        if old.runtime {
            result.push(Notice::Runtime);
        }
        result.extend(
            old.sessions
                .into_iter()
                .map(|(id, state)| Notice::Session { id, state }),
        );
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Sessions,
    Runtime,
    Environments,
    Targets,
    ProjectGit,
    Detail,
    Events,
}
pub const RESOURCES: [Resource; 7] = [
    Resource::Events,
    Resource::Detail,
    Resource::Sessions,
    Resource::Runtime,
    Resource::Environments,
    Resource::Targets,
    Resource::ProjectGit,
];
impl Resource {
    pub fn session(self) -> bool {
        matches!(self, Self::Detail | Self::Events)
    }
}
#[derive(Default)]
struct Slot {
    dirty: bool,
    failed: bool,
    flight: Option<u64>,
}
#[derive(Default)]
pub struct Reads {
    slots: [Slot; 7],
    serial: u64,
}
impl Reads {
    pub fn invalidate(&mut self, resource: Resource) {
        self.slots[resource as usize].dirty = true;
    }
    pub fn notice(&mut self, notice: Notice, selected: &str) {
        match notice {
            Notice::Changed => {
                for resource in RESOURCES {
                    self.invalidate(resource);
                }
            }
            Notice::Runtime => {
                self.invalidate(Resource::Runtime);
                self.invalidate(Resource::ProjectGit);
                self.invalidate(Resource::Detail);
                // Retry failed reads on a heartbeat, never in a tight loop.
                for resource in RESOURCES {
                    if self.slots[resource as usize].failed {
                        self.invalidate(resource);
                    }
                }
            }
            Notice::Session { id, state } => {
                if state {
                    self.invalidate(Resource::Sessions);
                    self.invalidate(Resource::ProjectGit);
                }
                if id == selected {
                    self.invalidate(Resource::Events);
                    if state {
                        self.invalidate(Resource::Detail);
                    }
                }
            }
        }
    }
    pub fn reset(&mut self, session_only: bool) {
        for resource in RESOURCES {
            if !session_only || resource.session() {
                self.slots[resource as usize] = Slot::default();
            }
        }
    }
    pub fn start(&mut self, resource: Resource, selected: &str) -> Option<u64> {
        let slot = &mut self.slots[resource as usize];
        if resource.session() && selected.is_empty() {
            slot.dirty = false;
            return None;
        }
        if !slot.dirty || slot.flight.is_some() {
            return None;
        }
        slot.dirty = false;
        slot.failed = false;
        self.serial += 1;
        slot.flight = Some(self.serial);
        Some(self.serial)
    }
    pub fn finish(&mut self, resource: Resource, ticket: u64) -> bool {
        let slot = &mut self.slots[resource as usize];
        if slot.flight != Some(ticket) {
            return false;
        }
        slot.flight = None;
        true
    }
    pub fn failed(&mut self, resource: Resource) {
        self.slots[resource as usize].failed = true;
    }
    pub fn busy(&self) -> bool {
        self.slots.iter().any(|s| s.flight.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalescing_preserves_every_scope_and_recovers_after_overflow() {
        let mut notices = Notices::default();
        notices.push(Notice::Session {
            id: "a".into(),
            state: true,
        });
        notices.push(Notice::Session {
            id: "a".into(),
            state: false,
        });
        notices.push(Notice::Session {
            id: "b".into(),
            state: false,
        });
        notices.push(Notice::Runtime);
        let batch = notices.take();
        assert_eq!(batch.len(), 3);
        assert!(batch.contains(&Notice::Session {
            id: "a".into(),
            state: true
        }));
        assert!(notices.take().is_empty());
        for i in 0..65 {
            notices.push(Notice::Session {
                id: i.to_string(),
                state: false,
            });
        }
        assert_eq!(notices.take(), vec![Notice::Changed]);
    }
    #[test]
    fn content_does_not_refresh_other_resources_or_other_sessions() {
        let mut reads = Reads::default();
        reads.notice(
            Notice::Session {
                id: "other".into(),
                state: false,
            },
            "selected",
        );
        assert!(
            RESOURCES
                .into_iter()
                .all(|r| reads.start(r, "selected").is_none())
        );
        reads.notice(
            Notice::Session {
                id: "selected".into(),
                state: false,
            },
            "selected",
        );
        assert!(reads.start(Resource::Events, "selected").is_some());
        assert!(
            RESOURCES
                .into_iter()
                .filter(|r| *r != Resource::Events)
                .all(|r| reads.start(r, "selected").is_none())
        );
    }
    #[test]
    fn slow_runtime_does_not_block_events_and_inflight_notices_are_retained() {
        let mut reads = Reads::default();
        reads.notice(Notice::Changed, "a");
        let runtime = reads.start(Resource::Runtime, "a").unwrap();
        let events = reads.start(Resource::Events, "a").unwrap();
        for _ in 0..100 {
            reads.invalidate(Resource::Events);
        }
        assert!(reads.start(Resource::Events, "a").is_none());
        assert!(reads.finish(Resource::Events, events));
        let next = reads.start(Resource::Events, "a").unwrap();
        assert!(reads.finish(Resource::Events, next));
        assert!(reads.start(Resource::Events, "a").is_none());
        assert!(reads.finish(Resource::Runtime, runtime));
    }
    #[test]
    fn heartbeat_retries_failed_reads_without_polling_successful_transcripts() {
        let mut reads = Reads::default();
        reads.invalidate(Resource::Events);
        let ticket = reads.start(Resource::Events, "a").unwrap();
        assert!(reads.finish(Resource::Events, ticket));
        reads.failed(Resource::Events);
        assert!(reads.start(Resource::Events, "a").is_none());
        reads.notice(Notice::Runtime, "a");
        let retry = reads.start(Resource::Events, "a").unwrap();
        assert!(reads.finish(Resource::Events, retry));
        reads.notice(Notice::Runtime, "a");
        assert!(reads.start(Resource::Events, "a").is_none());
    }
    #[test]
    fn navigation_and_reconnection_reject_old_completions_even_for_same_session() {
        let mut reads = Reads::default();
        reads.invalidate(Resource::Events);
        let old = reads.start(Resource::Events, "a").unwrap();
        reads.reset(true);
        reads.invalidate(Resource::Events);
        let new = reads.start(Resource::Events, "a").unwrap();
        assert!(!reads.finish(Resource::Events, old));
        assert!(reads.finish(Resource::Events, new));
        reads.reset(false);
        assert!(!reads.finish(Resource::Events, new));
    }
}
