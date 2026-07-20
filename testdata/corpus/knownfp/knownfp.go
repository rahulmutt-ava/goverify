// Package knownfp pins false-positive analyzer behavior found by the
// phase-4 bbolt shakeout (docs/shakeout-phase4-bbolt.md), post the
// 2026-07-20 FP/encoding fix-wave (fixes 1-5, commits 9c9d99f..788f25a;
// re-run + gate results: docs/shakeout-phase4-bbolt.md's "Fix-wave
// re-run" addendum). Two kinds of `// want` live here now:
//
//   - FIXED / FIXED-partially: the pin is a GREEN REGRESSION CASE. The
//     analyzer no longer reports the FP at that line (or reports fewer
//     of the family's original findings); do not "unfix" these — they
//     guard against the fix-wave regressing.
//   - KNOWN-FP(phase-5) and residual KNOWN-FP entries below: the pin is
//     STILL a live false positive. These are the families this wave's
//     narrow, same-function/same-Convert mechanisms don't reach: plain
//     FP/invariant (whole-program construction invariants), FP/requires-
//     lifting (cross-call-boundary postconditions), and re-attributed
//     residuals (call-boundary-indirected unsafe-pointer arithmetic,
//     closure-capture boundaries, SCC-widening) documented in the
//     addendum's gate-1/gate-3 sections — all awaiting their own wave.
//     Do not "fix" these functions — their unsafety-to-the-analyzer is
//     still the point until that wave lands.
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

// FIXED-partially (fix-wave 2026-07-20, task 7 / "fix 5"): formerly
// KNOWN-FP — nil-map range is legal (mechanism group 5, e.g. C038's
// `for size, idSet := range f.freemaps` over a `map[uint64]pidSet`
// field; 3 classes / 8 findings: C038 x5, C362 x1, and — contrary to
// the candidate guessed during planning (C178, cursor.go:403, a
// dominance/widening case per its own verdict row, not this
// mechanism) — C186 x2, a nil-map-index-read sibling of the same
// underlying fact). Verified rather than assumed against a live,
// full-`./...` bbolt re-run: Range/Next already lower to `Op::Havoc`,
// so the obligation was always the FieldAddr+Load chain feeding the
// range header, and fixes 1/2b (non-nil address-of dsts; dominating
// checked-deref assumptions) do subsume it for 4 of the 8 original
// findings — confirmed clean at hashmap.go:126 (C362, `freePageIds`,
// silenced by an earlier same-function deref of the same receiver at
// line 125's `len(f.forwardMap)`), shared.go:113 (C038, `Rollback`,
// likewise silenced by line 91's `t.pending[txid]`), and
// bucket.go:863/942 (C186). The green case now lives in the regular
// nil corpus as `RangeNilMap`/`RangeNilMapCaller` (see nil.go), not
// here.
//
// The remaining 4 of 8 (hashmap.go:237/255/271 — `idsFromFreemaps`,
// `idsFromForwardMap`, `idsFromBackwardMap` — and shared.go:224 —
// `NoSyncReload`; all C038) still report, but NOT for a range-legality
// reason: each has no OTHER same-function deref of its receiver to
// dominate the range header (fix 2b has nothing to anchor to), so
// discharging relies entirely on the function's own self-inferred
// `receiver != nil` requires — and each of these four transitively
// reaches `fmt.Sprintf`/reflect internals (three via a sibling
// `mergeSpans$1` `common.Verify` closure's duplicate-page-ID panic
// path; `NoSyncReload` via the same freelist-wide component once the
// FULL program's call graph is built), which `debug sccs` shows lands
// them in one multi-hundred-member SCC the call graph treats as
// recursive (indirect calls through `func()`-typed closure parameters,
// e.g. `common.Verify`/`sync.Once.Do`, conservatively fan out to every
// nullary closure in the reachable program). That SCC doesn't converge
// within `widen_after` rounds, so it widens to `Summary::havoc()`,
// discarding the self-requires that would otherwise silence these
// four. This is a distinct, deeper mechanism (closure-call-graph
// precision / requires surviving SCC widening) than fix 5's remit and
// is intentionally left unaddressed here; still tracked below as its
// own KNOWN-FP. (Scope-dependent: analyzing `internal/freelist/...`
// alone keeps `NoSyncReload` in a small non-recursive SCC and it stays
// clean there — only the full-module SCC pulls it in; the three
// `idsFrom*Map` functions report under either scope.)

// KNOWN-FP(phase-5, residual after fix 5): FP/encoding — SCC-widening
// swallows a locally-derivable receiver-nonnil requires: a function
// whose only nil-deref site is its own unguarded receiver, with no
// other in-function deref to dominate it, loses that self-requires
// (and so self-reports) whenever it lands in a widened SCC — here,
// because it transitively reaches `fmt.Sprintf`/reflect through a
// debug-only `common.Verify` closure (or, module-wide, other
// `func()`-typed indirection) that the call graph conservatively
// treats as mutually recursive with unrelated fmt/reflect internals.
// Not minimally reproducible outside a real closure-heavy,
// fmt.Sprintf-reaching call graph of bbolt's actual size; left as a
// real, documented shakeout finding rather than a corpus pin.
// Follow-up: call-graph precision for `func()`-typed indirect calls,
// or preserving a widened function's own singleton-derivable requires.

// KNOWN-FN (fix-wave, undischarged): FP/encoding's dual — a FALSE
// NEGATIVE, not a false positive, so no `// want` pin lives here (one
// would fail: the analyzer stays silent where it should report). Same
// "documented, not corpus-pinnable as a want" convention as the
// SCC-widening residual block above — here because the shape is a
// same-function *composition* between fix 2b's checked-deref-dominance
// assumptions and nil.rs's `params_only` filter, not a minimal
// standalone repro.
//
// Shape (`f`/`NamedPtr` exemplar):
//
//	type NamedPtr *T
//
//	func f(p *T) {
//		q := NamedPtr(p)
//		_ = q.x
//		_ = p.y // should want: nil-deref, but is silently discharged
//	}
//
// `q := NamedPtr(p)` lowers to an Assign/ChangeType copy of `p` (same
// underlying pointer, new SSA value `v_q`). The deref of `q.x` fails
// `nil.rs`'s `params_only` filter — `v_q` is its own SMT var, not
// literally `p0` — so it never emits its own requires clause. But
// `q.x` still counts as a deref *site*, so `shared::
// checked_deref_assumptions` grants `¬nil(v_q)` once that site is
// reached. Combined with the Assign defining equality `v_q = p0`
// (`encode_ops`, unconditional), the solver derives `¬nil(p0)` —
// discharging the *unrelated* `p.y` deref's would-be requires clause,
// even though nothing ever actually checked `p` itself. Net effect:
// callers of `f` that pass nil are no longer flagged, though `f`
// genuinely panics on `p.y`.
//
// Fix direction (deferred to the plan owner, not applied by this
// commit): canonicalize a deref subject through same-function Assign/
// ChangeType chains to its root value *before* `params_only` decides
// expressibility, so `q.x`'s deref is recognized as a deref of `p`
// itself and emits its own `nonnil(p0)` requires clause instead of
// being silently absorbed by `checked_deref_assumptions`.

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

// FIXED (interprocedural summaries, 2026-07-20): formerly
// KNOWN-FP(phase-5) FP/invariant — the FP this pin actually pinned was
// never rootNode's own field-level nilness (still untracked field-by-
// field, so that half of the original description remains true in
// spirit) but `Depth`'s own precondition ¬is_nil(b) failing to
// discharge at UseBucket's call site because `newBucket`'s return value
// was havoc'd there. `newBucket`'s inferred ensures (unconditional
// ¬is_nil(r0): its sole return path always yields a freshly allocated
// `*bucket`) is now asserted at that call site, discharging the
// propagated call-site obligation (exemplars C002a, C017b: bbolt's
// `InBucket`/`rootNode` fields set at every `newBucket`/`openBucket`
// call before the `*Bucket` escapes). Kept as the green regression
// case.
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
	return b.Depth()
}

// FIXED (interprocedural summaries, 2026-07-20): formerly
// KNOWN-FP(phase-5) FP/invariant — err==nil ⇒ result!=nil paired-
// return contract of a callee, checked locally within the same
// function immediately after the call: `newHandle`'s inferred
// correlation ensures (is_nil(err) ⇒ ¬is_nil(h), validated under the
// Go-idiom rule for the errConstructFailed sentinel returns across all
// three early guards) is now asserted at UseHandle's call site; the
// `err != nil` guard then renders is_nil(h) unreachable at the
// handleID call, discharging the propagated nil-deref requirement
// (exemplars C025, C004a: `bolt.Open`'s `err == nil ⇒ *DB != nil`
// contract, checked immediately before `db.View`). Kept as the green
// regression case.
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
	return handleID(h)
}

// --- FP/requires-lifting canonical shapes (primary phase-5 flip targets) ---

// FIXED (interprocedural summaries, 2026-07-20): formerly
// KNOWN-FP(phase-5) FP/requires-lifting — `beginTx`'s inferred
// correlation ensures (is_nil(err) ⇒ ¬is_nil(tx), validated under the
// Go-idiom rule for the errConstructFailed sentinel returns) is now
// asserted at both call sites in Compact; each `err != nil` guard then
// renders is_nil(tx) unreachable at the commitFn call, discharging the
// propagated nil-deref requirement. Kept as the green regression case
// for exemplar C009c.
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
	return commitFn(tx)
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
