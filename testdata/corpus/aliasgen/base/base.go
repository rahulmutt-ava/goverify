// Package base declares the single underlying named type that both
// pkgnamed and pkgalias instantiate the shared generic with — one via
// the named spelling base.U, one via a cross-package alias of it.
package base

// U is the underlying named type. Mirrors io/fs.DirEntry in the bbolt
// determinism bug: one *types.Named reachable under two source
// spellings (base.U and pkgalias.Entry).
type U struct{ V int }
