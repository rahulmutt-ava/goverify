//! Content-addressed cache (parent spec §9; phase-3 spec §7). Phase 3
//! ships the generic store + the query layer; extraction/summary layers
//! land in phase 5 on the same Store.

mod query;
mod store;

pub use query::{CachedOutcome, QueryCache, QueryKeyParts, query_key};
pub use store::Store;
