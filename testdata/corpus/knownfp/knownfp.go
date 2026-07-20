// Package knownfp pins CURRENT false-positive analyzer behavior found
// by the phase-4 bbolt shakeout (docs/shakeout-phase4-bbolt.md). Every
// want here is a KNOWN FP: phase 5 must make it disappear and flip the
// pin. Do not "fix" these functions — their unsafety-to-the-analyzer is
// the point.
//
// One minimal repro per FP mechanism group (not per class — the bbolt
// shakeout triaged 968 confirmed-FP findings into 438 classes, far too
// many for one-pin-per-class). Some mechanism groups have no repro
// below because they weren't minimally reproducible outside bbolt's own
// context (see docs/shakeout-phase4-bbolt.md's "Corpus pins" subsection
// for the full mapping and the reasons each was dropped).
package knownfp

import "unsafe"

// --- FP/encoding mechanism groups (report §"FP/encoding findings") ---

// Mechanism group 1 (same-function dominating check not carried
// forward, e.g. C015a's two `if b.buckets != nil` checks around an
// intervening `b.Cursor()`/`c.seek(name)`) is NOT pinned here: every
// minimal repro tried (a receiver from a branching constructor, a bare
// field/pointer-chain `!= nil` comparison checked twice with an
// intervening call, both flat map-field and two-hop pointer-field
// forms) produced no finding at either check — this checker snapshot
// does not appear to attach a nil-deref obligation to a bare `!= nil`
// comparison read in isolation, only to reads that flow into a further
// call, index, or arithmetic operation. Recorded as not minimally
// reproducible (see the report's "Corpus pins" subsection).

// FIXED (fix-wave 2026-07-20, fix 1): formerly KNOWN-FP — address-of
// stack-local / composite-literal / slice-element / value-typed field
// (mechanism group 2, 48 classes / 111 findings; exemplars C009b,
// C002b). Alloc/FieldAddr/IndexAddr dsts now carry a non-nil fact, so
// calling the promoted (&o.baseOptions).Validate() on a stack-local
// no longer reports. Kept as the green regression case.
type baseOptions struct{ path string }

func (o *baseOptions) Validate() error {
	if o.path == "" {
		return errPathRequired
	}
	return nil
}

type copyOptions struct{ baseOptions }

func BuildSurgeryOptions() error {
	var o copyOptions
	return o.Validate()
}

var errPathRequired = &constructError{}

// KNOWN-FP(phase-5): FP/requires-lifting — residual, re-attributed after
// fix-wave fix 3 (task 5, 2026-07-20). Fix 3 (encode.rs `op_def`'s
// `Convert` arm) now asserts non-nil on `elemAt`'s `(*elem)(...)` dst
// itself, so the nil-deref half of mechanism group 3 is closed (see
// `PageAt`/`page.Count` below, the green case for that half). This
// finding persists as a `bounds` obligation for a different reason: the
// checker has no fact relating `&buf[0]`'s (length-1-implying) address
// back to `buf`'s own `len`, so `i`'s in-bounds-ness against `buf` can't
// be derived through the `uintptr(base)+i*size` conversion chain — the
// `&buf[0]` length / index-conversion shape, requires-lifting territory
// (C101-family: bbolt's `LeafPageElement`/`BranchPageElement`, same
// family as `BranchElemOffset` below). Exemplars C001, C057, C033.
type elem struct{ v uint32 }

func elemAt(base unsafe.Pointer, i int) *elem {
	return (*elem)(unsafe.Pointer(uintptr(base) + uintptr(i)*unsafe.Sizeof(elem{})))
}

func ReadElem(buf []byte, i int) uint32 {
	e := elemAt(unsafe.Pointer(&buf[0]), i) // want: bounds
	return e.v
}

// Mechanism group 4 (stdlib constructor documented never-nil, e.g.
// `flag.NewFlagSet` in C003) is NOT pinned here: reproducing it needs a
// genuine external-package constructor (the whole point is that the
// analyzer treats an *opaque* dependency's return as generically
// nilable), and pulling in a real stdlib package such as "flag" drags
// its entire transitive closure into the corpus's whole-DAG analysis —
// confirmed empirically to blow the analysis past 30 minutes for this
// single-package addition, unacceptable for a blocking-tier corpus
// test. Recorded as not minimally reproducible (see the report's
// "Corpus pins" subsection).

// Mechanism group 5 (nil-map range is legal, e.g. C038's `for size,
// idSet := range f.freemaps` over a `map[uint64]pidSet` field) is NOT
// pinned here: both a bare-map-field and a two-hop pointer-field form
// were tried (a receiver from a branching constructor, ranging over its
// map field/its pointer-field's map field) and neither produced a
// finding — consistent with mechanism group 1's finding above that a
// field read used only for a nil-safe operation (here, `range`, which
// never dereferences a nil map) doesn't register as an obligation site
// in this checker snapshot. Recorded as not minimally reproducible (see
// the report's "Corpus pins" subsection).

// KNOWN-FP(phase-5): FP/encoding — other/miscellaneous encoding gap:
// a reslice is only reached after an in-bounds length check on the same
// slice proves the reslice bound safe, but the analyzer doesn't carry
// that same-function guard fact into the reslice (mechanism group 6, 29
// classes / 41 findings; exemplar C062: cmd/bbolt/main.go's
// `args[1:]` reslice guarded by an earlier `len(args)==0` return).
func Tail(args []string) []string {
	if len(args) == 0 {
		return nil
	}
	return args[1:] // want: bounds
}

// --- FP/invariant dominant mechanisms ---

// KNOWN-FP(phase-5): FP/invariant — field set at every construction
// site before exposure: the only constructor for `bucket` sets
// `rootNode` immediately after allocation, before the value is ever
// exposed to a caller, so no live `*bucket` can have a nil `rootNode`;
// the analyzer only tracks local facts and has no whole-program view of
// every construction site, so it flags the dereference anyway
// (exemplars C002a, C017b: bbolt's `InBucket`/`rootNode` fields set at
// every `newBucket`/`openBucket` call before the `*Bucket` escapes).
type node struct{ depth int }

type bucket struct{ rootNode *node }

func (b *bucket) Depth() int {
	return b.rootNode.depth
}

// newBucket is the sole constructor and always sets rootNode right
// after allocation, before any *bucket is ever exposed to a caller —
// but the checker doesn't track that field-level postcondition through
// newBucket's return, so UseBucket's call to Depth() is flagged as if
// rootNode could still be nil.
func newBucket() *bucket {
	b := &bucket{}
	b.rootNode = &node{}
	return b
}

func UseBucket() int {
	b := newBucket()
	return b.Depth() // want: nil-deref
}

// KNOWN-FP(phase-5): FP/invariant — err==nil ⇒ result!=nil paired-
// return contract of a callee, checked locally within the same
// function immediately after the call: `newHandle`'s only error path
// returns `(nil, err)` and its only success path returns a freshly
// allocated non-nil value, so `err == nil` implies `h != nil` right
// where it's checked; the analyzer doesn't encode this stdlib-style
// paired-return contract and still treats `h` as possibly nil past the
// guard (exemplars C025, C004a: `bolt.Open`'s `err == nil ⇒ *DB !=
// nil` contract, checked immediately before `db.View`).
type handleStats struct{ id int }

type handle struct{ stats *handleStats }

// handleID routes the read through a separate accessor (mirroring
// bucket/Depth above) so the postcondition gap shows up at a genuine
// call boundary rather than an inline field read.
func handleID(h *handle) int { return h.stats.id }

// newHandle mirrors bolt.Open's shape: several independent early-return
// error paths (not a single if/else) before the sole success path,
// closer to the multi-guard real constructor than a trivial one-branch
// stand-in.
func newHandle(a, b, c bool) (*handle, error) {
	if a {
		return nil, errConstructFailed
	}
	if b {
		return nil, errConstructFailed
	}
	if c {
		return nil, errConstructFailed
	}
	return &handle{stats: &handleStats{id: 1}}, nil
}

var errConstructFailed = &constructError{}

type constructError struct{}

func (*constructError) Error() string { return "construct failed" }

func UseHandle(a, b, c bool) int {
	h, err := newHandle(a, b, c)
	if err != nil {
		return -1
	}
	return handleID(h) // want: nil-deref
}

// --- FP/requires-lifting canonical shapes (primary phase-5 flip targets) ---

// KNOWN-FP(phase-5): FP/requires-lifting — err==nil ⇒ result!=nil
// postcondition not lifted across a call boundary, and re-derived
// across a SECOND guarded reassignment of the same variable: `beginTx`
// is called twice, each immediately guarded by its own `err != nil`
// check, mirroring compact.go's `dst.Begin(true)` called once up front
// and again on reassignment inside the loop before `tx.Commit()`
// (exemplar C009c). `commitTx` never sees either guard — its own
// `tx.in.id` read is analyzed as a standalone summary that doesn't
// inherit either call site's proof.
type txnStats struct{ id int }

type txn struct{ in *txnStats }

func commitTx(tx *txn) int { return tx.in.id }

func beginTx(a, b bool) (*txn, error) {
	if a {
		return nil, errConstructFailed
	}
	if b {
		return nil, errConstructFailed
	}
	return &txn{in: &txnStats{id: 1}}, nil
}

func Compact(a, b bool) error {
	tx, err := beginTx(a, false)
	if err != nil {
		return err
	}
	tx, err = beginTx(false, b)
	if err != nil {
		return err
	}
	return commitFn(tx) // want: nil-deref
}

func commitFn(tx *txn) error {
	_ = commitTx(tx)
	return nil
}

// FIXED (fix-wave 2026-07-20, fix 2b): formerly KNOWN-FP, filed under
// FP/requires-lifting (exemplars C031a, C053b: bbolt's
// `tx.rollback`/`(*node).split`) because the callee's own nil-receiver
// check looked like it needed the caller's fact lifted INTO its
// analysis. But the obligation this pin actually pins is raised at the
// CALL SITE inside `Use`, not inside `closeSession`'s body — and
// `Use`'s own prior dereference `_ = s.db` strictly dominates that call
// in the same function, which is exactly fix 2b's mechanism (no
// interprocedural lifting needed at all: the checked-deref assumption
// discharges the call-site obligation directly). Kept as the green
// regression case; still misfiled as its own class in
// docs/shakeout-phase4-bbolt.md's C031a/C053b rows since the true
// remaining "requires-lifting" instances lift facts across the
// CALLEE's boundary, which this one never needed.
type session struct{ db *int }

func maybeSession(nilPath bool) *session {
	if nilPath {
		return nil
	}
	return &session{}
}

func closeSession(s *session) bool { return s.db == nil }

func Use(nilPath bool) bool {
	s := maybeSession(nilPath)
	_ = s.db
	return closeSession(s)
}

// KNOWN-FP(phase-5): FP/requires-lifting — length-guard established by
// the caller not lifted across the call boundary into the callee's
// index: every caller of `tail` guards `len(names) == 0` immediately
// before calling it, so `names[0]` can never be out of bounds inside
// `tail` — but the analyzer analyzes `tail` with an unconstrained
// `names` parameter instead of substituting the caller's proven
// length bound (exemplars C403, C218a: bbolt's `findLastBucket`,
// guarded by an identical `len(buckets)==0` check at both call sites
// before `bucketNames[0]`/`bucketNames[1:]`).
func tail(names []string) string {
	return names[0] // want: bounds
}

func First(names []string) string {
	if len(names) == 0 {
		return ""
	}
	return tail(names)
}

// KNOWN-FP(phase-5): FP/requires-lifting — bound-derived obligation
// kept local instead of lifted to the caller: `idx`'s parameter type is
// `uint16`, so `int(idx)` is provably in [0, 65535] at every call site
// by Go's conversion semantics alone, and the constant 16-byte element
// size can never make the product overflow a uintptr on any real
// platform — but the analyzer's generic summary for `elemOffset`
// treats its `n int` parameter as an unconstrained int and never
// propagates the caller-side uint16 domain fact through the `int(idx)`
// conversion (exemplar C101: bbolt's `LeafPageElement`/
// `BranchPageElement`, whose `uint16` index is converted to `int`
// before reaching `UnsafeIndex`'s unconstrained `n int`).
func elemOffset(base uintptr, elemSize uintptr, n int) uintptr {
	return base + uintptr(n)*elemSize
}

func BranchElemOffset(base uintptr, idx uint16) uintptr {
	return elemOffset(base, 16, int(idx)) // want: overflow
}

// fix-wave fix 3 (green): the nil-deref manifestation of mechanism
// group 3 — a method with an inferred non-nil-receiver requirement is
// called on a pointer minted from uintptr arithmetic (bbolt C001's
// db.page/LeafPageElement shape). Verified: `p.Count()` carries NO
// nil-deref finding (fix 3 closes exactly that half of mechanism 3).
//
// KNOWN-FP(phase-5): FP/requires-lifting — this repro reuses the same
// `&buf[0]` idiom as `ReadElem`/`elemAt` above, so it also inherits
// that unrelated residual: the checker has no fact relating `&buf[0]`'s
// address back to `buf`'s own `len`, so it raises its own `bounds`
// obligation on the index independent of anything fix 3 touches (same
// C101-family requires-lifting gap, not a fix-3 regression).
type page struct{ count uint32 }

func (p *page) Count() uint32 { return p.count }

func PageAt(buf []byte, off uintptr) uint32 {
	p := (*page)(unsafe.Pointer(uintptr(unsafe.Pointer(&buf[0])) + off)) // want: bounds
	return p.Count()
}
