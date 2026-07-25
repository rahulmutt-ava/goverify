package annot

// Reported is the unsuppressed twin: it pins that the finding exists at
// all.
//
// Adapted from the brief's sketch (a helper `maybe() *int { return nil
// }` called as `p := maybe(); return *p`): the nil checker's
// `obligations` pass only raises a call-result obligation when the
// callee's INFERRED summary carries a proven non-nil-refuting ensures
// clause (nil.rs's `call_result` filter, keyed on a "nonnil" template);
// there is no "always nil" ensures template, so a function that
// unconditionally returns nil earns no summary clause at all and a
// caller dereferencing its result is a silent "havoc'd heap value" —
// never a finding. A manifest local nil — the same shape as
// testdata/corpus/nil/nil.go's LocalNil — is what the checker's
// obligations pass actually flags.
func Reported() int {
	var p *int
	return *p // want: nil-deref
}

// Suppressed pins the ignore rule as a (func, checker) CONJUNCTION, not
// a func-wide blanket: the same statement raises findings from TWO
// different checkers — a manifest nil deref (checker "nil", tag
// "nil-deref") and an out-of-range constant index (checker "bounds",
// tag "bounds" — the BadIndex shape from
// testdata/corpus/bounds/bounds.go). `//goverify:ignore nil` must
// suppress only the nil-deref finding at the CLI layer (asserted in
// annot_integration.rs); the bounds finding must survive. The engine
// itself never consults `ignores` (only the CLI's apply_pragma_ignores
// does), so both findings are still present — and therefore both
// pinned — at this corpus test's layer.
//
//goverify:ignore nil
func Suppressed() int {
	var p *int
	s := make([]int, 3)
	return *p + s[5] // want: nil-deref, bounds
}
