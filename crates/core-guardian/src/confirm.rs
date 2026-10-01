//! Pending human confirmations.
//!
//! When policy demands confirmation, the Guardian parks the validated action under a
//! random single-use token and tells the client. Only the same connection can redeem
//! the token, and only before it expires. The model never sees or produces tokens:
//! the shell asks the human directly and answers on the human's behalf.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::time::{Duration, Instant};

use core_protocol::ValidatedAction;

pub struct Pending {
    pub request_id: u64,
    pub action: ValidatedAction,
    created: Instant,
}

pub struct Confirmations {
    pending: HashMap<String, Pending>,
    ttl: Duration,
    max: usize,
}

impl Confirmations {
    pub fn new(ttl: Duration, max: usize) -> Self {
        Confirmations { pending: HashMap::new(), ttl, max: max.max(1) }
    }

    /// Park an action; returns its token.
    pub fn insert(&mut self, request_id: u64, action: ValidatedAction) -> String {
        let ttl = self.ttl;
        self.pending.retain(|_, p| p.created.elapsed() < ttl);
        while self.pending.len() >= self.max {
            let oldest = self.pending.iter().min_by_key(|(_, p)| p.created).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.pending.remove(&k),
                None => break,
            };
        }
        let token = random_token();
        self.pending.insert(token.clone(), Pending { request_id, action, created: Instant::now() });
        token
    }

    /// Redeem a token (single use). `None` if unknown or expired.
    pub fn take(&mut self, token: &str) -> Option<Pending> {
        let p = self.pending.remove(token)?;
        (p.created.elapsed() < self.ttl).then_some(p)
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// 128 random bits, hex encoded.
pub fn random_token() -> String {
    let mut buf = [0u8; 16];
    // SAFETY: getrandom(2) writes at most buf.len() bytes into a valid buffer.
    let n = unsafe { libc::getrandom(buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n != buf.len() as isize {
        File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).expect("no source of randomness available");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use core_protocol::Intent;
    use serde_json::json;

    use super::*;

    fn action() -> ValidatedAction {
        ValidatedAction::from_intent(&Intent::new("reboot", json!({}))).unwrap()
    }

    #[test]
    fn tokens_are_single_use_and_random() {
        let mut c = Confirmations::new(Duration::from_secs(60), 4);
        let t = c.insert(1, action());
        assert_eq!(t.len(), 32);
        assert_ne!(t, random_token());
        assert_eq!(c.take(&t).unwrap().request_id, 1);
        assert!(c.take(&t).is_none());
        assert!(c.take("forged").is_none());
    }

    #[test]
    fn tokens_expire() {
        let mut c = Confirmations::new(Duration::from_millis(1), 4);
        let t = c.insert(1, action());
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.take(&t).is_none());
    }

    #[test]
    fn bounded() {
        let mut c = Confirmations::new(Duration::from_secs(60), 2);
        let first = c.insert(1, action());
        c.insert(2, action());
        c.insert(3, action());
        assert_eq!(c.len(), 2);
        assert!(c.take(&first).is_none(), "oldest evicted");
    }
}
