//! Drives the Go extractor sidecar; owns the `.gvir` protobuf schema
//! bindings and loader.

pub mod gvir {
    #![allow(clippy::all, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/gvir.v1.rs"));
}

mod load;

pub use load::{load_package, LoadError, SCHEMA_VERSION};
