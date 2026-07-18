//! Portable text backend (parent spec §8): pipes canonical SMT-LIB2 to
//! any solver binary. Used by --solver-cmd and the differential harness.
//! Every failure — spawn, timeout, garbage output — is Unknown.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::reader::{parse_response, parse_sexpr};
use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

/// Deadline for the one-shot `<cmd> --version` probe in `new()`. A hung
/// or misbehaving binary must not hang construction forever; 5s is
/// generous for a version flag (the actual query path has its own,
/// caller-supplied `SolverLimits::timeout_ms`).
const VERSION_PROBE_TIMEOUT_MS: u64 = 5_000;

/// Grace window for `run_with_deadline`'s stdout-draining thread to
/// deliver its result once the child has already exited (normally or
/// via kill+wait). The child's exit closes its end of the pipe, so the
/// thread's `read_to_string` reaches EOF and sends almost immediately;
/// this bound only guards against a misbehaving grandchild that
/// inherited the write end and kept it open — the expected path never
/// gets close to it.
const DRAIN_JOIN_GRACE_MS: u64 = 2_000;

pub struct SmtLib2Process {
    cmd: String,
    limits: SolverLimits,
    identity: String,
}

/// Runs `<cmd> <args>`, capturing stdout as text, under a hard
/// `deadline_ms`. `None` on any failure: spawn failure, non-UTF8
/// output, or an elapsed deadline (the child is killed and reaped
/// either way — never left as a zombie, and the caller never blocks
/// past `deadline_ms` + `DRAIN_JOIN_GRACE_MS`).
///
/// Stdout is drained on a dedicated thread concurrently with the
/// `try_wait` poll loop below, not read after the child exits: a child
/// that writes more than the OS pipe buffer (commonly ~64 KiB) before
/// exiting blocks on its own `write()` once the pipe fills, if nobody
/// is reading — the poll loop would then burn the *entire* deadline
/// waiting on a child that isn't hung, it's just full, and a correct
/// answer would be silently downgraded to `Unknown`. Reading as bytes
/// arrive means `try_wait` only ever measures real solver compute time.
fn run_with_deadline(cmd: &str, args: &[&str], deadline_ms: u64) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let res = std::io::Read::read_to_string(&mut stdout, &mut out);
        let _ = tx.send(res.map(|_| out));
    });

    if !wait_with_deadline(&mut child, deadline_ms) {
        return None;
    }
    rx.recv_timeout(Duration::from_millis(DRAIN_JOIN_GRACE_MS))
        .ok()?
        .ok()
}

/// Polls `child` until it exits or `deadline_ms` elapses; on timeout (or
/// a `try_wait` error) kills and reaps it so it never becomes a zombie.
/// Returns `true` iff the child exited on its own within the deadline.
fn wait_with_deadline(child: &mut Child, deadline_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                // Deadline elapsed, or try_wait() itself errored: kill
                // and reap defensively so the child is never left as a
                // zombie, then degrade to Unknown.
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

impl SmtLib2Process {
    pub fn new(cmd: &str, limits: SolverLimits) -> SmtLib2Process {
        let base = Path::new(cmd)
            .file_name()
            .map_or_else(|| cmd.to_string(), |b| b.to_string_lossy().into_owned());
        let version = run_with_deadline(cmd, &["--version"], VERSION_PROBE_TIMEOUT_MS)
            .and_then(|s| s.lines().next().map(str::to_string))
            .unwrap_or_else(|| "unknown-version".into());
        SmtLib2Process {
            cmd: cmd.to_string(),
            limits,
            identity: format!("process:{base}:{version}"),
        }
    }

    fn run(&self, canonical: &str) -> Option<String> {
        let mut file = tempfile::Builder::new()
            .prefix("goverify-query-")
            .suffix(".smt2")
            .tempfile()
            .ok()?;
        file.write_all(canonical.as_bytes()).ok()?;
        file.write_all(b"(get-model)\n").ok()?;
        file.flush().ok()?;
        let path = file.path().to_str()?;
        run_with_deadline(&self.cmd, &[path], u64::from(self.limits.timeout_ms) + 250)
    }
}

impl TextSolver for SmtLib2Process {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn limits(&self) -> SolverLimits {
        self.limits
    }

    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        let Some(out) = self.run(canonical) else {
            return QueryOutcome {
                result: SatResult::Unknown,
                model: None,
            };
        };
        let mut lines = out.splitn(2, '\n');
        let result = parse_response(lines.next().unwrap_or(""));
        let model = if result == SatResult::Sat {
            lines.next().and_then(|rest| {
                parse_sexpr(rest)
                    .ok()
                    .map(|(_, n)| rest[..n].trim().to_string())
            })
        } else {
            None
        };
        QueryOutcome { result, model }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SatResult, SolverLimits};

    fn z3() -> SmtLib2Process {
        SmtLib2Process::new("z3", SolverLimits::default())
    }

    /// Generous limits: these queries are trivial; a CI-box hiccup under
    /// parallel load must not turn a verdict into Unknown and flake the
    /// test (this one has flaked at the default 100ms under load, unlike
    /// the plain `z3()` helper's default-limits instance used elsewhere
    /// in this module where a flake was never observed).
    fn z3_generous() -> SmtLib2Process {
        SmtLib2Process::new(
            "z3",
            SolverLimits {
                timeout_ms: 5_000,
                mem_mb: 1024,
            },
        )
    }

    #[test]
    fn sat_unsat_and_model() {
        let sat = z3_generous()
            .solve_text("(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n");
        assert_eq!(sat.result, SatResult::Sat);
        assert!(sat.model.is_some());
        let unsat = z3_generous().solve_text(
            "(set-logic QF_BV)\n(declare-const b Bool)\n(assert (and b (not b)))\n(check-sat)\n",
        );
        assert_eq!(unsat.result, SatResult::Unsat);
        assert!(unsat.model.is_none());
    }

    #[test]
    fn garbage_and_missing_binary_are_unknown() {
        assert_eq!(z3().solve_text("garbage").result, SatResult::Unknown);
        let mut missing = SmtLib2Process::new("goverify-no-such-solver", SolverLimits::default());
        assert_eq!(
            missing.solve_text("(check-sat)\n").result,
            SatResult::Unknown
        );
    }

    #[test]
    fn identity_includes_version() {
        let id = z3().identity();
        assert!(id.starts_with("process:z3:"), "{id}");
        assert!(id.contains("4.1"), "version line captured: {id}");
    }

    /// Writes an executable shell script with the given body to a temp
    /// file (mode 0o755) and returns the handle (kept alive by the
    /// caller for the script's lifetime).
    #[cfg(unix)]
    fn fake_solver_script(body: &str) -> tempfile::NamedTempFile {
        use std::os::unix::fs::PermissionsExt;

        let mut f = tempfile::Builder::new()
            .prefix("goverify-fake-solver-")
            .tempfile()
            .expect("create fake solver script");
        f.write_all(body.as_bytes())
            .expect("write fake solver script");
        f.flush().expect("flush fake solver script");
        let mut perms = f
            .as_file()
            .metadata()
            .expect("stat fake solver script")
            .permissions();
        perms.set_mode(0o755);
        f.as_file()
            .set_permissions(perms)
            .expect("chmod fake solver script");
        f
    }

    /// Regression test for a reviewer-flagged gap: stdout used to be
    /// read only *after* `try_wait()` reported the child had exited. A
    /// solver that decides `sat` quickly but then writes a model larger
    /// than the OS pipe buffer (commonly ~64 KiB) blocks on `write()`
    /// once the pipe fills if nobody is draining it concurrently — the
    /// poll loop would see the child as still running for the entire
    /// deadline, kill it, and downgrade a correct `sat` answer to
    /// `Unknown`. This fake "solver" reproduces exactly that shape (a
    /// quick `sat` line immediately followed by >64 KiB of output) via
    /// the real public `SmtLib2Process` API, matching how `(get-model)`
    /// can make a genuine z3 response large. (The payload comes *after*
    /// the verdict line here — the realistic order for this protocol,
    /// since we always append `(get-model)`, whereas the write-blocks
    /// deadlock this guards against does not depend on ordering.)
    #[cfg(unix)]
    #[test]
    fn solve_text_drains_a_chatty_solver_without_downgrading_to_unknown() {
        let script = fake_solver_script("#!/bin/sh\nprintf 'sat\\n'\nyes x | head -c 100000\n");
        let mut proc = SmtLib2Process::new(
            script.path().to_str().expect("utf8 temp path"),
            SolverLimits {
                timeout_ms: 2_000,
                mem_mb: 1024,
            },
        );
        let out = proc.solve_text("(check-sat)\n");
        assert_eq!(
            out.result,
            SatResult::Sat,
            ">64KiB of trailing output must not time out a decided verdict"
        );
    }

    /// Regression/robustness test for the shared timeout helper both
    /// `run()` (query solving) and `new()` (the `--version` probe) go
    /// through: a child that never exits on its own must still be
    /// killed, reaped, and reported as a timeout well within the given
    /// deadline, never hanging the caller. Uses `run_with_deadline`
    /// directly with a short deadline rather than waiting out the real
    /// `VERSION_PROBE_TIMEOUT_MS` (5s) `new()` uses internally, so this
    /// stays fast and non-flaky while still exercising the exact same
    /// kill/reap code path `new()`'s probe relies on.
    #[cfg(unix)]
    #[test]
    fn run_with_deadline_kills_a_hung_child_promptly() {
        let script = fake_solver_script("#!/bin/sh\nsleep 5\necho sat\n");
        let start = std::time::Instant::now();
        let out = run_with_deadline(script.path().to_str().expect("utf8 temp path"), &[], 50);
        assert!(out.is_none(), "a hung child must degrade to None (Unknown)");
        assert!(
            start.elapsed() < Duration::from_millis(2_000),
            "must not block anywhere near the child's 5s sleep, took {:?}",
            start.elapsed()
        );
    }
}
