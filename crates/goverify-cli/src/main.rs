//! `goverify` — SMT-backed static analyzer for Go.
//!
//! Phase 1 ships the developer-facing `extract` subcommand; phase 2 adds
//! `debug` for inspecting the analyzer's view of a module (spec §7).
//! `check`, `baseline`, and `spec` arrive with the checkers (spec §10, §15).

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use goverify_extract::Sidecar;

#[derive(Parser)]
#[command(
    name = "goverify",
    version,
    about = "SMT-backed static analyzer for Go"
)]
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
    /// Inspect the analyzer's view of a module (phase-2 spec §7).
    Debug {
        #[command(subcommand)]
        what: DebugWhat,
    },
}

#[derive(clap::Args)]
struct DebugArgs {
    /// Directory of pre-extracted .gvir files. When omitted, extracts the
    /// current directory into a temp dir first.
    #[arg(long)]
    gvir_dir: Option<PathBuf>,
    /// Restrict output to one function (substring match on the ssa id).
    #[arg(long)]
    func: Option<String>,
    /// Go package patterns for extraction (ignored with --gvir-dir).
    #[arg(default_value = "./...")]
    patterns: Vec<String>,
}

#[derive(Subcommand)]
enum DebugWhat {
    /// Dump lowered function bodies (goverify_ir::dump_function).
    Ir(DebugArgs),
    /// Dump the whole-program call graph.
    Callgraph(DebugArgs),
    /// Dump strongly-connected components of the call graph.
    Sccs(DebugArgs),
    /// Dump per-function prepass domains.
    Prepass(DebugArgs),
    /// Dump instantiated function summaries.
    Summary(DebugArgs),
    /// Run the analysis + checkers and print findings (phase-3 tracer).
    Findings(FindingsArgs),
}

#[derive(clap::Args)]
struct FindingsArgs {
    #[command(flatten)]
    common: DebugArgs,
    /// Dump every canonical SMT-LIB2 query to this directory.
    #[arg(long)]
    emit_smt: Option<PathBuf>,
    /// Solve via an external SMT-LIB2 binary instead of built-in Z3.
    #[arg(long)]
    solver_cmd: Option<String>,
    /// Per-query timeout in milliseconds.
    #[arg(long, default_value_t = 100)]
    solver_timeout_ms: u32,
    /// Query-cache directory (omit to run uncached).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
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
            eprintln!(
                "goverify: extracted {} package(s) to {}",
                files.len(),
                out.display()
            );
            Ok(())
        }
        Cmd::Debug { what } => run_debug(what),
    }
}

fn run_debug(what: DebugWhat) -> Result<(), Box<dyn std::error::Error>> {
    let (kind, args) = match what {
        DebugWhat::Ir(a) => ("ir", a),
        DebugWhat::Callgraph(a) => ("callgraph", a),
        DebugWhat::Sccs(a) => ("sccs", a),
        DebugWhat::Prepass(a) => ("prepass", a),
        DebugWhat::Summary(a) => ("summary", a),
        DebugWhat::Findings(fa) => return run_findings(fa),
    };
    // --func filters per-function output; callgraph/sccs dumps are
    // whole-program (final-review deferred T15) — warn instead of
    // silently ignoring the flag.
    if args.func.is_some() && matches!(kind, "callgraph" | "sccs") {
        eprintln!("goverify: --func has no effect on `debug {kind}`; ignoring");
    }
    let program = load_program(&args)?;
    for d in program.diagnostics() {
        eprintln!("goverify: {d}");
    }
    // --func is a substring filter everywhere (help text says so).
    let selected = |name: &str| args.func.as_deref().is_none_or(|f| name.contains(f));
    match kind {
        "ir" => {
            for f in program.func_ids() {
                if program.func(f).is_some() && selected(program.func_name(f)) {
                    print!("{}", goverify_ir::dump_function(&program, f));
                    println!();
                }
            }
        }
        "callgraph" => {
            let g = goverify_ir::CallGraph::build(&program);
            print!("{}", goverify_ir::dump_callgraph(&program, &g));
        }
        "sccs" => {
            let g = goverify_ir::CallGraph::build(&program);
            let s = goverify_ir::Sccs::compute(&program, &g);
            print!("{}", goverify_ir::dump_sccs(&program, &s));
        }
        "prepass" | "summary" => {
            let a = goverify_analysis::analyze(&program, &goverify_analysis::Options::default());
            for d in &a.diagnostics {
                eprintln!("goverify: {d}");
            }
            if kind == "prepass" {
                print!(
                    "{}",
                    goverify_analysis::dump_prepass(&program, &a, args.func.as_deref())
                );
            } else {
                print!(
                    "{}",
                    goverify_analysis::dump_summaries(&program, &a, args.func.as_deref())
                );
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Shared gvir-dir resolution: an explicit `--gvir-dir` is loaded as-is;
/// otherwise extract the current directory into a fresh temp dir first
/// (the tempdir is cleaned up once this function returns, after
/// `Program::load_dir` has already copied everything it needs into
/// memory).
fn load_program(args: &DebugArgs) -> Result<goverify_ir::Program, Box<dyn std::error::Error>> {
    let mut _tmp: Option<tempfile::TempDir> = None; // keep tempdir alive
    let gvir_dir = match &args.gvir_dir {
        Some(d) => d.clone(),
        None => {
            let sidecar = Sidecar::build(&extractor_dir()?, &sidecar_build_dir())?;
            let tmp = tempfile::tempdir()?;
            let patterns: Vec<&str> = args.patterns.iter().map(String::as_str).collect();
            sidecar.extract(Path::new("."), &patterns, tmp.path())?;
            let d = tmp.path().to_path_buf();
            _tmp = Some(tmp);
            d
        }
    };
    let program = goverify_ir::Program::load_dir(&gvir_dir)?;
    Ok(program)
}

/// `debug findings` (phase-3 tracer, this task's end-to-end milestone):
/// extract/load, run the checkers through `analyze_full`, print every
/// `Sat`-confirmed finding.
fn run_findings(fa: FindingsArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Filtering findings is a `check`-UX concern (phase 4); the flattened
    // DebugArgs only contributes gvir-dir/patterns here, but clap still
    // drags --func along, so warn instead of silently ignoring it (same
    // convention as the callgraph/sccs arms above).
    if fa.common.func.is_some() {
        eprintln!("goverify: --func has no effect on `debug findings`; ignoring");
    }
    let program = load_program(&fa.common)?;
    for d in program.diagnostics() {
        eprintln!("goverify: {d}");
    }
    let limits = goverify_solver::SolverLimits {
        timeout_ms: fa.solver_timeout_ms,
        ..Default::default()
    };
    let cfg = goverify_analysis::EngineConfig {
        opts: goverify_analysis::Options::default(),
        cache_dir: fa.cache_dir.clone(),
        emit_smt: fa.emit_smt.clone(),
    };
    let cmd = fa.solver_cmd.clone();
    let mk: Box<dyn Fn() -> Box<dyn goverify_solver::TextSolver> + Sync> = match cmd {
        Some(c) => Box::new(move || Box::new(goverify_solver::SmtLib2Process::new(&c, limits))),
        None => Box::new(move || Box::new(goverify_solver::Z3Native::new(limits))),
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&goverify_checkers::NilTracer];
    let a = goverify_analysis::analyze_full(&program, &cfg, &checkers, &*mk);
    for d in &a.diagnostics {
        eprintln!("goverify: {d}");
    }
    print!("{}", goverify_analysis::dump_findings(&a, None));
    Ok(())
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

/// Sidecar build cache root: user-scoped (`$XDG_CACHE_HOME/goverify` or
/// `$HOME/.cache/goverify`, spec §9), created 0700. A predictable,
/// world-writable-parent path (bare `temp_dir()`) would let another local
/// user pre-plant a binary for `Sidecar::build` to execute unchecked
/// (CWE-377); temp_dir() is used only as a last-resort fallback.
fn sidecar_build_dir() -> PathBuf {
    let cache_root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    let Some(cache_root) = cache_root else {
        return std::env::temp_dir().join("goverify-extractor-bin");
    };
    let dir = cache_root.join("goverify");
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    dir.join("extractor-bin")
}
