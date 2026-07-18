//! Z3 via the C API, statically linked (parent spec §13). Consumes
//! canonical SMT-LIB2 text — Z3_parse_smtlib2_string, never AST-building
//! from terms (single-lowering rule). All abnormal paths => Unknown.

use std::ffi::{CStr, CString};
use std::ptr;

use z3_sys::*;

use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

pub struct Z3Native {
    // `None` means "poisoned": a prior `reset()` couldn't rebuild a fresh
    // Z3 context/solver. `solve_text` checks this first and degrades to
    // `Unknown` immediately rather than touching a missing context — see
    // `reset()`. Only `new()` is allowed to treat construction failure as
    // fatal (there is no query in flight yet to mark `Unknown`).
    ctx: Option<Z3_context>,
    solver: Option<Z3_solver>,
    limits: SolverLimits,
    identity: String,
}

// One instance per rayon worker; moved, never shared (`Send`, not Sync).
unsafe impl Send for Z3Native {}

/// Replaces Z3's default abort-on-error handler; errors are checked via
/// Z3_get_error_code after each fallible call.
extern "C" fn quiet_error_handler(_ctx: Z3_context, _e: ErrorCode) {}

/// Builds a fresh context/solver pair, or `None` if Z3 itself failed to
/// allocate one (out-of-memory or similar internal failure — z3-sys >= 0.9
/// wraps every Z3-allocated pointer in `Option<NonNull<_>>`, `None` meaning
/// "Z3 internal error"). Fallible, not `.expect()`-ing, because this also
/// runs from `reset()`, which is reachable from every `solve_text` error
/// path — panicking here would breach the "per-query path never panics"
/// invariant. `new()` is the only caller allowed to treat a `None` as fatal.
fn make_ctx_solver(limits: SolverLimits) -> Option<(Z3_context, Z3_solver)> {
    unsafe {
        let cfg = Z3_mk_config()?;
        let ctx = Z3_mk_context(cfg);
        Z3_del_config(cfg);
        let ctx = ctx?;
        Z3_set_error_handler(ctx, Some(quiet_error_handler));
        let solver = Z3_mk_solver(ctx)?;
        Z3_solver_inc_ref(ctx, solver);
        let params = Z3_mk_params(ctx)?;
        Z3_params_inc_ref(ctx, params);
        let timeout = CString::new("timeout").ok()?;
        let timeout_sym = Z3_mk_string_symbol(ctx, timeout.as_ptr())?;
        Z3_params_set_uint(ctx, params, timeout_sym, limits.timeout_ms);
        let max_memory = CString::new("max_memory").ok()?;
        let max_memory_sym = Z3_mk_string_symbol(ctx, max_memory.as_ptr())?;
        Z3_params_set_uint(ctx, params, max_memory_sym, limits.mem_mb);
        Z3_solver_set_params(ctx, solver, params);
        Z3_params_dec_ref(ctx, params);
        Some((ctx, solver))
    }
}

impl Z3Native {
    pub fn new(limits: SolverLimits) -> Z3Native {
        // Construction time is the one place a `None` here is treated as
        // fatal: there is no in-flight query to degrade to `Unknown`, and
        // no prior working context to fall back to either.
        let (ctx, solver) = make_ctx_solver(limits)
            .expect("Z3Native::new: Z3 failed to allocate an initial context/solver");
        let identity = unsafe {
            let v = Z3_get_full_version();
            format!("z3native:{}", CStr::from_ptr(v).to_string_lossy())
        };
        Z3Native {
            ctx: Some(ctx),
            solver: Some(solver),
            limits,
            identity,
        }
    }

    /// Tear down and rebuild after any Z3 error: a poisoned context must
    /// not leak into the next query (parent §11 worker-restart semantics).
    /// If the rebuild itself fails, `self.ctx`/`self.solver` are left as
    /// `None` — poisoned — rather than panicking; every subsequent
    /// `solve_text` call sees that and returns `Unknown` immediately
    /// without touching Z3 again.
    fn reset(&mut self) {
        if let (Some(ctx), Some(solver)) = (self.ctx.take(), self.solver.take()) {
            unsafe {
                Z3_solver_dec_ref(ctx, solver);
                Z3_del_context(ctx);
            }
        }
        if let Some((ctx, solver)) = make_ctx_solver(self.limits) {
            self.ctx = Some(ctx);
            self.solver = Some(solver);
        }
        // else: stay poisoned (both fields already `None` from `.take()`).
    }

    fn ok(&self) -> bool {
        match self.ctx {
            Some(ctx) => unsafe { Z3_get_error_code(ctx) == ErrorCode::Ok },
            None => false,
        }
    }
}

impl Drop for Z3Native {
    fn drop(&mut self) {
        if let (Some(ctx), Some(solver)) = (self.ctx, self.solver) {
            unsafe {
                Z3_solver_dec_ref(ctx, solver);
                Z3_del_context(ctx);
            }
        }
    }
}

const UNKNOWN: QueryOutcome = QueryOutcome {
    result: SatResult::Unknown,
    model: None,
};

impl TextSolver for Z3Native {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn limits(&self) -> SolverLimits {
        self.limits
    }

    fn solve_text(&mut self, canonical: &str) -> QueryOutcome {
        // A poisoned solver (a prior `reset()` couldn't rebuild a Z3
        // context) must never touch Z3 again — degrade to `Unknown`
        // immediately rather than unwrapping a missing context.
        let (Some(ctx), Some(solver)) = (self.ctx, self.solver) else {
            return UNKNOWN;
        };
        // The canonical artifact always ends "(check-sat)\n"; Z3's parser
        // only handles declarations/assertions — check-sat is ours.
        let Some(body) = canonical.strip_suffix("(check-sat)\n") else {
            return UNKNOWN;
        };
        // A body that is empty (or only whitespace) means the caller never
        // gave us anything to check — Z3 would happily call that trivially
        // Sat, but with nothing asserted that's a signal something upstream
        // went wrong, not a real query. Treat it the same as a malformed one.
        if body.trim().is_empty() {
            return UNKNOWN;
        }
        let Ok(cbody) = CString::new(body) else {
            return UNKNOWN;
        };
        unsafe {
            Z3_solver_push(ctx, solver);
            // z3-sys >= 0.9: a parse failure surfaces as `None` here *and*
            // as a non-Ok error code; check both rather than assuming either
            // implies the other.
            let Some(vec) = Z3_parse_smtlib2_string(
                ctx,
                cbody.as_ptr(),
                0,
                ptr::null(),
                ptr::null(),
                0,
                ptr::null(),
                ptr::null(),
            ) else {
                self.reset();
                return UNKNOWN;
            };
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            for i in 0..Z3_ast_vector_size(ctx, vec) {
                let Some(ast) = Z3_ast_vector_get(ctx, vec, i) else {
                    self.reset();
                    return UNKNOWN;
                };
                Z3_solver_assert(ctx, solver, ast);
            }
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            let r = Z3_solver_check(ctx, solver);
            let outcome = match r {
                Z3_L_TRUE => {
                    let text = match Z3_solver_get_model(ctx, solver) {
                        Some(model) if self.ok() => {
                            Z3_model_inc_ref(ctx, model);
                            let s_ptr = Z3_model_to_string(ctx, model);
                            let s = if s_ptr.is_null() {
                                None
                            } else {
                                Some(CStr::from_ptr(s_ptr).to_string_lossy().into_owned())
                            };
                            Z3_model_dec_ref(ctx, model);
                            s
                        }
                        _ => None,
                    };
                    QueryOutcome {
                        result: SatResult::Sat,
                        model: text,
                    }
                }
                Z3_L_FALSE => QueryOutcome {
                    result: SatResult::Unsat,
                    model: None,
                },
                _ => UNKNOWN,
            };
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            Z3_solver_pop(ctx, solver, 1);
            outcome
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SatResult;

    fn solver() -> Z3Native {
        Z3Native::new(SolverLimits::default())
    }

    #[test]
    fn trivial_sat_with_model() {
        let out = solver().solve_text(
            "(set-logic QF_BV)\n\
             (declare-const x (_ BitVec 8))\n\
             (assert (bvult x (_ bv5 8)))\n\
             (check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Sat);
        assert!(out.model.is_some(), "sat must carry a model");
    }

    #[test]
    fn trivial_unsat() {
        let out = solver().solve_text(
            "(set-logic QF_BV)\n\
             (declare-const b Bool)\n\
             (assert (and b (not b)))\n\
             (check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Unsat);
        assert!(out.model.is_none(), "unsat carries no model");
    }

    #[test]
    fn ptr_datatype_queries_work() {
        let out = solver().solve_text(
            "(set-logic ALL)\n\
             (declare-datatypes ((Ptr 0)) (((ptr-nil) (ptr-addr (ptr-addr-val (_ BitVec 64))))))\n\
             (declare-const p0 Ptr)\n\
             (assert ((_ is ptr-nil) p0))\n\
             (check-sat)\n",
        );
        assert_eq!(out.result, SatResult::Sat, "unconstrained ptr can be nil");
    }

    #[test]
    fn malformed_input_is_unknown_not_panic() {
        for bad in [
            "",
            "garbage",
            "(assert (undeclared))\n(check-sat)\n",
            "(check-sat)\n",
        ] {
            let out = solver().solve_text(bad);
            assert_eq!(out.result, SatResult::Unknown, "{bad:?} => Unknown");
        }
    }

    #[test]
    fn solver_survives_a_bad_query() {
        let mut s = solver();
        assert_eq!(s.solve_text("garbage").result, SatResult::Unknown);
        // Context must have been rebuilt: a good query still works.
        let out =
            s.solve_text("(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n");
        assert_eq!(out.result, SatResult::Sat);
    }

    #[test]
    fn identity_names_z3_and_version() {
        let id = solver().identity();
        assert!(id.starts_with("z3native:"), "{id}");
        assert!(id.len() > "z3native:".len(), "{id}");
    }

    #[test]
    fn poisoned_solver_returns_unknown_without_panicking() {
        // Directly construct the poisoned state `reset()` degrades to when
        // Z3 itself fails to rebuild a context (rather than trying to force
        // a real Z3 allocation failure, which isn't practically triggerable
        // from a test): both fields `None`. `solve_text` must see this and
        // return `Unknown` immediately, never panic, and never touch Z3.
        let mut s = solver();
        s.ctx = None;
        s.solver = None;
        let out =
            s.solve_text("(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n");
        assert_eq!(
            out.result,
            SatResult::Unknown,
            "poisoned solver must degrade to Unknown, not panic"
        );
        // Dropping a poisoned `Z3Native` must not double-free or panic either.
        drop(s);
    }

    #[test]
    fn queries_are_independent_across_calls() {
        let mut s = solver();
        let unsat = "(set-logic QF_BV)\n(declare-const b Bool)\n\
                     (assert (and b (not b)))\n(check-sat)\n";
        let sat = "(set-logic QF_BV)\n(declare-const b Bool)\n(assert b)\n(check-sat)\n";
        assert_eq!(s.solve_text(unsat).result, SatResult::Unsat);
        assert_eq!(
            s.solve_text(sat).result,
            SatResult::Sat,
            "no state bleed (push/pop)"
        );
    }
}
