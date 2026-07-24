// Package pkgalias instantiates the SAME shared.Reduce instance as
// pkgnamed, but spells the element type through a cross-package ALIAS
// (Entry = base.U), mirroring os.DirEntry = io/fs.DirEntry.
package pkgalias

import (
	"example.com/aliasgen/base"
	"example.com/aliasgen/shared"
)

// Entry is a cross-package alias of base.U.
type Entry = base.U

func SumAlias(xs []Entry) int {
	return shared.Reduce(xs, func(acc int, x Entry) int { return acc + x.V })
}

// UseIface takes an ANONYMOUS interface whose method signature uses the
// cross-package alias Entry with a named parameter (e). The anonymous
// interface type reaches the type table via this parameter, so its Repr
// renders the method signature verbatim — the alias spelling and param
// name would race unless the extractor canonicalizes anonymous-interface
// method signatures too. (Kept deliberately body-free so the only
// alias-bearing surface added is the interface Type.Repr, not local
// Entry-typed consts/allocs, which render deterministically anyway.)
func UseIface(x interface{ Peek(e Entry) Entry }) { _ = x }
