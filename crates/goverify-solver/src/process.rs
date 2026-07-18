//! Portable text backend (parent spec §8): pipes canonical SMT-LIB2 to
//! any solver binary. Used by --solver-cmd and the differential harness.
//! Every failure — spawn, timeout, garbage output — is Unknown.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::reader::{parse_response, parse_sexpr};
use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

pub struct SmtLib2Process {
    cmd: String,
    limits: SolverLimits,
    identity: String,
}

impl SmtLib2Process {
    pub fn new(cmd: &str, limits: SolverLimits) -> SmtLib2Process {
        let base = Path::new(cmd)
            .file_name()
            .map_or_else(|| cmd.to_string(), |b| b.to_string_lossy().into_owned());
        let version = Command::new(cmd)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()?
                    .lines()
                    .next()
                    .map(str::to_string)
            })
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
        let mut child = Command::new(&self.cmd)
            .arg(file.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(self.limits.timeout_ms) + 250);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    // Deadline elapsed: kill and reap so the child never
                    // becomes a zombie, then degrade to Unknown.
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Err(_) => {
                    // try_wait() itself errored: still attempt to kill/reap
                    // defensively (the child may or may not still exist).
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let mut out = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
        Some(out)
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

    #[test]
    fn sat_unsat_and_model() {
        let sat =
            z3().solve_text("(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n");
        assert_eq!(sat.result, SatResult::Sat);
        assert!(sat.model.is_some());
        let unsat = z3().solve_text(
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
}
