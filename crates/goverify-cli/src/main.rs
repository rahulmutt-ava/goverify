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
use goverify_solver::TextSolver;

mod diff;
mod json;
mod render;
mod sarif;

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
    /// Analyze packages and report findings (spec §10).
    Check(CheckArgs),
    /// Manage the findings baseline (spec §10).
    Baseline {
        #[command(subcommand)]
        what: BaselineWhat,
    },
}

#[derive(Subcommand)]
enum BaselineWhat {
    /// Record current findings in .goverify/baseline.json; later
    /// `check` runs report only new findings.
    Write(CheckArgs),
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum OutputFormat {
    /// Labeled source spans with traces (default).
    Human,
    /// Native machine schema (schema_version 1).
    Json,
    /// SARIF 2.1.0 for GitHub code scanning.
    Sarif,
}

#[derive(clap::Args)]
struct CheckArgs {
    /// Directory of pre-extracted .gvir files (omit to extract).
    #[arg(long)]
    gvir_dir: Option<PathBuf>,
    /// Go package patterns for extraction (ignored with --gvir-dir).
    #[arg(default_value = "./...")]
    patterns: Vec<String>,
    /// Dump every canonical SMT-LIB2 query to this directory.
    #[arg(long)]
    emit_smt: Option<PathBuf>,
    /// Solve via an external SMT-LIB2 binary instead of built-in Z3.
    #[arg(long)]
    solver_cmd: Option<String>,
    /// Per-query timeout for requires-inference queries (ms).
    #[arg(long, default_value_t = 100)]
    solver_timeout_ms: u32,
    /// Per-query timeout for obligation (findings) queries (ms) —
    /// function-sized formulas get more room (spec §8).
    #[arg(long, default_value_t = 250)]
    obligation_timeout_ms: u32,
    /// Cache directory (default: $XDG_CACHE_HOME/goverify, falling back
    /// to ~/.cache/goverify — spec §9). Project-local hermetic mode:
    /// pass an explicit dir (the shakeout does).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Disable all cache layers (extract, scc, query).
    #[arg(long, conflicts_with = "cache_dir")]
    no_cache: bool,
    /// Import-path prefix the report is scoped to (defaults to the
    /// module path from the nearest `go.mod`). Extraction walks the whole
    /// import closure — stdlib and deps included — so inference stays
    /// whole-program, but only findings in packages under this prefix are
    /// rendered and gate the exit code.
    #[arg(long)]
    scope: Option<String>,
    /// Output format (spec §10): human terminal report, or machine
    /// formats for CI. Machine formats are byte-identical across runs.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
    /// Baseline file to suppress known findings (default:
    /// .goverify/baseline.json when it exists — spec §4). The exit code
    /// gates on the post-suppression count.
    #[arg(long, conflicts_with = "no_baseline")]
    baseline: Option<PathBuf>,
    /// Ignore any baseline file.
    #[arg(long)]
    no_baseline: bool,
    /// Report only findings in functions changed since this git ref, or
    /// in their transitive callers (spec §10). Analysis still covers
    /// everything; only the report is scoped. Requires git.
    #[arg(long, value_name = "GIT_REF")]
    diff_base: Option<String>,
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
        Ok(code) => code,
        // Exit codes (spec §10): 0 clean, 1 findings (check only), 2
        // usage/analyzer error.
        Err(e) => {
            eprintln!("goverify: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
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
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Debug { what } => run_debug(what).map(|()| ExitCode::SUCCESS),
        Cmd::Check(ca) => run_check(ca),
        Cmd::Baseline { what } => match what {
            BaselineWhat::Write(ca) => run_baseline_write(ca),
        },
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
    let program = load_program(Path::new("."), &args)?;
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
/// otherwise extract `dir` into a fresh temp dir first (the tempdir is
/// cleaned up once this function returns, after `Program::load_dir` has
/// already copied everything it needs into memory). Debug-command call
/// sites are all cwd-bound (`Path::new(".")`); `acquire_program` passes
/// its caller-supplied `dir` through instead.
fn load_program(
    dir: &Path,
    args: &DebugArgs,
) -> Result<goverify_ir::Program, Box<dyn std::error::Error>> {
    let mut _tmp: Option<tempfile::TempDir> = None; // keep tempdir alive
    let gvir_dir = match &args.gvir_dir {
        Some(d) => d.clone(),
        None => {
            let sidecar = Sidecar::build(&extractor_dir()?, &sidecar_build_dir())?;
            let tmp = tempfile::tempdir()?;
            let patterns: Vec<&str> = args.patterns.iter().map(String::as_str).collect();
            sidecar.extract(dir, &patterns, tmp.path())?;
            let d = tmp.path().to_path_buf();
            _tmp = Some(tmp);
            d
        }
    };
    let program = goverify_ir::Program::load_dir(&gvir_dir)?;
    Ok(program)
}

/// Retry-tier escalation (wave-2 spec §2): an Unknown at the base
/// timeout is re-issued once at 10x (100ms -> 1s for Infer at the
/// defaults). Applied uniformly to both backend roles; if the shakeout
/// gate shows unacceptable wall-clock cost, restricting to Infer here
/// is the pre-agreed fallback.
const RETRY_FACTOR: u32 = 10;

fn escalated(lim: goverify_solver::SolverLimits) -> goverify_solver::SolverLimits {
    goverify_solver::SolverLimits {
        timeout_ms: lim.timeout_ms.saturating_mul(RETRY_FACTOR),
        ..lim
    }
}

fn retry_backend(
    cmd: &Option<String>,
    lim: goverify_solver::SolverLimits,
) -> Box<dyn goverify_solver::TextSolver> {
    let esc = escalated(lim);
    match cmd {
        Some(c) => {
            let base = goverify_solver::SmtLib2Process::new(c, lim);
            let identity = base.identity();
            let c = c.clone();
            Box::new(goverify_solver::RetryBackend::new(
                Box::new(base),
                Box::new(goverify_solver::LazySolver::new(
                    identity,
                    esc,
                    Box::new(move || Box::new(goverify_solver::SmtLib2Process::new(&c, esc))),
                )),
            ))
        }
        None => {
            let base = goverify_solver::Z3Native::new(lim);
            // Same z3 build, same identity string — safe to carry as
            // data; the escalated tier's cache entries stay keyed
            // identically to the eager construction they replace.
            let identity = base.identity();
            Box::new(goverify_solver::RetryBackend::new(
                Box::new(base),
                Box::new(goverify_solver::LazySolver::new(
                    identity,
                    esc,
                    Box::new(move || Box::new(goverify_solver::Z3Native::new(esc))),
                )),
            ))
        }
    }
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
    let program = load_program(Path::new("."), &fa.common)?;
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
        annotations: goverify_analysis::Annotations::default(),
    };
    let cmd = fa.solver_cmd.clone();
    // `debug findings` keeps one timeout for both backend roles; `check`
    // (Task 11) differentiates Infer vs Findings.
    let mk: Box<
        dyn Fn(goverify_analysis::BackendRole) -> Box<dyn goverify_solver::TextSolver> + Sync,
    > = Box::new(move |_role| retry_backend(&cmd, limits));
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&goverify_checkers::NilChecker];
    let a = goverify_analysis::analyze_full(&program, &cfg, &checkers, &*mk);
    for d in &a.diagnostics {
        eprintln!("goverify: {d}");
    }
    let esc = goverify_solver::escalation_count();
    if esc > 0 {
        eprintln!("goverify: solver: {esc} queries escalated to the retry tier");
    }
    print!("{}", goverify_analysis::dump_findings(&a, None));
    Ok(())
}

/// Everything `check` and `baseline write` share (spec §4): cache-root
/// resolution, program acquisition (extraction cache when available),
/// the engine run, and module scoping. Returns the SCOPED findings in
/// render order plus the pieces `--diff-base` (spec §5) needs.
struct Analyzed {
    program: goverify_ir::Program,
    scoped: Vec<goverify_analysis::Finding>,
    cache_root: Option<PathBuf>,
    timings: bool,
}

/// `check` (spec §10, this task): the user-facing analyzer entry point.
/// Two solver-timeout tiers — tight for the per-SCC requires-inference
/// backend, generous for the sequential findings pass that gates
/// user-visible output (`BackendRole` doc comment, engine.rs) — unlike
/// `debug findings`, which keeps one timeout for both roles.
fn analyze_module(ca: &CheckArgs) -> Result<Analyzed, Box<dyn std::error::Error>> {
    // Phase wall-clocks on stderr, opt-in by setting GOVERIFY_TIMINGS to
    // any value (presence-checked, not compared to "1"; spec §6 rider 1 /
    // G4). stderr only: stdout is the cold/warm byte-identity surface.
    let timings = std::env::var_os("GOVERIFY_TIMINGS").is_some();
    // Cache-root resolution (spec §9): --no-cache disables every layer
    // (extract, scc, query) by leaving cache_root at None; otherwise an
    // explicit --cache-dir wins, falling back to the user cache root.
    // No root resolvable (no XDG_CACHE_HOME or HOME) degrades to an
    // uncached run rather than failing.
    let cache_root: Option<PathBuf> = if ca.no_cache {
        None
    } else {
        match ca.cache_dir.clone().or_else(user_cache_root) {
            Some(r) => Some(r),
            None => {
                eprintln!("goverify: no cache root (no XDG_CACHE_HOME or HOME); running uncached");
                None
            }
        }
    };
    let t_extract = std::time::Instant::now();
    let program = acquire_program(
        Path::new("."),
        ca.gvir_dir.as_ref(),
        &ca.patterns,
        cache_root.as_ref(),
        timings,
    )?;
    if timings {
        eprintln!(
            "goverify: timing: extract+load {:.2}s",
            t_extract.elapsed().as_secs_f64()
        );
    }
    for d in program.diagnostics() {
        eprintln!("goverify: {d}");
    }
    let infer = goverify_solver::SolverLimits {
        timeout_ms: ca.solver_timeout_ms,
        ..Default::default()
    };
    let oblig = goverify_solver::SolverLimits {
        timeout_ms: ca.obligation_timeout_ms,
        ..Default::default()
    };
    let cmd = ca.solver_cmd.clone();
    let mk: Box<
        dyn Fn(goverify_analysis::BackendRole) -> Box<dyn goverify_solver::TextSolver> + Sync,
    > = Box::new(move |role| {
        let lim = match role {
            goverify_analysis::BackendRole::Infer => infer,
            goverify_analysis::BackendRole::Findings => oblig,
        };
        retry_backend(&cmd, lim)
    });
    let cfg = goverify_analysis::EngineConfig {
        opts: goverify_analysis::Options::default(),
        cache_dir: cache_root.clone(),
        emit_smt: ca.emit_smt.clone(),
        annotations: goverify_analysis::Annotations::default(),
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![
        &goverify_checkers::NilChecker,
        &goverify_checkers::BoundsChecker,
    ];
    let t_analyze = std::time::Instant::now();
    let a = goverify_analysis::analyze_full(&program, &cfg, &checkers, &*mk);
    if timings {
        eprintln!(
            "goverify: timing: analyze {:.2}s",
            t_analyze.elapsed().as_secs_f64()
        );
        eprintln!(
            "goverify: timing: scc cache {} hit / {} miss",
            a.scc_cache_hits, a.scc_cache_misses
        );
    }
    for d in &a.diagnostics {
        eprintln!("goverify: {d}");
    }
    let esc = goverify_solver::escalation_count();
    if esc > 0 {
        eprintln!("goverify: solver: {esc} queries escalated to the retry tier");
    }
    // Scope findings to the analyzed module: extraction walks the whole
    // import closure (stdlib + deps), so `a.findings` covers far more than
    // the user asked to check. Inference/summaries above already used the
    // whole closure; only what we render and count is scoped. Exit code 1
    // keys off the SCOPED set.
    let scope = ca.scope.clone().or_else(|| module_path(Path::new(".")));
    let scoped: Vec<goverify_analysis::Finding> = match &scope {
        Some(s) => scope_findings(&a.findings, s),
        None => {
            eprintln!(
                "goverify: no module scope (no go.mod found and no --scope); \
                 reporting findings across the whole import closure"
            );
            a.findings.clone()
        }
    };
    Ok(Analyzed {
        program,
        scoped,
        cache_root,
        timings,
    })
}

/// Program acquisition for the module rooted at `dir` (spec §9 paths):
/// an explicit gvir_dir loads as-is; with a cache root, extract through
/// the extraction cache and fall back to plain extraction on any cache
/// failure (degrade, never die); otherwise plain extraction.
fn acquire_program(
    dir: &Path,
    gvir_dir: Option<&PathBuf>,
    patterns: &[String],
    cache_root: Option<&PathBuf>,
    timings: bool,
) -> Result<goverify_ir::Program, Box<dyn std::error::Error>> {
    let dargs = DebugArgs {
        gvir_dir: gvir_dir.cloned(),
        func: None,
        patterns: patterns.to_vec(),
    };
    // Program acquisition: --gvir-dir (or no cache root) takes the plain
    // load_program path (no extraction-cache interaction, spec §9);
    // otherwise extract through the cache and fall back to plain
    // extraction on any cache failure (degrade, never die).
    match (gvir_dir, cache_root) {
        (Some(_), _) | (None, None) => load_program(dir, &dargs),
        (None, Some(root)) => {
            let sidecar = Sidecar::build(&extractor_dir()?, &sidecar_build_dir())?;
            let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
            match goverify_extract::load_packages_cached(&sidecar, dir, &patterns, root) {
                Ok((pkgs, stats)) => {
                    if timings {
                        eprintln!(
                            "goverify: timing: extract cache {} hit / {} extracted",
                            stats.cached, stats.extracted
                        );
                    }
                    Ok(goverify_ir::Program::from_packages(pkgs))
                }
                Err(e) => {
                    eprintln!("goverify: extraction cache unavailable ({e}); extracting uncached");
                    load_program(dir, &dargs)
                }
            }
        }
    }
}

/// Baseline filtering (spec §4). An explicit --baseline must exist; the
/// implicit .goverify/baseline.json applies only when present. A
/// malformed or unreadable file is a hard error (exit 2 via run()) —
/// the documented degrade-never-die exception: this is user-authored
/// gate configuration, and silently reporting unfiltered findings would
/// flood CI and misreport the gate. Returns (kept findings, their
/// fingerprints, suppressed count).
#[allow(clippy::type_complexity)] // interface fixed by the phase-5b plan (Task 10 relocates this call)
fn apply_baseline(
    ca: &CheckArgs,
    findings: Vec<goverify_analysis::Finding>,
    fps: Vec<String>,
) -> Result<(Vec<goverify_analysis::Finding>, Vec<String>, usize), Box<dyn std::error::Error>> {
    let path: Option<PathBuf> = if ca.no_baseline {
        None
    } else {
        match &ca.baseline {
            Some(p) => {
                if !p.is_file() {
                    return Err(format!("baseline {} not found", p.display()).into());
                }
                Some(p.clone())
            }
            None => {
                let implied = Path::new(".goverify").join("baseline.json");
                implied.is_file().then_some(implied)
            }
        }
    };
    let Some(path) = path else {
        return Ok((findings, fps, 0));
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("baseline {}: {e}", path.display()))?;
    let b = goverify_cli::baseline::parse(&bytes)
        .map_err(|e| format!("baseline {}: {e}", path.display()))?;
    let set: std::collections::HashSet<&str> =
        b.entries.iter().map(|e| e.fingerprint.as_str()).collect();
    let mut kept = Vec::new();
    let mut kept_fps = Vec::new();
    let mut suppressed = 0usize;
    for (f, fp) in findings.into_iter().zip(fps) {
        if set.contains(fp.as_str()) {
            suppressed += 1;
        } else {
            kept.push(f);
            kept_fps.push(fp);
        }
    }
    Ok((kept, kept_fps, suppressed))
}

fn run_check(ca: CheckArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let Analyzed {
        program,
        scoped,
        cache_root,
        timings,
    } = analyze_module(&ca)?;
    let t_render = std::time::Instant::now();
    // Filter order (spec §5): scope (already applied) -> diff-base ->
    // fingerprints -> baseline. Fingerprint ordinals are stable across
    // this: diff-base filters whole functions, so identical-sibling
    // groups never split (spec §2).
    let (scoped, diff_base_scoped) = match &ca.diff_base {
        None => (scoped, false),
        Some(git_ref) => {
            let base = diff::checkout_base(Path::new("."), git_ref)?;
            let base_prog = acquire_program(
                &base.module_dir,
                None,
                &ca.patterns,
                cache_root.as_ref(),
                timings,
            )
            .map_err(|e| format!("--diff-base: extracting {git_ref:?}: {e}"))?;
            let changed = diff::changed_funcs(&program, &base_prog);
            let g = goverify_ir::CallGraph::build(&program);
            let keep = g.callers_closure(&changed);
            let kept: Vec<goverify_analysis::Finding> = scoped
                .into_iter()
                .filter(|f| {
                    // A finding whose function isn't in the current
                    // program is kept (conservative: never hide by
                    // accident).
                    program
                        .lookup_func(&f.func)
                        .is_none_or(|id| keep.contains(&id))
                })
                .collect();
            (kept, true)
            // `base` drops here: worktree removed on success and (via
            // Drop) on every error path above.
        }
    };
    let fps = goverify_cli::fingerprint::fingerprints(&scoped);
    let (scoped, fps, suppressed) = apply_baseline(&ca, scoped, fps)?;
    let summary = json::Summary {
        total: scoped.len(),
        suppressed_by_baseline: suppressed,
        diff_base_scoped,
    };
    match ca.format {
        OutputFormat::Human => {
            print!("{}", render::render_findings(&scoped, Path::new(".")));
            if suppressed > 0 {
                println!("goverify: baseline: {suppressed} finding(s) suppressed");
            }
        }
        OutputFormat::Json => print!("{}", json::render_json(&scoped, &fps, &summary)),
        OutputFormat::Sarif => print!("{}", sarif::render_sarif(&scoped, &fps, suppressed)),
    }
    if timings {
        eprintln!(
            "goverify: timing: scope+render {:.2}s",
            t_render.elapsed().as_secs_f64()
        );
    }
    Ok(if scoped.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `baseline write` (spec §4): the identical pipeline as `check`,
/// recording the scoped findings instead of rendering them. Exit 0 on
/// success regardless of finding count — recording findings is the
/// point.
fn run_baseline_write(ca: CheckArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if ca.baseline.is_some() || ca.no_baseline || ca.diff_base.is_some() {
        return Err("baseline write records the full finding set; \
                    --baseline/--no-baseline/--diff-base do not apply"
            .into());
    }
    let a = analyze_module(&ca)?;
    let fps = goverify_cli::fingerprint::fingerprints(&a.scoped);
    let dir = Path::new(".goverify");
    std::fs::create_dir_all(dir)?;
    let path = dir.join("baseline.json");
    std::fs::write(&path, goverify_cli::baseline::render(&a.scoped, &fps))?;
    eprintln!(
        "goverify: baseline: {} finding(s) recorded in {}",
        a.scoped.len(),
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Keep only findings whose function lives in the module rooted at
/// `scope` (an import-path prefix). Deterministic — preserves the input
/// order, which `analyze_full` already fixed.
fn scope_findings(
    findings: &[goverify_analysis::Finding],
    scope: &str,
) -> Vec<goverify_analysis::Finding> {
    findings
        .iter()
        .filter(|f| in_module(&f.func, scope))
        .cloned()
        .collect()
}

/// True iff `func` (an ssa id) belongs to a package under module
/// `module`. A plain function id reads `<import-path>.<symbol>`, but
/// go/ssa emits METHOD ids (via `fn.String()`) as `(pkg.T).M` for a value
/// receiver and `(*pkg.T).M` for a pointer receiver — plus method
/// closures like `(*pkg.T).M$1` — so the module prefix is preceded by a
/// literal `(` and an optional `*`. Strip those first, then require the
/// import path to be `module` itself (id `module.<symbol>` / `(module.T).M`)
/// or rooted under `module/` (`module/pkg.<symbol>`). The boundary byte
/// (`.` or `/`) keeps `example.com/nil` from matching a sibling
/// `example.com/nilextra` — for a method the rest is `.T).M`, so the `.`
/// boundary still holds.
fn in_module(func: &str, module: &str) -> bool {
    // Peel an optional receiver wrapper: `(` then an optional `*`.
    let bare = func
        .strip_prefix('(')
        .map(|r| r.strip_prefix('*').unwrap_or(r))
        .unwrap_or(func);
    match bare.strip_prefix(module) {
        Some(rest) => matches!(rest.as_bytes().first(), Some(b'.') | Some(b'/')),
        None => false,
    }
}

/// The module path from the nearest `go.mod` at or above `start`
/// (mirroring how `go` resolves the module for a directory). `None` when
/// no `go.mod` is found — e.g. `--gvir-dir` runs outside any module,
/// where the caller degrades to an unscoped report (or an explicit
/// `--scope`).
fn module_path(start: &Path) -> Option<String> {
    let mut dir = start.canonicalize().ok()?;
    loop {
        if let Ok(text) = std::fs::read_to_string(dir.join("go.mod"))
            && let Some(m) = parse_module_directive(&text)
        {
            return Some(m);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The `module <path>` directive from go.mod text (first match wins).
/// Requires whitespace after the `module` keyword so `modulefoo` never
/// matches; tolerates surrounding quotes on the path.
fn parse_module_directive(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module")
            && rest.starts_with(char::is_whitespace)
            && let Some(path) = rest.split_whitespace().next()
        {
            return Some(path.trim_matches('"').to_string());
        }
    }
    None
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

/// The user cache root: `$XDG_CACHE_HOME/goverify` or
/// `$HOME/.cache/goverify` (spec §9), created 0700. `None` when neither
/// env var is set — callers degrade (uncached run, or a temp-dir
/// fallback for the sidecar build below).
fn user_cache_root() -> Option<PathBuf> {
    let cache_root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let dir = cache_root.join("goverify");
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    Some(dir)
}

/// Sidecar build cache root: user-scoped (`user_cache_root()`), created
/// 0700. A predictable, world-writable-parent path (bare `temp_dir()`)
/// would let another local user pre-plant a binary for `Sidecar::build`
/// to execute unchecked (CWE-377); temp_dir() is used only as a
/// last-resort fallback.
fn sidecar_build_dir() -> PathBuf {
    match user_cache_root() {
        Some(dir) => dir.join("extractor-bin"),
        None => std::env::temp_dir().join("goverify-extractor-bin"),
    }
}

#[cfg(test)]
mod tests {
    use goverify_analysis::Finding;

    use super::*;

    fn finding(func: &str) -> Finding {
        Finding {
            checker: "nil".to_string(),
            tag: "nil-deref".to_string(),
            func: func.to_string(),
            pos: None,
            message: "m".to_string(),
            trace: Vec::new(),
            model: Vec::new(),
            severity: goverify_analysis::Severity::Error,
        }
    }

    #[test]
    fn in_module_matches_module_and_submodules_only() {
        // Same package as the module root.
        assert!(in_module("example.com/nil.Bad", "example.com/nil"));
        // A package rooted under the module.
        assert!(in_module("example.com/nil/sub.F", "example.com/nil"));
        // A dependency / stdlib package: outside.
        assert!(!in_module("strings.ToUpper", "example.com/nil"));
        // Boundary trap: a sibling module sharing the prefix must NOT match.
        assert!(!in_module("example.com/nilextra.F", "example.com/nil"));
        // Exact prefix with no boundary byte must NOT match.
        assert!(!in_module("example.com/nil", "example.com/nil"));
    }

    #[test]
    fn in_module_matches_go_ssa_method_ids() {
        // go/ssa method ids: value receiver `(pkg.T).M`, pointer receiver
        // `(*pkg.T).M`, method closure `(*pkg.T).M$1` — the leading `(`
        // and optional `*` precede the import path.
        assert!(in_module("(example.com/m.T).M", "example.com/m"));
        assert!(in_module("(*example.com/m.T).M", "example.com/m"));
        assert!(in_module("(*example.com/m.T).M$1", "example.com/m"));
        // A method on a package rooted under the module.
        assert!(in_module("(*example.com/m/sub.T).M", "example.com/m"));
        // Sibling-module method must still be rejected (boundary guard
        // holds after peeling the receiver wrapper).
        assert!(!in_module("(*example.com/hellox.T).M", "example.com/hello"));
    }

    #[test]
    fn scope_findings_drops_out_of_module_entries() {
        let findings = vec![
            finding("example.com/nil.Bad"),
            finding("strings.ToUpper"),
            finding("example.com/nil/sub.Helper"),
            finding("runtime.mapaccess1"),
        ];
        let scoped = scope_findings(&findings, "example.com/nil");
        let names: Vec<&str> = scoped.iter().map(|f| f.func.as_str()).collect();
        assert_eq!(
            names,
            vec!["example.com/nil.Bad", "example.com/nil/sub.Helper"],
            "only in-module findings survive scoping, in input order"
        );
    }

    #[test]
    fn parse_module_directive_reads_the_module_path() {
        let text = "// a comment\nmodule example.com/nil\n\ngo 1.25.10\n";
        assert_eq!(
            parse_module_directive(text).as_deref(),
            Some("example.com/nil")
        );
        // No `module` directive at all.
        assert_eq!(parse_module_directive("go 1.25\n"), None);
        // `module`-prefixed non-directive must not match.
        assert_eq!(parse_module_directive("modulefoo bar\n"), None);
    }
}
