//! Host ID validation and the check against the ID stored in hosts.json.

use combine_protocol::HostId;

use super::config::StoreError;

/// A lowercase hyphenated UUID shape: 36 chars, `-` at 8, 13, 18 and 23, lowercase hex elsewhere.
pub fn is_valid_host_id(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityCheck {
    FirstContact,
    Match,
    Mismatch,
}

pub fn check(stored: Option<&HostId>, reported: &HostId) -> IdentityCheck {
    match stored {
        None => IdentityCheck::FirstContact,
        Some(stored) if stored == reported => IdentityCheck::Match,
        Some(_) => IdentityCheck::Mismatch,
    }
}

pub fn mismatch_message(name: &str, stored: &HostId, reported: &HostId) -> String {
    format!(
        "host ID mismatch for \"{name}\": hosts.json has {}, but the daemon reported {}. Combine closed the connection. If this is the same machine reinstalled, remove \"host_id\" from its entry in hosts.json and restart Combine.",
        stored.0, reported.0
    )
}

pub fn duplicate_message(name: &str, other: &str, id: &HostId) -> String {
    format!(
        "\"{name}\" reaches the same machine as \"{other}\" (host ID {}). Remove one of the two entries from hosts.json and restart Combine.",
        id.0
    )
}

/// Where a supervisor records the ID a host reported on first contact.
pub trait HostIdStore: Send + Sync {
    fn record_first_contact(&self, name: &str, host_id: &HostId) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> HostId {
        HostId(s.to_string())
    }

    #[test]
    fn valid_host_ids() {
        assert!(is_valid_host_id("6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c"));
        assert!(is_valid_host_id("00000000-0000-0000-0000-000000000000"));
        assert!(!is_valid_host_id(""));
        assert!(!is_valid_host_id("6A1D3C9E-2F4B-4E8A-B7C5-1D0E9F8A7B6C"));
        assert!(!is_valid_host_id("6a1d3c9e2f4b4e8ab7c51d0e9f8a7b6c"));
        assert!(!is_valid_host_id("6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6"));
        assert!(!is_valid_host_id("6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6cc"));
        assert!(!is_valid_host_id("6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6g"));
        assert!(!is_valid_host_id(
            "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6\u{e9}"
        ));
        assert!(!is_valid_host_id("6a1d3c9e_2f4b-4e8a-b7c5-1d0e9f8a7b6c"));
    }

    /// The daemon's `is_valid_host_id` reads the same file and must give the same answers.
    #[test]
    fn host_id_rule_matches_the_shared_table() {
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../crates/combine-protocol/tests/fixtures/host_id_cases.json"
        ))
        .expect("the table is JSON");
        for (key, expected) in [("accept", true), ("reject", false)] {
            let ids = cases[key].as_array().expect("the table has the list");
            assert!(!ids.is_empty());
            for id in ids {
                let id = id.as_str().expect("the table holds strings");
                assert_eq!(is_valid_host_id(id), expected, "{id:?}");
            }
        }
    }

    #[test]
    fn check_outcomes() {
        let a = id("6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c");
        let b = id("0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c");
        assert_eq!(check(None, &a), IdentityCheck::FirstContact);
        assert_eq!(check(Some(&a), &a), IdentityCheck::Match);
        assert_eq!(check(Some(&a), &b), IdentityCheck::Mismatch);
        assert_eq!(
            mismatch_message("mini", &a, &b),
            "host ID mismatch for \"mini\": hosts.json has 6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c, but the daemon reported 0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c. Combine closed the connection. If this is the same machine reinstalled, remove \"host_id\" from its entry in hosts.json and restart Combine."
        );
        assert_eq!(
            duplicate_message("spare", "mini", &a),
            "\"spare\" reaches the same machine as \"mini\" (host ID 6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c). Remove one of the two entries from hosts.json and restart Combine."
        );
    }
}
