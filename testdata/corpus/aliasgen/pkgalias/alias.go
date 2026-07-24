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
