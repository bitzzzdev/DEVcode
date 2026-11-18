//! Sortable ascending identifiers, same shape as packages/schema/src/identifier.ts:
//! a millisecond timestamp plus a per-millisecond counter in hex, then random
//! base62 suffix. 26 characters total.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const LENGTH: usize = 26;
const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static LAST: Mutex<(u64, u64)> = Mutex::new((0, 0));

pub fn ascending() -> String {
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
        let byte = (value >> ((5 - index) * 8)) & 0xff;
        output.push_str(&format!("{byte:02x}"));
    }
    let mut bytes = [0u8; LENGTH - 12];
    getrandom::getrandom(&mut bytes).expect("system RNG unavailable");
    for byte in bytes {
        output.push(CHARS[(byte as usize) % CHARS.len()] as char);
    }
    output
}
