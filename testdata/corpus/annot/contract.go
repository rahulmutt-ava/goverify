// Package annot is the phase-6 annotation-language corpus module.
package annot

//goverify:requires n >= 1
func Positive(n int) int { return n }

func CallsPositiveBad() int {
	return Positive(0) // want: contract
}

func CallsPositiveOK() int {
	return Positive(2)
}

// ForwardPositive pins current semantics for fix-wave item 3: annotated
// requires do NOT lift into callers the way a checker's own INFERRED
// requires would (via propagate_requires) — annotating Positive
// obliges its DIRECT callers to establish `n >= 1` themselves.
// ForwardPositive merely forwards its own unchecked `n`, so THIS call
// is itself a violating call site (n is unconstrained, so 0 is a
// reachable value) — the finding lands here, not at any of
// ForwardPositive's own callers. Annotating ForwardPositive too (to
// chain the contract) is the fix a caller of this function would need;
// this fixture deliberately leaves it unannotated to pin the gap.
func ForwardPositive(n int) int {
	return Positive(n) // want: contract
}
