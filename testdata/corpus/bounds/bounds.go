// Package boundscorpus exercises the phase-4 bounds checker: index and
// slice obligations, guards as path conditions, requires propagation.
package boundscorpus

// get requires 0 <= i < len(s).
func get(s []int, i int) int { return s[i] }

// safe guards before indexing — no requires, no finding.
func safe(s []int, i int) int {
	if i < 0 || i >= len(s) {
		return 0
	}
	return s[i]
}

// BadIndex indexes a 3-element slice at 5.
func BadIndex() int {
	s := make([]int, 3)
	return s[5] // want: bounds
}

// BadCall violates get's inferred requires.
func BadCall() int {
	s := make([]int, 3)
	return get(s, 7) // want: bounds
}

// GoodCall respects it.
func GoodCall() int {
	s := make([]int, 3)
	return get(s, 2)
}

// BadSlice slices past capacity.
func BadSlice() []int {
	s := make([]int, 2, 4)
	return s[1:5] // want: bounds
}

// div requires b != 0.
func div(a, b int) int { return a / b }

// BadDiv divides by a constant zero path.
func BadDiv(a int) int {
	b := 0
	return a / b // want: div-zero
}

// GoodDiv guards.
func GoodDiv(a, b int) int {
	if b == 0 {
		return 0
	}
	return a / b
}

// BadNarrow truncates 300 into an int8.
func BadNarrow() int8 {
	x := 300
	return int8(x) // want: overflow
}

// GoodNarrow stays in range.
func GoodNarrow() int8 {
	x := 100
	return int8(x)
}

// count is an opaque uint16 source: its call result is havoc, but its
// SORT is BitVec(16), so ≤65535 is intrinsic — the widening int()
// conversion formerly severed the bound; task 4A's range model now
// carries it through the conversion (C221 exemplar, surgeon.go:78:20 /
// ClearPageElements).
func count() uint16 { return 42 }

func ClearElems(start int) uint16 {
	n := int(count())
	if start < 0 || start >= n {
		return 0
	}
	return uint16(start)
}

// clearOpts mirrors bbolt's surgeryClearPageElementsOptions: the real
// command_surgery.go:268 call site passes cfg.startElementIdx, a STRUCT
// FIELD (CLI-flag-populated), not a bare forwarded parameter — params_only
// fails on the field-access value at this call site, so requires-clause
// propagation stops here (a genuine call-site obligation) instead of
// lifting the precondition further up an unguarded parameter-forwarding
// chain, where it would just self-mask and never actually discharge.
type clearOpts struct{ start int }

// One unbounded and one bounded caller: both stay silent — the
// convert-model discharge (task 4A) landed and carries the bound
// through `int(count())` regardless of the caller's argument (under
// the requires-form fallback (task 4B), only the unbounded one may
// fire).
func ClearElemsUnbounded(o clearOpts) uint16 { return ClearElems(o.start) }

func ClearElemsBounded() uint16 { return ClearElems(3) }
