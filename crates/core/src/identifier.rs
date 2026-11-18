//! Sortable identifiers, same shape as packages/schema/src/identifier.ts:
//! a millisecond timestamp plus a per-millisecond counter, hex-encoded in
//! descending bit order, then a random base62 suffix. 26 characters total.
//! Descending flips each time-byte so ids sort newest-first (sessions).

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const LENGTH: usize = 26;
const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static LAST: Mutex<(u64, u64)> = Mutex::new((0, 0));

pub fn ascending() -> String {
    make(false)
}

pub fn descending() -> String {
    make(true)
}

fn make(descending: bool) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as u64;
    let (timestamp, count) = {
        let mut guard = LAST.lock().expect("identifier clock poisoned");
        if now != guard.0 {
            guard.0 = now;
            guard.1 = 0;
        }
        guard.1 += 1;
        (guard.0, guard.1)
    };
    let value = timestamp * 0x1000 + count;
    let mut output = String::with_capacity(LENGTH);
    for index in 0..6u64 {
        let byte = ((value >> ((5 - index) * 8)) & 0xff) as u8;
        let byte = if descending { !byte } else { byte };
        output.push_str(&format!("{byte:02x}"));
    }
    let mut bytes = [0u8; LENGTH - 12];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    for byte in bytes {
        output.push(CHARS[(byte as usize) % CHARS.len()] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascending_ids_sort_by_creation() {
        let first = ascending();
        let second = ascending();
        assert!(first < second, "{first} >= {second}");
        assert_eq!(first.len(), LENGTH);
    }

    #[test]
    fn descending_ids_sort_newest_first() {
        let first = descending();
        let second = descending();
        assert!(first > second, "{first} <= {second}");
        assert_eq!(first.len(), LENGTH);
    }
}
