// Package shared holds one generic function that pkgnamed and pkgalias
// both instantiate with the same underlying element type spelled two
// different ways. go/types unifies the aliases into ONE *ssa.Function
// instance; whichever call site type-checks first under concurrent
// packages.Load wins the recorded type-arg spelling. Emitting that raced
// spelling verbatim is the root-invariant bug this fixture guards.
package shared

// Reduce folds xs with f. E is inferred from the caller's slice element
// type, so the instance's type argument is exactly the caller's spelling.
func Reduce[E any](xs []E, f func(acc int, x E) int) int {
	acc := 0
	for _, x := range xs {
		acc = f(acc, x)
	}
	return acc
}
