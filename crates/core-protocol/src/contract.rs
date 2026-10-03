//! The action contract a model is trained and evaluated against.
//!
//! A trained model learns this crate's catalog (names, parameters, risks,
//! executors), the grammar generated from it, and the way the agent reports
//! observations back. Those together are the contract. [`fingerprint`] identifies
//! it: a SHA-256 over a canonical description, so any change to an action, a
//! parameter, a risk level or the grammar gives a new fingerprint.
//!
//! The contract is pinned per dataset, not frozen forever: a dataset records the
//! fingerprint its trajectories were made with, and records made with another one
//! are rejected (`docs/TRAINING.md`). Changing the catalog starts a new dataset
//! version.

use std::fmt::Write as _;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::catalog::CATALOG;
use crate::grammar::{GrammarOptions, gbnf};

/// Bumped by hand when the meaning of the contract changes in a way the canonical
/// description below does not capture.
pub const CONTRACT_VERSION: u32 = 1;

/// Version of the observation text the agent feeds back to the model (labels such
/// as `OBSERVATION`, how command output, exit status, denials and refusals are
/// rendered; `core-agent`'s `prompt` module). Bump it whenever that format
/// changes: the fingerprint cannot see the agent's code.
pub const OBSERVATION_FORMAT_VERSION: u32 = 1;

/// The canonical text the fingerprint is computed over, one fact per line.
pub fn canonical() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "contract {CONTRACT_VERSION}");
    let _ = writeln!(out, "protocol {}", crate::PROTOCOL_VERSION);
    let _ = writeln!(out, "observation-format {OBSERVATION_FORMAT_VERSION}");
    for spec in CATALOG {
        let _ = writeln!(
            out,
            "action {} risk={} executor={:?} category={:?} terminal={}",
            spec.name, spec.risk, spec.executor, spec.category, spec.terminal
        );
        for p in spec.params {
            let _ = writeln!(out, "  param {} required={} kind={}", p.name, p.required, p.kind.describe());
        }
    }
    let _ = writeln!(out, "grammar");
    out.push_str(&gbnf(&GrammarOptions::default()));
    out
}

/// `sha256:<hex>` of [`canonical`].
pub fn fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let digest = Sha256::digest(canonical().as_bytes());
        let mut hex = String::with_capacity(7 + 64);
        hex.push_str("sha256:");
        for b in digest.iter() {
            let _ = write!(hex, "{b:02x}");
        }
        hex
    })
}

/// Whether `s` is shaped like a fingerprint (`sha256:` and 64 lowercase hex digits).
pub fn is_fingerprint(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_well_formed() {
        assert!(is_fingerprint(fingerprint()), "{}", fingerprint());
        assert_eq!(fingerprint(), fingerprint());
        assert!(!is_fingerprint("sha256:ABC"));
        assert!(!is_fingerprint("md5:00"));
    }

    #[test]
    fn canonical_covers_every_action_parameter_and_the_grammar() {
        let c = canonical();
        for spec in CATALOG {
            assert!(c.contains(&format!("action {} risk={}", spec.name, spec.risk)), "{}", spec.name);
            for p in spec.params {
                assert!(c.contains(&format!("  param {} required={}", p.name, p.required)), "{}.{}", spec.name, p.name);
            }
        }
        assert!(c.contains(&gbnf(&GrammarOptions::default())), "the grammar text is included");
    }

    #[test]
    fn any_change_changes_the_fingerprint() {
        let base = Sha256::digest(canonical().as_bytes());
        let changed = canonical().replacen("risk=high", "risk=medium", 1);
        assert_ne!(base, Sha256::digest(changed.as_bytes()));
    }
}
