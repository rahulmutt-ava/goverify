//! Z3 via the C API, statically linked (parent spec §13). Consumes
//! canonical SMT-LIB2 text — Z3_parse_smtlib2_string, never AST-building
//! from terms (single-lowering rule). All abnormal paths => Unknown.

use std::ffi::{CStr, CString};
use std::ptr;

use z3_sys::*;

use crate::{QueryOutcome, SatResult, SolverLimits, TextSolver};

pub struct Z3Native {
    ctx: Z3_context,
    solver: Z3_solver,
    limits: SolverLimits,
    identity: String,
}

// One instance per rayon worker; moved, never shared (`Send`, not Sync).
unsafe impl Send for Z3Native {}

/// Replaces Z3's default abort-on-error handler; errors are checked via
/// Z3_get_error_code after each fallible call.
extern "C" fn quiet_error_handler(_ctx: Z3_context, _e: ErrorCode) {}

fn make_ctx_solver(limits: SolverLimits) -> (Z3_context, Z3_solver) {
    // z3-sys >= 0.9 wraps every Z3-allocated pointer in `Option<NonNull<_>>`
    // (null means "Z3 internal error"). These four calls only run at
    // construction/reset time, on hardcoded inputs Z3 cannot reject — unlike
    // `solve_text`'s per-query path, there is no degraded value to fall back
    // to here, so a `None` is treated the same as any other unrecoverable
    // allocator failure (expect, not a query-time error path).
    unsafe {
        let cfg = Z3_mk_config().expect("Z3_mk_config");
        let ctx = Z3_mk_context(cfg).expect("Z3_mk_context");
        Z3_del_config(cfg);
        Z3_set_error_handler(ctx, Some(quiet_error_handler));
        let solver = Z3_mk_solver(ctx).expect("Z3_mk_solver");
        Z3_solver_inc_ref(ctx, solver);
        let params = Z3_mk_params(ctx).expect("Z3_mk_params");
        Z3_params_inc_ref(ctx, params);
        let timeout = CString::new("timeout").expect("static");
        let timeout_sym =
            Z3_mk_string_symbol(ctx, timeout.as_ptr()).expect("Z3_mk_string_symbol(timeout)");
        Z3_params_set_uint(ctx, params, timeout_sym, limits.timeout_ms);
        let max_memory = CString::new("max_memory").expect("static");
        let max_memory_sym =
            Z3_mk_string_symbol(ctx, max_memory.as_ptr()).expect("Z3_mk_string_symbol(max_memory)");
        Z3_params_set_uint(ctx, params, max_memory_sym, limits.mem_mb);
        Z3_solver_set_params(ctx, solver, params);
        Z3_params_dec_ref(ctx, params);
        (ctx, solver)
    }
}

impl Z3Native {
    pub fn new(limits: SolverLimits) -> Z3Native {
        let (ctx, solver) = make_ctx_solver(limits);
        let identity = unsafe {
            let v = Z3_get_full_version();
            format!("z3native:{}", CStr::from_ptr(v).to_string_lossy())
        };
        Z3Native {
            ctx,
            solver,
            limits,
            identity,
        }
    }

    /// Tear down and rebuild after any Z3 error: a poisoned context must
    /// not leak into the next query (parent §11 worker-restart semantics).
    fn reset(&mut self) {
        unsafe {
            Z3_solver_dec_ref(self.ctx, self.solver);
            Z3_del_context(self.ctx);
        }
        let (ctx, solver) = make_ctx_solver(self.limits);
        self.ctx = ctx;
        self.solver = solver;
    }

    fn ok(&self) -> bool {
        unsafe { Z3_get_error_code(self.ctx) == ErrorCode::Ok }
    }
}

impl Drop for Z3Native {
    fn drop(&mut self) {
        unsafe {
            Z3_solver_dec_ref(self.ctx, self.solver);
            Z3_del_context(self.ctx);
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
            Z3_solver_push(self.ctx, self.solver);
            // z3-sys >= 0.9: a parse failure surfaces as `None` here *and*
            // as a non-Ok error code; check both rather than assuming either
            // implies the other.
            let Some(vec) = Z3_parse_smtlib2_string(
                self.ctx,
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
            for i in 0..Z3_ast_vector_size(self.ctx, vec) {
                let Some(ast) = Z3_ast_vector_get(self.ctx, vec, i) else {
                    self.reset();
                    return UNKNOWN;
                };
                Z3_solver_assert(self.ctx, self.solver, ast);
            }
            if !self.ok() {
                self.reset();
                return UNKNOWN;
            }
            let r = Z3_solver_check(self.ctx, self.solver);
            let outcome = match r {
                Z3_L_TRUE => {
                    let text = match Z3_solver_get_model(self.ctx, self.solver) {
                        Some(model) if self.ok() => {
                            Z3_model_inc_ref(self.ctx, model);
                            let s_ptr = Z3_model_to_string(self.ctx, model);
                            let s = if s_ptr.is_null() {
                                None
                            } else {
                                Some(CStr::from_ptr(s_ptr).to_string_lossy().into_owned())
                            };
                            Z3_model_dec_ref(self.ctx, model);
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
            Z3_solver_pop(self.ctx, self.solver, 1);
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
