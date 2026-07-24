//! Decode arbitrary bytes as an SCC cache entry. The decoder parses
//! bytes the current binary didn't necessarily write (shared caches,
//! version skew, corruption) — it must reject, never panic (parent
//! spec §12.4; phase-5a spec §4).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let _ = goverify_analysis::decode_entry_bytes(data);
});
