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
