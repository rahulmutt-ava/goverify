# Phase 6: Annotation language (core) — design

Date: 2026-07-25
Parent spec: `2026-07-16-goverify-design.md` §6 (annotation language), §15
item 6. This wave implements the **core** slice: `requires`, `ensures`,
and `ignore` pragmas, the `unverified-annotation` finding, error findings
on bad annotations, and the severity tier they force into existence.

## 1. Goal and scope

Give users a way to state function contracts in source that (a) kills
in-body false positives by assuming the contract at entry, (b) moves the
obligation to call sites where it is checked, and (c) is never silently
ignored — every annotation either takes effect or produces a finding.

**In scope:** `//goverify:requires`, `//goverify:ensures`,
`//goverify:ignore <checker>`; the annotation expression parser in
`goverify-spec`; engine merge of annotated clauses; best-effort ensures
verification; the `contract`, `bad-annotation`, and
`unverified-annotation` finding classes; a `severity` field on findings
with `--deny warnings`; pragma-ignore suppression sharing the baseline
suppression surface; fuzz target #7.

**Deferred to a 6b wave (see §11):** `effects` and `pure` pragmas (no
consumer until phase-7 concurrency checkers), field selection in
expressions (needs interface-level heap modeling), and third-party
`.gvspec` override files (needs an on-disk spec format).

**Not building:** a config file. The only CI knob is the `--deny` flag.

## 2. Surface syntax

Pragmas are `//goverify:` doc-comment lines on function/method
declarations. The extractor already captures them verbatim into `.gvir`
`Pragma` messages (`decl_id`, `text`, `pos`), sorted by
`(decl_id, text)`; **no extractor changes are required** beyond fixtures.

```go
//goverify:requires p != nil && n >= 0
//goverify:ensures err == nil ==> ret != nil
//goverify:ignore nil-deref        // trailing // comment allowed
```

- Multiple pragmas per declaration are allowed. Repeated
  `requires`/`ensures` lines become separate clauses.
- A trailing `//`-comment on the pragma line is stripped before parsing.
- `effects`, `pure`, and any unrecognized directive produce a
  `bad-annotation` **error** finding ("directive not supported") at the
  pragma position. Nothing is silently ignored. The project is
  greenfield on annotations, so this breaks no one.

### Expression grammar

Hand-rolled recursive descent in `goverify-spec` (untrusted input,
parent spec §11):

```
expr    := or ("==>" expr)?               ; implication, right-assoc, lowest
or      := and ("||" and)*
and     := cmp ("&&" cmp)*
cmp     := unary (("=="|"!="|"<"|"<="|">"|">=") unary)?   ; non-associative
unary   := "!" unary | primary
primary := ident | "len" "(" expr ")" | "cap" "(" expr ")"
         | "old" "(" expr ")" | intlit | "-" intlit
         | "true" | "false" | "nil" | "(" expr ")"
```

No arithmetic operators in this wave (the parent spec's subset lists
comparisons, boolean operators, `len`/`cap`, and literals; arithmetic
can be added compatibly later).

**Field selection (`p.buf`) is parsed but rejected** with an actionable
`bad-annotation` error ("field selection not supported in v1").
Interface variables (`p<i>`/`r<i>`) denote whole values today; a term
for a field of a parameter requires interface-level heap modeling that
summaries deliberately do not have yet (parent spec's "reachable heap
roots" is phase-7 effects territory). Parsing the syntax reserves it so
later support is not a grammar break.

## 3. Compilation pipeline (`goverify-spec`)

`goverify-spec` (today a stub) becomes the annotation compiler with
three stages, each total (returns `Result`, never panics):

1. **Parse** — pragma-line split (directive + payload), then the
   expression parser. Depth and node-count caps reject pathological
   input.
2. **Resolve** — identifiers bind against the annotated function's
   `.gvir` signature: receiver name, parameter names, and named result
   names map to `IfaceVar::Param(i)` / `IfaceVar::Result(i)` (receiver
   is param 0, the existing convention). For **unnamed results**: `ret`
   when there is exactly one result, `ret0..retN` otherwise; declared
   names always win on collision. Unknown identifier → `bad-annotation`
   error.
3. **Lower** — to the existing `Term` language: names →
   `Var(iface_var_name(v))`; comparisons choose signed/unsigned `BvCmp`
   from the Go type of the operands; `len`/`cap` → `GoSeq`
   `dt_get(seq-len/seq-cap)`; `nil` → the nil-pointer term; `==>` →
   `Implies`. Sort checking is structural at lowering time: `p != nil`
   on an integer parameter, `len` of a non-sequence, or a
   non-boolean-sorted top-level expression → `bad-annotation` error.

In `ensures`, bare parameter names denote **entry values** (which is
what `p<i>` already means in summary formulas); `old(x)` is accepted as
an explicit synonym for spec compatibility, at zero machinery cost.
`old()` in a `requires` is also accepted (trivially the identity).

The compiler's public output per function:
`CompiledAnnotations { requires: Vec<Clause>, ensures: Vec<Clause>,
ignores: Vec<String /* checker name */>, findings: Vec<Finding> }` —
all vectors deterministically ordered (pragmas arrive pre-sorted from
`.gvir`).

Crate dependencies: `goverify-spec` → `goverify-solver` (Term) and the
gvir/type-table types it resolves against. Exact import shape settled at
plan time against `ARCHITECTURE.md`'s edges.

## 4. Engine integration

**Per-clause provenance.** `Clause` gains
`provenance: Inferred | Annotated`. `Summary`'s summary-level field
keeps marking havoc summaries. The scc-cache clause codec grows the new
field → scc cache version bump (natural rotation).

**Compilation point.** The engine compiles each function's pragmas once,
at summary construction. `bad-annotation` findings flow into the normal
finding stream; a function whose annotation is bad is analyzed **as if
unannotated** (plus the error finding) — degrade, never die.

**Annotated requires — the contract split:**

1. **Assumed at entry** of the function's own encoding, so in-body
   obligations covered by the contract discharge. This is the
   FP-killing payoff and mirrors what requires-lifting already does for
   inferred clauses.
2. **Checked at every call site** via the existing
   `instantiate_requires` — no new call-site machinery. A violated
   annotated requires reports as a **`contract`** finding (error
   severity) at the call site, attributed to the *caller*, with the
   annotation text quoted in the message (position-free, so
   fingerprint-safe).

Annotated clauses that duplicate inferred ones dedup by formula
equality before entering the summary.

**Annotated ensures.** The clause **always enters the summary** with
`Annotated` provenance — callers rely on the human-stated contract.
Verification is best-effort and controls only the warning:

- After the SCC fixpoint settles (callee summaries stable), one solver
  query per (function, ensures clause): body encoding ∧ ¬clause, with
  `r<i>` bound to returned terms at return sites.
- Unsat → verified, silent. Sat or Unknown → **`unverified-annotation`
  warning** finding at the pragma position.
- Queries ride the existing solver stack: query cache, per-tier limits,
  retry-on-Unknown escalation.

An unproven ensures is therefore *used and flagged* — never silently
trusted, never silently dropped. `--deny warnings` is the CI ratchet.

**Ignore** compiles to a `(function FullName, checker name)` set carried
in the analysis result to the CLI. A name not matching any registered
checker → `bad-annotation` error (the parent spec: ignore must name the
checker).

**Recursion:** annotated clauses are constants, not fixpoint state — no
widening interaction. Ensures verification runs post-fixpoint.

## 5. Severity and CLI surface

**Severity.** `Finding` gains `severity: Error | Warning`. All existing
checkers stay `Error`; `bad-annotation` and `contract` are `Error`;
`unverified-annotation` is the sole `Warning`. **Severity is excluded
from fingerprints** — promotion must not churn baselines.

**Exit code.** Exit 1 iff at least one unsuppressed **error-severity**
finding remains. Warnings render but never fail the run.

**`--deny warnings`.** Promotes all warnings to effective-error: exit
code, JSON severity, and SARIF level all reflect the promotion.
Per-checker deny lists are YAGNI while exactly one warning class exists.

**Suppression filter order.** Pragma-ignore first, then baseline; a
finding matching both counts once, under ignore. Match rule:
`finding.checker == named checker && finding.func == annotated
function`. Because `contract` findings attribute to the caller, a
callee's ignore cannot suppress its callers' violations. Ignoring
`contract` or `unverified-annotation` themselves is legal (they are
checker names) and remains visible in suppressed counts.

**Formats.**
- Human summary: separate `suppressed (baseline)` / `suppressed
  (pragma)` counters; warning findings visually distinguished.
- JSON: per-finding `severity`, summary gains `suppressed_pragma`;
  `schema_version` bumps to 2.
- SARIF: severity → `level`; suppression kinds map natively —
  `"inSource"` for pragma-ignore, `"external"` for baseline.
- Hello-corpus goldens churn accordingly; the corpus gate pins the new
  shapes.

**Baselines** suppress by fingerprint as today, severity-blind —
warnings are baselineable.

## 6. Caching and determinism

- Pragmas are already in the scc-cache ctx-hash (position-cleared:
  comment shifts don't invalidate; text changes do). Annotation edits
  invalidate correctly with no new plumbing.
- The annotation compiler gets an `ANNOTATION_VERSION` constant salted
  into the scc cache like checker versions; bump on any semantics
  change to parse/resolve/lower.
- Ensures queries ride the existing query cache.
- **Invariant to verify at plan time:** `func_semantic_hash`
  (`--diff-base`) must see pragma text, so a function whose only change
  is an annotation edit lands in the changed set.
- Determinism: clause order from `.gvir` pragma sort; findings sort in
  the existing pipeline; no map iteration reaches output.

## 7. Security and error handling

- Annotation text is an untrusted-bytes surface (threat model): the
  parser is total, with depth/node caps; **fuzz target #7
  (`annotation_parse`)** joins the nightly set (reject-never-panic
  property), nightly budget extended accordingly.
- Degrade-never-die: any bad annotation yields exactly one error
  finding and the function is analyzed unannotated; analysis never
  aborts.
- Annotation text rendered in messages flows through render-time
  sanitization on the human path and JSON-escaping on machine paths
  (existing pattern for model values).

## 8. Testing

- **Unit:** parser accept/reject tables (incl. trailing-comment strip,
  depth caps, precedence, non-associative comparisons); resolution
  (named/unnamed results, receiver, collisions); lowering (signed vs
  unsigned comparison choice, len/cap sorts, sort-mismatch rejections);
  severity plumbing; filter ordering; `--deny warnings`.
- **Corpus fixtures** (want-pin tags are `[a-z0-9-]`-compatible:
  `contract`, `bad-annotation`, `unverified-annotation`):
  - requires kills an in-body FP and fires `contract` at a violating
    call site (non-liftable shape at the call site per the
    requires-lifting note in the analyzer);
  - a provable ensures verifies silently and a caller demonstrably
    benefits from the clause;
  - an unprovable ensures pins the warning;
  - ignore suppresses a pinned finding (corpus set-equality fails loudly
    on over-suppression);
  - one fixture per bad-annotation class: parse error, unknown name,
    unknown checker, field selection, `effects`/`pure` directive.
- **Fuzz smoke** on `annotation_parse` in-wave (EDR-stall caveat for
  this machine; park if it stalls).
- **Machine-format determinism suite** extends to the new fields.

## 9. Gates (bbolt shakeout)

bbolt carries no annotations, so the wave must be invisible there:

- **G1** — findings identical vs. base (457, same fingerprints, exit 1);
  human output byte-identical apart from the new summary counters.
- **G2** — JSON/SARIF cold/warm byte-identical; differences vs. base
  limited to the schema-v2 additions (severity fields, counters).
- **G3** — warm-time regression guard: annotation compilation ~free
  with zero pragmas (compare against the 3.35 s warm baseline;
  report-only threshold, no hard number).
- **G4 (dogfood)** — patch one `requires` annotation into a bbolt
  working copy and demonstrate the contract split end-to-end: the
  in-body finding disappears, the call-site obligation appears or
  discharges. Documented in a shakeout addendum like prior waves.

## 10. Finding classes introduced

| Checker | Severity | Position | Meaning |
|---|---|---|---|
| `bad-annotation` | Error | pragma site | unparseable / unknown name / unknown checker / unsupported construct or directive |
| `contract` | Error | call site (caller) | annotated requires violated |
| `unverified-annotation` | Warning | pragma site | annotated ensures not proven against the body |

## 11. Deferred to 6b (explicitly out of scope)

- `//goverify:effects` and `//goverify:pure` — parsed today only far
  enough to emit "directive not supported"; real support lands with
  phase-7 consumers.
- Field selection in expressions — needs interface-level heap terms.
- Third-party overrides (`goverify/overrides/<import/path>.gvspec`) —
  needs the on-disk `.gvspec` format, discovery, and a second untrusted
  parser + fuzz target.
- Arithmetic in expressions — add when a fixture or user needs it.

## 12. Open questions (non-blocking)

- Whether `contract` violations should carry the callee's position as a
  trace step (nice-to-have; the message already names the callee).
- Whether `--deny` should eventually take checker names; revisit when a
  second warning class exists.
