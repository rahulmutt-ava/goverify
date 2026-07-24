//! Drives the Go extractor sidecar; owns the `.gvir` protobuf schema
//! bindings and loader.

pub mod gvir {
    #![allow(clippy::all, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/gvir.v1.rs"));
}

mod cached;
mod load;
mod sidecar;

pub use cached::{ExtractStats, load_packages_cached};
pub use load::{LoadError, SCHEMA_VERSION, load_package, load_package_bytes};
pub use sidecar::{ManifestPkg, Sidecar, SidecarError};
