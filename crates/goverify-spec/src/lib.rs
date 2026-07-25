//! Summary/annotation format: parse, resolve, lower (parent spec §6,
//! phase-6 spec). The compiler is pure and deterministic; ANNOTATION_VERSION
//! is salted into the SCC cache — bump it on ANY semantics change to
//! parse/resolve/lower.

pub mod ast;
pub mod parse;

/// Cache-key version of annotation-compilation semantics.
pub const ANNOTATION_VERSION: u32 = 1;
