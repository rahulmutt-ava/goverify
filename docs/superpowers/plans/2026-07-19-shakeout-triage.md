# Phase-4 Shakeout Triage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Exception:** Task 3 is CONTROLLER-EXECUTED — the main session runs it
> directly because it dispatches triage subagents itself (subagents cannot
> nest). Do not hand Task 3 to an implementer subagent.
>
> **Isolation note:** run this plan on a branch (`shakeout/triage`) in the
> MAIN checkout, not a worktree. The pipeline depends on heavy uncommitted
> state under `.goverify/shakeout/` (bbolt clone, query cache, release
> build) that a fresh worktree would have to rebuild from scratch.

**Goal:** Close the phase-4 spec §7 exit criterion: triage every `goverify check` finding over pinned bbolt into a class-level report with a recorded FP rate, per-finding TSV appendix, and known-FP corpus pins.

**Architecture:** Four-stage pipeline per the design spec (`docs/superpowers/specs/2026-07-19-shakeout-triage-design.md`): capture the shakeout output → bucket findings into classes with a committed awk parser → hand-triage representatives per class via subagents → synthesize the report, appendix, and corpus pins. No product-code changes.

**Tech Stack:** POSIX awk (BSD awk on macOS — no gawk extensions), bash, the existing `mise run shakeout` harness, one new Rust integration test mirroring `nil_corpus.rs`.

## Global Constraints

- **No product-code changes**: nothing under `crates/*/src/` or `extractor/` changes. The only code additions are `scripts/shakeout_bucket.awk`, test fixtures, one corpus module, one integration test, and one `mise.toml` task-list edit.
- **Determinism**: committed artifacts carry no timestamps, no absolute paths, no machine-dependent content. Wall-clock numbers appear only as recorded measurements in the report's run-parameters section.
- **Commits are unsigned**: GPG signing times out in this sandbox. Every commit uses `git commit --no-gpg-sign`.
- **Toolchain via mise**: prefix cargo/go invocations with `mise x --` (e.g. `mise x -- cargo test ...`); named workflows via `mise run <task>`.
- **Finding tags are a closed set**: `nil-deref`, `bounds`, `div-zero`, `overflow` (from `crates/goverify-checkers/src/{nil,bounds}.rs`).
- **Working directory for uncommitted intermediates**: `.goverify/shakeout/triage/` (covered by the pre-existing `.goverify/` gitignore rule).
- **Verdict taxonomy** (design §2.3, exact strings used in every artifact): `TP`, `FP/requires-lifting`, `FP/invariant`, `FP/encoding`, `unclear`, plus `mixed` for classes still heterogeneous after one refinement round.
- **Loud-fail stance for pipeline tooling** (design §4): the parser aborts nonzero on any unrecognized shape; degrade-never-die does NOT apply to this pipeline.

---

### Task 1: awk bucketer with fixture tests

**Files:**
- Create: `scripts/shakeout_bucket.awk`
- Create: `scripts/testdata/shakeout_bucket/sample.txt`
- Create: `scripts/testdata/shakeout_bucket/expected.tsv`
- Create: `scripts/testdata/shakeout_bucket/bad.txt`

**Interfaces:**
- Consumes: `goverify check` rendered stdout. Frozen format per `crates/goverify-cli/src/render.rs`: blocks separated by ONE blank line; each block is a header `file:line:col: tag: message [func]` (pos may be `-:-:-`), then optionally a source-echo line `{line:>5} | {src}` + caret line `{"":>5} | {spaces}^`, then optionally `    path: f:l -> f:l`, then optionally `    with: k = v, k = v`. Sanitization upstream guarantees no tabs or control bytes in any rendered text.
- Produces: TSV on stdout, one row per finding, 8 TAB-separated columns: `pos` (`file:line:col` or `-:-:-`), `tag`, `func`, `message`, `source_line` (gutter-stripped, may be empty), `has_trace` (`0`/`1`), `model` (the `with:` payload, may be empty), `class` (coarse key `tag|<normalized source>` or `tag|msg:<normalized message>` when no source echo). Row order = input order (already deterministic from `analyze_full`). On stderr: `shakeout_bucket: N findings`. Exit 1 + stderr message on any malformed input. Tasks 2–4 consume this TSV and its `class` column verbatim.

- [ ] **Step 1: Write the fixture and expected output (the failing test)**

Create `scripts/testdata/shakeout_bucket/sample.txt` — four blocks covering every shape: full block (source+caret+path+with), source-only, source+with (no path), and header-only (no pos, no snippet). Echoed source uses spaces (the renderer maps tabs to one space):

```
bucket.go:912:15: nil-deref: nil passed to freelist.free (violates its nil-deref requirement) [(*go.etcd.io/bbolt.Bucket).Delete]
  912 |  f.free(tx.meta.txid, p)
      |               ^
    path: bucket.go:900 -> bucket.go:912
    with: p0 = (ptr-nil)

node.go:88:10: bounds: index may exceed slice length [go.etcd.io/bbolt.node.item]
   88 |  return n.inodes[index]
      |         ^

freelist.go:130:12: div-zero: divisor may be zero [go.etcd.io/bbolt.mod]
  130 |  x := a / b
      |       ^
    with: p1 = 0

-:-:-: overflow: narrowing conversion may overflow [go.etcd.io/bbolt.trunc]
```

Create `scripts/testdata/shakeout_bucket/expected.tsv` (columns are real TABs; `source_line` keeps its leading space from the tab-collapsed indent):

```
bucket.go:912:15	nil-deref	(*go.etcd.io/bbolt.Bucket).Delete	nil passed to freelist.free (violates its nil-deref requirement)	 f.free(tx.meta.txid, p)	1	p0 = (ptr-nil)	nil-deref|I.I(I.I.I, I)
node.go:88:10	bounds	go.etcd.io/bbolt.node.item	index may exceed slice length	 return n.inodes[index]	0		bounds|I I.I[I]
freelist.go:130:12	div-zero	go.etcd.io/bbolt.mod	divisor may be zero	 x := a / b	0	p1 = 0	div-zero|I := I / I
-:-:-	overflow	go.etcd.io/bbolt.trunc	narrowing conversion may overflow		0		overflow|msg:I I I I
```

Create `scripts/testdata/shakeout_bucket/bad.txt` (second line has no recognized shape — must abort):

```
node.go:88:10: bounds: index may exceed slice length [go.etcd.io/bbolt.node.item]
garbage without shape
```

- [ ] **Step 2: Run to verify it fails**

Run: `awk -f scripts/shakeout_bucket.awk scripts/testdata/shakeout_bucket/sample.txt`
Expected: FAIL — `awk: can't open file scripts/shakeout_bucket.awk`

- [ ] **Step 3: Write the parser**

Create `scripts/shakeout_bucket.awk`:

```awk
#!/usr/bin/awk -f
# Parse `goverify check` rendered output (frozen format,
# crates/goverify-cli/src/render.rs) into one TSV row per finding and
# assign a coarse triage class (design spec 2026-07-19, section 2.2):
#   pos  tag  func  message  source_line  has_trace  model  class
# class = tag "|" normalized source line (identifiers -> I, numbers -> N,
# string literals -> S, whitespace collapsed), or tag "|msg:" normalized
# message when the block has no source echo.
# Loud-fail contract: any unrecognized line shape, a block without a
# header, or zero findings aborts with exit 1 (stderr says why). stdout
# is data only. Portable: BSD awk, no gawk extensions.

function fail(why) {
  printf "shakeout_bucket: line %d: %s\n", NR, why > "/dev/stderr"
  bad = 1
  exit 1
}

function normalize(s,   t) {
  t = s
  gsub(/"[^"]*"/, "S", t)
  gsub(/`[^`]*`/, "S", t)
  gsub(/[0-9][0-9]*/, "N", t)
  gsub(/[A-Za-z_][A-Za-z0-9_]*/, "I", t)
  gsub(/[ \t][ \t]*/, " ", t)
  sub(/^ /, "", t)
  sub(/ $/, "", t)
  return t
}

function flush_block(   key) {
  if (!have) return
  key = tag "|" (src != "" ? normalize(src) : "msg:" normalize(msg))
  printf "%s\t%s\t%s\t%s\t%s\t%d\t%s\t%s\n", \
    pos, tag, fn, msg, src, has_trace, model, key
  emitted++
  have = 0; pos = ""; tag = ""; fn = ""; msg = ""; src = ""
  model = ""; has_trace = 0
}

/^$/ { flush_block(); next }

# Header: <file>:<line>:<col>: <tag>: <message> [<func>]. The func id is
# everything after the LAST " [" (ssa ids contain no " [" sequence; a
# violation of that assumption trips the tag plausibility check below).
/^[^ ].*\]$/ && !have {
  line = $0
  sub(/\]$/, "", line)
  j = 0
  while (m = index(substr(line, j + 1), " [")) j += m
  if (j == 0) fail("header without [func]: " $0)
  fn = substr(line, j + 2)
  line = substr(line, 1, j - 1)
  if (line ~ /^-:-:-: /) {
    pos = "-:-:-"
    rest = substr(line, 8)
  } else if (match(line, /^[^:]+:[0-9]+:[0-9]+: /)) {
    pos = substr(line, 1, RLENGTH - 2)
    rest = substr(line, RLENGTH + 1)
  } else fail("unparseable position in header: " $0)
  k = index(rest, ": ")
  if (k == 0) fail("header without tag separator: " $0)
  tag = substr(rest, 1, k - 1)
  msg = substr(rest, k + 2)
  if (tag !~ /^[a-z][a-z-]*$/) fail("implausible tag '" tag "': " $0)
  have = 1
  headers++
  next
}

/^ *[0-9][0-9]* \| / && have {
  src = $0
  sub(/^ *[0-9][0-9]* \| /, "", src)
  next
}

/^ +\| *\^$/ && have { next }

/^    path: / && have { has_trace = 1; next }

/^    with: / && have { model = substr($0, 11); next }

{ fail("unrecognized line shape: " $0) }

END {
  if (bad) exit 1
  flush_block()
  if (headers == 0) fail("no findings parsed")
  if (emitted != headers) {
    printf "shakeout_bucket: emitted %d rows for %d headers\n", \
      emitted, headers > "/dev/stderr"
    exit 1
  }
  printf "shakeout_bucket: %d findings\n", emitted > "/dev/stderr"
}
```

- [ ] **Step 4: Run the fixture tests to verify they pass**

Run:
```bash
awk -f scripts/shakeout_bucket.awk scripts/testdata/shakeout_bucket/sample.txt | diff - scripts/testdata/shakeout_bucket/expected.tsv && echo SAMPLE-OK
awk -f scripts/shakeout_bucket.awk scripts/testdata/shakeout_bucket/bad.txt; echo "exit=$?"
```
Expected: `SAMPLE-OK` (empty diff; stderr shows `shakeout_bucket: 4 findings`), then for bad.txt a `shakeout_bucket: line 2: unrecognized line shape: ...` message and `exit=1` (fail() sets `bad`, jumps to END, END exits 1). If the diff shows a mismatch in the `class` column only, fix `expected.tsv` to match the script's actual normalization — the fixture pins real behavior, but recheck by hand that the normalization is doing what the comment says before touching the expectation.

- [ ] **Step 5: Commit**

```bash
git add scripts/shakeout_bucket.awk scripts/testdata/shakeout_bucket/
git commit --no-gpg-sign -m "shakeout: awk bucketer for check output (triage design 2.2)"
```

---

### Task 2: capture runs + run-parameters section

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (Run parameters section only)
- Working (uncommitted): `.goverify/shakeout/triage/{capture-cold.txt,capture-warm.txt,log-cold.txt,log-warm.txt,findings-raw.tsv,classes.txt}`

**Interfaces:**
- Consumes: `mise run shakeout` (scripts/shakeout.sh — clones/updates pinned bbolt v1.4.0, release-builds the CLI, runs `goverify check ./... --cache-dir .goverify/shakeout/cache`; findings on stdout, everything else on stderr; exit 1 expected, exit 2 = analyzer error = BLOCKER). `scripts/shakeout_bucket.awk` from Task 1.
- Produces: `capture-cold.txt` (the canonical capture all later tasks read), `findings-raw.tsv` (8-column TSV per Task 1), `classes.txt` (count-per-class summary), and the filled-in Run parameters section. Task 3 consumes `findings-raw.tsv` + `classes.txt` + `capture-cold.txt`.

- [ ] **Step 1: Cold run (fresh cache), wall-clocked**

```bash
mkdir -p .goverify/shakeout/triage
rm -rf .goverify/shakeout/cache
{ /usr/bin/time -p mise run shakeout > .goverify/shakeout/triage/capture-cold.txt; } 2> .goverify/shakeout/triage/log-cold.txt
tail -5 .goverify/shakeout/triage/log-cold.txt
```
Expected: log tail shows `shakeout: exit 1 (0 clean / 1 findings)` and the `real`/`user`/`sys` lines. If it shows `shakeout: analyzer error` (exit 2): STOP — surface to the user, do not continue (design §4).

- [ ] **Step 2: Warm run, wall-clocked, determinism cross-check**

```bash
{ /usr/bin/time -p mise run shakeout > .goverify/shakeout/triage/capture-warm.txt; } 2> .goverify/shakeout/triage/log-warm.txt
tail -3 .goverify/shakeout/triage/log-warm.txt
diff -q .goverify/shakeout/triage/capture-cold.txt .goverify/shakeout/triage/capture-warm.txt && echo CAPTURES-IDENTICAL
```
Expected: `CAPTURES-IDENTICAL` — warm reuses the cold run's cached verdicts, so findings must be byte-identical. If they differ: keep going with capture-cold.txt as canonical, but record the diff summary in the report's Totals section as a flakiness observation (likely borderline-timeout Unknowns).

- [ ] **Step 3: Bucket the capture**

```bash
awk -f scripts/shakeout_bucket.awk .goverify/shakeout/triage/capture-cold.txt > .goverify/shakeout/triage/findings-raw.tsv
cut -f8 .goverify/shakeout/triage/findings-raw.tsv | sort | uniq -c | sort -rn > .goverify/shakeout/triage/classes.txt
wc -l < .goverify/shakeout/triage/findings-raw.tsv
head -20 .goverify/shakeout/triage/classes.txt
```
Expected: stderr `shakeout_bucket: N findings`; N should be near the ledger's 1006 (toolchain drift is fine — note the delta). If the awk script aborts on a real-capture shape the fixture missed: fix the script AND add the shape to `sample.txt`/`expected.tsv`, re-run Task 1 Step 4, amend commit or commit separately, then re-run this step.

- [ ] **Step 4: Fill in the Run parameters section**

Edit `docs/shakeout-phase4-bbolt.md` — replace the Run parameters section's placeholders (keep the rest of the skeleton untouched for Task 4):

```markdown
## Run parameters
- goverify commit: <output of `git rev-parse --short HEAD`>
- bbolt ref: v1.4.0
- timeouts: infer 100 ms / obligation 250 ms (defaults)
- findings: <N> (ledger's last recorded run: 1006; delta noted if any)
- wall clock: cold <real from log-cold.txt> s / warm <real from log-warm.txt> s
```

- [ ] **Step 5: Commit**

```bash
git add docs/shakeout-phase4-bbolt.md
git commit --no-gpg-sign -m "shakeout: record phase-4 triage run parameters"
```

---

### Task 3: class triage (CONTROLLER-EXECUTED — do not delegate to an implementer subagent)

**Files:**
- Working (uncommitted): `.goverify/shakeout/triage/verdicts/C*.md`, `.goverify/shakeout/triage/verdict-map.tsv`, `.goverify/shakeout/triage/findings-classed.tsv`

**Interfaces:**
- Consumes: `findings-raw.tsv`, `classes.txt`, `capture-cold.txt` (Task 2); the pinned bbolt checkout at `.goverify/shakeout/bbolt`.
- Produces: `verdict-map.tsv` — TAB-separated `class_key  class_id  verdict` covering EVERY class key in classes.txt (verdict from the Global Constraints taxonomy). `findings-classed.tsv` — findings-raw.tsv with two appended columns `class_id`, `verdict`. One `verdicts/C<NN>.md` per class recording the evidence. Task 4 consumes all three.

- [ ] **Step 1: Assign class ids**

Number classes by descending count from classes.txt: C01 = largest. Seed verdict-map.tsv:
```bash
cd .goverify/shakeout/triage
awk '{c=$1; $1=""; sub(/^ /,""); printf "%s\tC%02d\tpending\n", $0, NR}' classes.txt > verdict-map.tsv
```
(If classes.txt's count column trick mangles keys containing multiple spaces — class keys have collapsed single spaces, so it won't — verify: `cut -f1 verdict-map.tsv | sort | diff - <(cut -f8 findings-raw.tsv | sort -u)` must be empty.)

- [ ] **Step 2: Select representatives per class**

For each class (loop over verdict-map.tsv): all rows if count ≤ 5, else 5 by sorted position at indices `1, int((n-1)/4)+1, int((n-1)/2)+1, int(3*(n-1)/4)+1, n` (distinct for n ≥ 5). Extract each representative's full rendered block from the capture:

```bash
mkdir -p .goverify/shakeout/triage/work
# representative positions for one class (sorted by pos, spread-picked):
awk -F'\t' -v c="$CLASS_KEY" '$8==c{print $1}' findings-raw.tsv | sort \
  | awk '{a[NR]=$0} END{n=NR; if(n<=5){for(i=1;i<=n;i++)print a[i]} else {print a[1]; print a[int((n-1)/4)+1]; print a[int((n-1)/2)+1]; print a[int(3*(n-1)/4)+1]; print a[n]}}' \
  > work/reps-C<NN>.txt
# full rendered block for one representative pos P:
awk -v p="$P: " 'index($0,p)==1{f=1} f&&/^$/{exit} f' capture-cold.txt
```

- [ ] **Step 3: Dispatch one triage subagent per class (parallel batches)**

Prompt template — fill the `<...>` slots per class; paste representative blocks verbatim:

```
You are triaging static-analyzer findings from goverify (an SMT-backed
Go analyzer, bug-finder stance: it reports only paths Z3 found
satisfiable under inferred preconditions) against the REAL code of
etcd-io/bbolt v1.4.0, checked out read-only at
<repo>/.goverify/shakeout/bbolt (paths in findings are relative to it).

Class <C-id>: tag=<tag>, <count> findings, pattern: <normalized-pattern>
Representative findings (full rendered blocks: header is
file:line:col: tag: message [func]; "path:" is the violating path's
block trail; "with:" are solver model values on that path):

<blocks>

For EACH representative: read the bbolt source at the site AND enough
surrounding context (the function, its callers via grep, relevant
struct invariants) to decide a verdict:
- TP: the violating path is actually reachable — a real bbolt bug.
- FP/requires-lifting: safe because callers guarantee a precondition
  the analyzer failed to lift to them (e.g. a make() length derived
  from a parameter that callers always bound).
- FP/invariant: safe due to a data-structure invariant the analyzer
  cannot see (e.g. a field established non-nil at construction).
- FP/encoding: the analyzer's own encoding/logic is wrong at this
  site — describe exactly what it got wrong.
- unclear: not determinable from local reading; say what's missing.

RULES: every verdict MUST cite file:line evidence from bbolt source
(callers count). If representatives disagree, name the single
distinguishing feature that separates them (a source-visible
predicate over the site). For any FP/requires-lifting verdict, state
what a substitution-based requires-lifting pass would need to carry
through the call site to kill the finding.

Return exactly this structure (raw text, no prose preamble):
CLASS: <C-id>
PER-REP:
- <pos>: <verdict> — <one-sentence why> [evidence: file:line, ...]
  (repeat per representative)
CLASS-VERDICT: <verdict or MIXED>
DISTINGUISHING-FEATURE: <predicate, only if MIXED>
PHASE5-NOTE: <requires-lifting detail, only if FP/requires-lifting>
```

Save each result verbatim to `verdicts/C<NN>.md`. **Rejection rule:** a response with any verdict lacking a file:line citation is re-dispatched once with `Your previous response was rejected: verdict(s) for <pos> lacked file:line evidence. Re-do with citations.` A second failure → mark the class `unclear` and note the rejection in the verdicts file.

- [ ] **Step 4: Second opinion on the dominant class**

C01 gets an independent second subagent: same template, but a DISJOINT sample — indices `int((n-1)/8)+1, int(3*(n-1)/8)+1, int(5*(n-1)/8)+1, int(7*(n-1)/8)+1` (bump any index by +1 if it collides with the first sample), and append to the prompt: `This is a blind second pass; do not assume any prior verdict exists.` Save as `verdicts/C01-second.md`. If the two class verdicts disagree: controller reads the disputed sites directly, adjudicates, and records the adjudication (both verdicts + reasoning) at the top of `verdicts/C01.md`; the adjudicated verdict wins.

- [ ] **Step 5: One refinement round for MIXED classes**

For each class that came back MIXED: split it on the reported distinguishing feature by re-keying rows — append a suffixed class id (`C05a`/`C05b`) to verdict-map.tsv with an awk predicate over `source_line`/`message` recorded as a comment line in `verdicts/C05.md` (so the split is reproducible), e.g.:
```bash
awk -F'\t' -v c="$CLASS_KEY" '$8==c && $5 ~ /\[I\]$/' findings-raw.tsv   # -> C05a
awk -F'\t' -v c="$CLASS_KEY" '$8==c && $5 !~ /\[I\]$/' findings-raw.tsv  # -> C05b
```
Dispatch Step-3 subagents for each subclass (fresh representatives from the subclass rows). A subclass still MIXED after this single round gets verdict `mixed` and its sampled TP:FP ratio recorded in its verdicts file. Update verdict-map.tsv: replace the parent row with one row per subclass (`class_key` stays the parent key; disambiguation lives in the recorded predicate — see Step 6).

- [ ] **Step 6: Materialize per-finding class ids + verdicts**

```bash
cd .goverify/shakeout/triage
awk -F'\t' 'NR==FNR{id[$1]=$2; v[$1]=$3; next} {print $0 "\t" id[$8] "\t" v[$8]}' verdict-map.tsv findings-raw.tsv > findings-classed.tsv
```
For split classes, re-apply each recorded split predicate to overwrite columns 9–10 of the affected rows with the subclass id/verdict (small awk pass per split, using the predicate saved in the verdicts file). Gate before finishing:
```bash
awk -F'\t' '$9=="" || $10=="" || $10=="pending"' findings-classed.tsv | wc -l   # must print 0
wc -l findings-classed.tsv findings-raw.tsv                                     # equal counts
```

No commit — these are working artifacts; Task 4 commits their synthesis.

---

### Task 4: synthesize report + committed TSV appendix

**Files:**
- Modify: `docs/shakeout-phase4-bbolt.md` (full rewrite of the skeleton's remaining sections)
- Create: `docs/shakeout-phase4-bbolt-findings.tsv`

**Interfaces:**
- Consumes: `findings-classed.tsv`, `verdict-map.tsv`, `verdicts/*.md` (Task 3); run-parameters section (Task 2).
- Produces: the two committed exit-criterion artifacts. Task 5 consumes the report's class table (unanimous-FP classes) to decide which pins to write.

- [ ] **Step 1: Write the committed TSV**

```bash
{ printf 'pos\ttag\tfunc\tmessage\tsource_line\thas_trace\tmodel\tclass_key\tclass_id\tverdict\n'; cat .goverify/shakeout/triage/findings-classed.tsv; } > docs/shakeout-phase4-bbolt-findings.tsv
```

- [ ] **Step 2: Rewrite the report**

`docs/shakeout-phase4-bbolt.md` keeps its title and Run parameters section, status flips to `COMPLETE (2026-07-19)`, and the rest becomes:

```markdown
## Class triage
| class | tag | pattern | count | verdict | reason (one line) | representatives | note |
|---|---|---|---|---|---|---|---|
(one row per class_id in verdict-map order; pattern = the normalized
class key's source part; representatives = the sampled pos values;
reason distilled from the class's verdicts/C*.md; the C01 row's note
records the second-opinion outcome/adjudication)

## Totals
- findings: N; per-verdict counts (TP / FP/requires-lifting /
  FP/invariant / FP/encoding / unclear / mixed-class rows)
- headline FP rate: confirmed-FP rows (verdict starts "FP/") / N.
  Rows of mixed classes count as neither FP nor TP here; the
  estimated rate folding each mixed class's sampled ratio is reported
  separately and labeled an estimate.
- wall clock: (from Run parameters)
- capture determinism: cold==warm byte-identical (or the observed diff)

## Dispatch-precision + phase-5 observations
- dispatch precision (carried Task-10 watch item, spec §16): whether any
  triaged FP's trace routed through a shared-signature over-approximated
  invoke edge — evidence from the verdicts files, or "none observed".
- requires-lifting (phase-5 input): count of FP/requires-lifting
  findings, the canonical example (pos + snippet + trace), and the
  distilled PHASE5-NOTE payloads: exactly what substitution-based
  lifting must carry through call sites to kill this class.
- FP/encoding findings, if any: each is a goverify bug — listed here as
  fix-wave candidates for the plan owner (NOT fixed in this task).

## Exit-criteria disposition (spec §7)
- all findings triaged: every row of the committed TSV carries a class
  id and verdict (mixed classes documented with sampled ratios).
- FP rate recorded: Totals above.
- "every fixed FP lands a corpus case": satisfied vacuously — no fixes
  in scope (design 2026-07-19 §1); KNOWN-FP(phase-5) corpus pins stand
  in as forward-looking red/green targets.
- dispatch-precision observations: section above.
```

- [ ] **Step 3: Verify totals are arithmetic over the TSV**

```bash
tail -n +2 docs/shakeout-phase4-bbolt-findings.tsv | wc -l
tail -n +2 docs/shakeout-phase4-bbolt-findings.tsv | cut -f10 | sort | uniq -c
```
Expected: numbers printed here EQUAL the numbers written in the report's Totals section (recompute the FP rate by hand from them). Any mismatch: fix the report, not the TSV.

- [ ] **Step 4: Commit**

```bash
git add docs/shakeout-phase4-bbolt.md docs/shakeout-phase4-bbolt-findings.tsv
git commit --no-gpg-sign -m "shakeout: phase-4 bbolt triage report + per-finding appendix"
```

---

### Task 5: KNOWN-FP corpus pins

**Files:**
- Create: `testdata/corpus/knownfp/go.mod`
- Create: `testdata/corpus/knownfp/knownfp.go`
- Create: `crates/goverify-checkers/tests/knownfp_corpus.rs`
- Modify: `mise.toml` (corpus task: the `cargo test -p goverify-checkers` line)

**Interfaces:**
- Consumes: the report's class table (Task 4): every class with a unanimous `FP/*` verdict gets one pin; `mixed`/`unclear` classes get none (design §3.4). `goverify_ir::testutil::{load_corpus, wants}` (existing, `crates/goverify-ir/src/testutil.rs`): `load_corpus("knownfp")` extracts `testdata/corpus/knownfp`; `wants("knownfp") -> Vec<(String, u32, String)>` parses `// want: tag` comments (comma-separated for several on one line).
- Produces: a corpus module whose `// want:` lines pin CURRENT (wrong) findings per FP class, and a blocking-tier test enforcing them. Phase 5 flips these pins.

- [ ] **Step 1: Write the integration test (red)**

Create `crates/goverify-checkers/tests/knownfp_corpus.rs`:

```rust
//! Known-FP pins from the phase-4 bbolt shakeout triage
//! (docs/shakeout-phase4-bbolt.md). Every `// want:` in
//! testdata/corpus/knownfp pins CURRENT (wrong) analyzer behavior for a
//! confirmed false-positive class — each carries a KNOWN-FP(phase-5)
//! comment naming its class. Phase 5 (requires-lifting et al.) turns
//! these findings off and must flip the pins to match.

use goverify_analysis::{EngineConfig, Options, analyze_full};
use goverify_checkers::{BoundsChecker, NilChecker};
use goverify_solver::{SolverLimits, Z3Native};

fn limits() -> SolverLimits {
    // Corpus queries are trivial; generous timeout so slow CI can't turn
    // a Sat into Unknown and flake the pins.
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

#[test]
fn knownfp_corpus_findings_match_want_comments() {
    let p = goverify_ir::testutil::load_corpus("knownfp");
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
    };
    let checkers: Vec<&dyn goverify_analysis::Checker> = vec![&NilChecker, &BoundsChecker];
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    let got: std::collections::BTreeSet<(String, u32, String)> = a
        .findings
        .iter()
        .filter(|f| f.func.contains("example.com/knownfp"))
        .filter_map(|f| {
            let pos = f.pos.as_ref()?;
            Some((pos.file.clone(), pos.line, f.tag.clone()))
        })
        .collect();
    let want: std::collections::BTreeSet<(String, u32, String)> =
        goverify_ir::testutil::wants("knownfp").into_iter().collect();
    assert_eq!(got, want, "known-FP pins vs current analyzer behavior");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: FAIL — `load_corpus` panics on the missing `testdata/corpus/knownfp` directory.

- [ ] **Step 3: Write the corpus module**

Create `testdata/corpus/knownfp/go.mod`:

```
module example.com/knownfp

go 1.25.10
```

Create `testdata/corpus/knownfp/knownfp.go` — package clause `package knownfp`, then ONE minimal function per unanimous-FP class from the report's class table. Derive each repro from that class's canonical representative in `verdicts/C*.md`: reproduce the minimal shape that makes the analyzer fire the same tag, WITHOUT the caller context that makes it safe in bbolt. Each pin follows this template (a real example of the shape expected for the dominant make-from-param class — replace/extend with what triage actually found):

```go
// Package knownfp pins CURRENT false-positive analyzer behavior found
// by the phase-4 bbolt shakeout (docs/shakeout-phase4-bbolt.md). Every
// want here is a KNOWN FP: phase 5 must make it disappear and flip the
// pin. Do not "fix" these functions — their unsafety-to-the-analyzer is
// the point.
package knownfp

// KNOWN-FP(phase-5): C01 make-from-param — bbolt callers always bound n,
// but the analyzer keeps the obligation local instead of lifting the
// length relation to callers (shakeout report, class C01).
func MakeFromParam(n int) byte {
	b := make([]byte, n)
	return b[0] // want: bounds
}
```

Then iterate: run the Step-2 command; the `assert_eq!` diff lists actual findings vs wants. Adjust the `// want:` lines to pin ACTUAL current behavior (correct line + tag). Rules: (a) every want line corresponds to a triaged FP class named in its KNOWN-FP comment; (b) every repro function must produce at least one finding — if a class can't be minimally reproduced (finding won't fire outside bbolt's context), DELETE its repro and record "not minimally reproducible" in that class's row note in `docs/shakeout-phase4-bbolt.md` instead; (c) gofmt the file: `mise x -- gofmt -w testdata/corpus/knownfp/knownfp.go`.

- [ ] **Step 4: Run to verify it passes**

Run: `mise x -- cargo test -p goverify-checkers --test knownfp_corpus`
Expected: PASS (`knownfp_corpus_findings_match_want_comments ... ok`)

- [ ] **Step 5: Wire into the corpus task**

In `mise.toml`, `[tasks.corpus]` run list, change:

```toml
  "cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus",
```
to:
```toml
  "cargo test -p goverify-checkers --test nil_corpus --test bounds_corpus --test knownfp_corpus",
```

- [ ] **Step 6: Full blocking gate**

```bash
mise run corpus && mise run lint && mise run test && mise run secrets && mise run audit
```
Expected: all green (test includes the corpus determinism suite; lint covers rustfmt/clippy on the new test file). Report failures verbatim; do not commit on red.

- [ ] **Step 7: Commit**

```bash
git add testdata/corpus/knownfp/ crates/goverify-checkers/tests/knownfp_corpus.rs mise.toml docs/shakeout-phase4-bbolt.md
git commit --no-gpg-sign -m "shakeout: KNOWN-FP corpus pins for triaged bbolt FP classes"
```

---

## Self-review notes

- Spec coverage: design §2.1 → Task 2; §2.2 → Task 1; §2.3 → Task 3; §2.4 + §3.1–3.2 → Task 4; §3.3 → Task 1; §3.4 → Task 5; §4 error handling → Task 1 Step 3 loud-fail + Task 2 Step 1 exit-2 blocker + Task 3 Step 3 rejection rule; §5 verification → Task 4 Step 3 + Task 5 Step 6.
- Report skeleton's `docs/shakeout-phase4-bbolt.md` "triage every finding below" per-finding table is deliberately superseded by the class table + TSV appendix (design §3.1, user-approved).
- Task 3 emits no commit by design (working artifacts); the reviewable deliverable is the gate in its Step 6 plus the files Task 4 synthesizes.
