package annot

// GenPositive pins the documented generic-annotation limitation
// (fix-wave item 1, README's Annotations "Limitations" note): go/ssa
// gives every instantiation of a generic function `Pkg == nil`, so the
// extractor never emits `GenPositive[int]` etc. as functions of their
// own — only the generic ORIGIN is emitted, and the pragma below
// attaches to it via an exact decl_id match (the fan-out branch in
// goverify-ir's `program.rs` never runs: there is nothing to fan out
// to). A call site's callee resolves to the annotation-free
// instantiation, not the origin, so the pragma currently has no effect
// at any call site. `CallsGenPositiveBad` below violates `n >= 1`
// outright and yields NO contract finding today — a bespoke assertion
// in annot_corpus.rs pins this (not a `// want:` pin, since nothing is
// found), to flip once generic-origin fan-out lands (plan follow-up
// queue).
//
//goverify:requires n >= 1
func GenPositive[T any](n int, v T) int {
	_ = v
	return n
}

// CallsGenPositiveBad passes n=0, violating GenPositive's `n >= 1`
// contract outright — the same shape as contract.go's
// `CallsPositiveBad`, except this callee is generic, so (today) it
// produces no finding at all.
func CallsGenPositiveBad() int {
	return GenPositive(0, 1)
}
