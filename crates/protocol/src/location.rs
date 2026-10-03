//! Project placement uses stable target identities, never executor generations.
//! All historical host IDs belong to the current daemon's single host group.
pub fn target_id(environment: &str) -> &str {
    if environment == "host" || environment.starts_with("host-") { return "host"; }
    for length in [36, 32] {
        if environment.len() > length + 1 {
            let split = environment.len() - length;
            if environment.is_char_boundary(split) && environment.as_bytes()[split - 1] == b'-' {
                let suffix = &environment[split..];
                let valid = suffix.bytes().enumerate().all(|(i,b)| {
                    if length == 36 && matches!(i,8|13|18|23) { b == b'-' } else { b.is_ascii_hexdigit() }
                });
                if valid && !matches!(&environment[..split - 1], "ssh" | "vm" | "container" | "external") { return &environment[..split - 1]; }
            }
        }
    }
    environment
}

pub fn preferred<'a>(ids: impl IntoIterator<Item = &'a str>, reported: Option<&str>) -> Option<&'a str> {
    let ids: Vec<_> = ids.into_iter().collect();
    reported.and_then(|id| ids.iter().copied().find(|candidate| *candidate == id))
        .or_else(|| ids.iter().copied().find(|id| target_id(id) == "host"))
        .or_else(|| ids.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_group_survives_any_number_of_executor_restarts() {
        assert_eq!(target_id("host"), "host");
        for generation in 0..100 {
            assert_eq!(target_id(&format!("host-{generation:032x}")), "host");
        }
        assert_eq!(target_id("host-76b8efe0-c965-4dce-811b-c3b76389813f"), "host");
        assert_eq!(target_id("host-old"), "host");
        // A similar display name must not collapse a distinct remote target.
        assert_ne!(target_id("ssh-host"), "host");
        assert_ne!(target_id("external-host"), "host");
    }

    #[test]
    fn remote_groups_survive_restarts_without_merging_distinct_targets() {
        for kind in ["ssh", "vm", "container", "external"] {
            let stable = format!("{kind}-76b8efe0-c965-4dce-811b-c3b76389813f");
            let other = format!("{kind}-769c319a-e739-4cf8-ba39-28fe564f46d3");
            assert_eq!(target_id(&stable), stable);
            for generation in 0..100 {
                // Native executors use compact UUIDs; SSH uses hyphenated UUIDs.
                for suffix in [format!("{generation:032x}"), format!("00000000-0000-0000-0000-{generation:012x}")] {
                    assert_eq!(target_id(&format!("{stable}-{suffix}")), stable);
                    assert_eq!(target_id(&format!("{other}-{suffix}")), other);
                }
            }
            assert_ne!(target_id(&stable), target_id(&other));
        }
    }

    #[test]
    fn stable_generations_and_host_fallback() {
        let ssh = "ssh-76b8efe0-c965-4dce-811b-c3b76389813f";
        assert_eq!(target_id(ssh), ssh);
        assert_eq!(target_id(&format!("{ssh}-76b8efe0-c965-4dce-811b-c3b76389813f")), ssh);
        assert_eq!(target_id("vm-one-76b8efe0c9654dce811bc3b76389813f"), "vm-one");
        assert_eq!(preferred(["ssh-one", "host-new"], None), Some("host-new"));
        assert_eq!(preferred(["ssh-one", "host-new"], Some("ssh-one")), Some("ssh-one"));
        assert_eq!(preferred(["ssh-one", "ssh-two"], Some("gone")), Some("ssh-one"));
    }
}
