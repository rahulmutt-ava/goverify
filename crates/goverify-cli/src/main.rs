//! `goverify` — SMT-backed static analyzer for Go.
//!
//! Phase 1 ships the developer-facing `extract` subcommand; `check`,
//! `baseline`, and `spec` arrive with the checkers (spec §10, §15).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use goverify_extract::Sidecar;

#[derive(Parser)]
#[command(name = "goverify", version, about = "SMT-backed static analyzer for Go")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Extract .gvir IR artifacts for Go packages (developer command).
    Extract {
        /// Output directory for .gvir files.
        #[arg(short, long, default_value = ".goverify/gvir")]
        out: PathBuf,
        /// Go package patterns, resolved in the current directory.
        #[arg(default_value = "./...")]
        patterns: Vec<String>,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        // Exit codes (spec §10): 0 clean, 1 findings (phase 4+),
        // 2 analyzer error.
        Err(e) => {
            eprintln!("goverify: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().cmd {
        Cmd::Extract { out, patterns } => {
            let sidecar = Sidecar::build(&extractor_dir()?, &sidecar_build_dir())?;
            let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
            let files = sidecar.extract(Path::new("."), &patterns, &out)?;
            for f in &files {
                println!("{}", f.display());
            }
            eprintln!("goverify: extracted {} package(s) to {}", files.len(), out.display());
            Ok(())
        }
    }
}

/// Locate the vendored extractor sources: explicit override first,
/// then the dev-build layout (extractor/ beside the workspace root).
fn extractor_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(dir) = std::env::var("GOVERIFY_EXTRACTOR_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extractor");
    if dev.is_dir() {
        return Ok(dev.canonicalize()?);
    }
    Err("cannot locate extractor sources; set GOVERIFY_EXTRACTOR_DIR".into())
}

fn sidecar_build_dir() -> PathBuf {
    std::env::temp_dir().join("goverify-extractor-bin")
}
