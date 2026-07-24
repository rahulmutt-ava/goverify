//! Content-addressed cache (parent spec §9; phase-3 spec §7). Ships the
//! generic byte-only Store + the query layer; the phase-5a extraction and
//! SCC-summary layers key/frame their own bytes and write to the same Store
//! (in goverify-extract and goverify-analysis respectively).

mod query;
mod store;

pub use query::{CachedOutcome, QueryCache, QueryKeyParts, query_key};
pub use store::Store;
