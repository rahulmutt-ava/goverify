// Package pkgnamed instantiates shared.Reduce with the NAMED spelling
// base.U of the element type.
package pkgnamed

import (
	"example.com/aliasgen/base"
	"example.com/aliasgen/shared"
)

func SumNamed(xs []base.U) int {
	return shared.Reduce(xs, func(acc int, x base.U) int { return acc + x.V })
}
