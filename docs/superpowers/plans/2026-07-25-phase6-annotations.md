# Phase 6: Annotation Language (core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `//goverify:requires`, `//goverify:ensures`, and `//goverify:ignore` — compiled by `goverify-spec`, merged into summaries by the engine, checked at call sites (`contract`), verified best-effort (`unverified-annotation`), never silently ignored (`bad-annotation`) — plus the severity tier they force into existence.

**Architecture:** `goverify-spec` becomes the annotation compiler (parse → resolve → lower to `Term`), depending on `goverify-ir` + `goverify-solver` + `goverify-analysis` (it reuses `sort_of`/`int_repr`/`iface_var_name`). **Dependency direction refinement vs the spec doc:** the engine cannot depend on `goverify-spec` (cycle), so the **CLI** calls `goverify_spec::compile_program(...)` and passes the compiled `Annotations` into `analyze_full` via `EngineConfig`; the data types (`Annotations`, `FuncAnnotations`, `AnnClause`) live in a new `goverify-analysis/src/annotations.rs` so both sides share them. The engine merges annotated clauses into summaries (per-clause provenance), runs contract call-site obligations and ensures verification inside the findings pass (so results land in the SCC cache payload and replay), and the CLI applies the pragma-ignore filter beside the baseline filter.

**Tech Stack:** Rust workspace + Go sidecar. **No new external deps.** `.gvir` schema bump `"3"` → `"4"` (new `Function.result_names` proto field + pragma decl-id alignment).

**Spec:** `docs/superpowers/specs/2026-07-25-phase6-annotations-design.md`. Parent: `2026-07-16-goverify-design.md` §6.

## Global Constraints

- **Determinism is the root invariant.** Annotation clauses derive their order from `.gvir` pragma order (sorted by `(decl_id, text)`); no map iteration reaches output.
- **Annotation text is untrusted input**: the parser rejects, never panics (depth/node caps); fuzz target #7 (`annotation_parse`, Task 12).
- **Errors degrade, never die**: a bad annotation yields exactly one `bad-annotation` error finding and the function is analyzed as if unannotated.
- **Finding messages must never embed source positions** (fingerprint invariant). Annotation expression text is position-free and may be quoted in messages.
- **Severity is excluded from fingerprints** — `fingerprint.rs` is not touched.
- Schema-version bump discipline (AGENTS.md): `schemaVersion` (Go, `extractor/emit.go`) + `SCHEMA_VERSION` (Rust, `crates/goverify-extract/src/load.rs`) + `schema_version` comment (`proto/gvir/v1/gvir.proto`) move together, `"3"` → `"4"`; run `mise run proto-gen` and commit `extractor/gvirpb/`.
- Cache version bumps in this wave: `SCC_CACHE_VERSION` 1→2 and `SCC_ENTRY_FORMAT` 1→2 (clause-provenance + finding-severity codec changes); `goverify_spec::ANNOTATION_VERSION` (new, `= 1`) enters the SCC salt.
- Run toolchain commands through mise: `mise x -- cargo ...` (sandbox RUSTUP relocation; memory `goverify-sandbox-environment`).
- Commits are **unsigned** in this sandbox: `git commit --no-gpg-sign`. Commit-message prefix: `phase6:`.
- Blocking gate per task: `mise run lint` + the task's tests green; `mise run corpus` stays green throughout (except where a task explicitly regenerates goldens). Commit `Cargo.lock` changes with the task that causes them.
- Tests never write into checked-in corpus dirs — copy fixtures to a tempdir first. CLI integration tests reuse one extracted fixture / one sidecar build where possible (EDR stall hazard; memory `sentinelone-exec-stall`).
- Report files go to `.superpowers/sdd/task-N-report-phase6.md`. **Never overwrite** the TRACKED wave-2 records `task-1-investigation.md`, `task-3-investigation.md`, `task-3-report.md`.
- `want:` pin tags must be `[a-z0-9-]+` and sit as the line's LAST `//` comment with real code before it. **A pragma line is a whole-line comment and can never carry a pin** — annotation-site findings (`bad-annotation`, `unverified-annotation` at pragma pos) are asserted with bespoke tuple assertions, not `wants()` (Task 11).

---

## Task Dependency Order

- Task 1 (extractor) → Task 2 (IR plumbing).
- Tasks 3 (severity), 4 (clause provenance), 5 (spec parser) are independent of 1–2 and each other.
- Task 6 (spec resolve/lower/compile) needs 2, 3, 4, 5.
- Task 7 (engine merge) needs 4, 6. Task 8 (engine findings pass + salt) needs 3, 7.
- Task 9 (CLI severity surface) needs 3. Task 10 (CLI wiring + ignore filter + SARIF suppressions) needs 6, 8, 9.
- Task 11 (corpus fixtures + goldens) needs 10. Task 12 (fuzz) needs 5. Task 13 (cache/diff-base invalidation tests) needs 8.
- Task 14 (docs + bbolt shakeout) last.

---

### Task 1: Extractor — result names on `Function`, pragma decl-id = ssa id, schema "4"

**Files:**
- Modify: `proto/gvir/v1/gvir.proto` (Function message, Pragma comment, schema comment)
- Modify: `extractor/emit.go` (`schemaVersion`, `emitFunction`, `emitPragmas`)
- Modify: `crates/goverify-extract/src/load.rs:6` (`SCHEMA_VERSION`)
- Modify: `extractor/extract_test.go` (schema pin, new assertions)
- Generated: `extractor/gvirpb/` (commit `mise run proto-gen` output)

**Interfaces:**
- Produces: `gvir::Function.result_names: Vec<String>` (signature order; `""` for unnamed) and the guarantee that a `Pragma.decl_id` for a function declaration equals the `Function.id` of its SSA function (or is a prefix of instantiations' ids for generics). Task 2 consumes both.

**Why decl-id alignment:** today `decl_id` is `types.Func.FullName()` while `Function.id` is `canonFuncID(ssa.Function)`. They coincide for plain functions (pinned by `extract_test.go:286`) but not for generic instantiations. Emitting the ssa-derived id makes Rust-side matching exact; generic *origins* keep `FullName()` and match instantiations by `id == decl_id || id starts_with(decl_id + "[")`.

- [ ] **Step 1: Proto change**

In `proto/gvir/v1/gvir.proto`, `message Function` (line 93), add after `Position pos = 7;`:

```proto
  // Signature result names in declaration order ("" for unnamed).
  // Length always equals the signature's result count. Names come from
  // the ORIGINAL signature (canonicalized signature types blank names
  // for determinism; this field is the sole carrier of result names).
  repeated string result_names = 8;
```

Update the `Pragma.decl_id` comment (line 182):

```proto
  string decl_id = 1;  // the SSA function id (== Function.id) for func decls
                       // when resolvable, else types.Func.FullName();
                       // "pkgpath.Name" for type/var decls
```

Update the schema-version comment near `schema_version` (line 14) to mention `"4"`.

- [ ] **Step 2: Go emit changes**

In `extractor/emit.go`:

1. Line 26: `schemaVersion = "3"` → `schemaVersion = "4"`.
2. In `emitFunction` (line 418), after the params loop (line 439), add — reading the **original** signature, never the canonicalized one:

```go
	sig := fn.Signature
	for i := range sig.Results().Len() {
		f.ResultNames = append(f.ResultNames, sig.Results().At(i).Name())
	}
```

3. In `emitPragmas` (line 648), in the `*ast.FuncDecl` case, resolve the SSA function for exact id alignment:

```go
			case *ast.FuncDecl:
				doc = d.Doc
				if obj, ok := e.pkg.TypesInfo.Defs[d.Name].(*types.Func); ok {
					// Prefer the SSA id so Rust-side pragma->function
					// matching is exact; generic origins fall back to
					// FullName() and match instantiations by prefix.
					if ssaFn := e.prog.FuncValue(obj); ssaFn != nil {
						declID = canonFuncID(ssaFn)
					} else {
						declID = obj.FullName()
					}
				}
```

(`e.prog` is the `*ssa.Program`; if the emitter doesn't already hold it, thread it in from the extract call site — check `extract.go` for where `ssautil`/`ssa` packages are built and store the program on the emitter struct.)

- [ ] **Step 3: Regenerate protobuf and pin versions**

```bash
mise run proto-gen
```

Change `crates/goverify-extract/src/load.rs:6` to `pub const SCHEMA_VERSION: &str = "4";`. Update the Go-side pin in `extractor/extract_test.go:47-48` to `"4"`.

- [ ] **Step 4: Extractor tests**

In `extractor/extract_test.go`, extend the hello assertions (near line 286):

```go
	// result_names: Deref has one unnamed result.
	if fn := findFunc(pkg, "example.com/hello.Deref"); fn != nil {
		if len(fn.GetResultNames()) != 1 || fn.GetResultNames()[0] != "" {
			t.Fatalf("Deref result_names = %v, want [\"\"]", fn.GetResultNames())
		}
	}
	// pragma decl_id equals the Function.id for a plain func.
	if pr.GetDeclId() != "example.com/hello.Deref" {
		t.Fatalf("pragma decl_id = %q", pr.GetDeclId())
	}
```

Add a method + named-result fixture to the hello corpus module only if it doesn't disturb goldens; otherwise add the assertions to the `annot` corpus in Task 11 and keep this task's test on `hello`. Run:

```bash
cd extractor && go test ./...
```

- [ ] **Step 5: Corpus + lint green, commit**

`mise run corpus` (extraction determinism must hold — result names come from source, not type-checker races; they are declaration facts). Note the `.gvir` byte change rotates every ctx hash — expected, schema bump.

```bash
git add -A && git commit --no-gpg-sign -m "phase6: extractor result_names + pragma decl-id alignment (schema 4)"
```

---

### Task 2: IR — param/result names on `Function`, pragma table on `Program`

**Files:**
- Modify: `crates/goverify-ir/src/func.rs` (Function fields)
- Modify: `crates/goverify-ir/src/lower.rs:53-64` (populate names)
- Modify: `crates/goverify-ir/src/program.rs` (pragma table + accessors)
- Test: `crates/goverify-ir/tests/lower_corpus.rs` (or a new `#[cfg(test)]` block in program.rs)

**Interfaces:**
- Consumes: `gvir::Param.name`, `gvir::Function.result_names` (Task 1), `gvir::Package.pragmas`.
- Produces:
  - `Function.param_names: Vec<String>` (parallel to `params`), `Function.result_names: Vec<String>`.
  - `pub struct PragmaInfo { pub text: String, pub pos: Option<Pos> }` in `program.rs`.
  - `Program::pragmas(&self, f: FuncId) -> &[PragmaInfo]` (empty slice default).
  - `Program::unmatched_pragmas(&self) -> &[PragmaInfo]` — goverify-pragmas whose decl_id matched no function (type/var decls, typos). Task 6 turns these into `bad-annotation` findings.

- [ ] **Step 1: Function fields**

In `crates/goverify-ir/src/func.rs`, add to `Function` (after `params`):

```rust
    /// Declared parameter names, parallel to `params` (receiver is
    /// index 0 for methods). Display/resolution only — never hashed
    /// (hashes are computed over raw .gvir bytes in program.rs).
    pub param_names: Vec<String>,
    /// Signature result names in declaration order ("" for unnamed).
    pub result_names: Vec<String>,
```

In `lower.rs`, inside the params loop (line 55), push the name alongside the id, and copy result names:

```rust
        let mut params = Vec::with_capacity(gf.params.len());
        let mut param_names = Vec::with_capacity(gf.params.len());
        for p in &gf.params {
            if p.id != 0
                && let Some(slot) = values.get_mut(p.id as usize)
            {
                *slot = ValueInfo {
                    ty: resolve_ty(tmap, unknown, p.r#type),
                    kind: ValueKind::Param,
                };
                params.push(ValueId(p.id));
                param_names.push(p.name.clone());
            }
        }
        let result_names = gf.result_names.clone();
```

and add both to the `Function { .. }` literal. Fix every other `Function { .. }` literal in the crate (tests, `testpkg`-style builders in goverify-analysis — `grep -rn "Function {" crates/` and add `param_names: vec![], result_names: vec![]`).

- [ ] **Step 2: Pragma table on Program**

In `program.rs`, add:

```rust
/// A //goverify: pragma attached to a function declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaInfo {
    pub text: String,
    pub pos: Option<Pos>,
}
```

Fields on `Program`:

```rust
    /// FuncId -> its //goverify: pragmas, in .gvir order (sorted by
    /// (decl_id, text) at extraction). Generic origins fan out to every
    /// instantiation (id starts_with(decl_id + "[")).
    pragmas: HashMap<FuncId, Vec<PragmaInfo>>,
    /// Pragmas whose decl_id matched no function (type/var decls,
    /// typos, dead code). Surfaced as bad-annotation findings (spec §4).
    unmatched_pragmas: Vec<PragmaInfo>,
```

In the package-installation loop (near `program.rs:214-235`, after functions are interned), attach pragmas:

```rust
        for pr in &pkg.pragmas {
            if !pr.text.starts_with("//goverify:") {
                continue;
            }
            let info = PragmaInfo {
                text: pr.text.clone(),
                pos: pr.pos.as_ref().map(pos_from_proto),
            };
            let exact = self.by_name.get(pr.decl_id.as_str()).copied();
            if let Some(f) = exact {
                self.pragmas.entry(f).or_default().push(info);
                continue;
            }
            // Generic origin: apply to every instantiation.
            let prefix = format!("{}[", pr.decl_id);
            let mut matched = false;
            let ids: Vec<FuncId> = self
                .func_names
                .iter()
                .enumerate()
                .filter(|(_, n)| n.starts_with(&prefix))
                .map(|(i, _)| FuncId(i as u32))
                .collect();
            for f in ids {
                self.pragmas.entry(f).or_default().push(info.clone());
                matched = true;
            }
            if !matched {
                self.unmatched_pragmas.push(info);
            }
        }
```

(`pos_from_proto` — reuse however lowering converts `gvir::Position` to `Pos`; grep `lower.rs` for the existing helper and call the same one.) Accessors:

```rust
    pub fn pragmas(&self, f: FuncId) -> &[PragmaInfo] {
        self.pragmas.get(&f).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn unmatched_pragmas(&self) -> &[PragmaInfo] {
        &self.unmatched_pragmas
    }
```

Determinism note (add as a comment): `pragmas` is a HashMap for membership only — every consumer iterates via `FuncId` order or a single func's Vec (which preserves `.gvir` sort order); it is never iterated into output.

- [ ] **Step 3: Tests**

In the ir test that loads the hello corpus (`crates/goverify-ir/tests/lower_corpus.rs`), add:

```rust
#[test]
fn hello_pragma_attached_and_names_plumbed() {
    let p = goverify_ir::testutil::load_corpus("hello");
    let f = p.lookup_func("example.com/hello.Deref").expect("Deref");
    let prs = p.pragmas(f);
    assert_eq!(prs.len(), 1, "Deref pragma count");
    assert_eq!(prs[0].text, "//goverify:requires p != nil");
    assert!(prs[0].pos.is_some(), "pragma carries a position");
    let func = p.func(f).expect("body");
    assert_eq!(func.param_names, vec!["p".to_string()]);
    assert_eq!(func.result_names, vec![String::new()]);
    assert!(p.unmatched_pragmas().is_empty());
}
```

Run: `mise x -- cargo test -p goverify-ir`. Expected: PASS (plus the whole-workspace compile fix ripple from Step 1).

- [ ] **Step 4: Corpus green, commit**

`mise run corpus` (the `hello.ir.txt` golden must NOT churn — dump.rs is untouched). Commit:

```bash
git add -A && git commit --no-gpg-sign -m "phase6: IR param/result names + Program pragma table"
```

---

### Task 3: Severity on `Finding` + SCC finding codec

**Files:**
- Modify: `crates/goverify-analysis/src/checker.rs` (Severity enum, Finding field)
- Modify: `crates/goverify-analysis/src/engine.rs:390-397` (construction site)
- Modify: `crates/goverify-analysis/src/scc_cache.rs` (finding codec, version constants)
- Test: in-file unit tests at each site

**Interfaces:**
- Produces: `pub enum Severity { Error, Warning }` (Ord, Copy), `Finding.severity: Severity`. Every existing construction site sets `Severity::Error`. Tasks 8–10 consume.

- [ ] **Step 1: The enum + field**

In `checker.rs`, above `Finding`:

```rust
/// Finding severity (phase-6 spec §5). Exit code and --deny promotion
/// key on this; it is EXCLUDED from fingerprints (promotion must not
/// churn baselines) — fingerprint.rs enumerates its fields explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}
```

Add to `Finding` (LAST field — `Finding` derives `Ord` and the engine sorts by `(pos, message)` explicitly, but appending keeps derived comparisons stable for existing fields):

```rust
    /// Error findings gate exit codes; warnings render but never fail
    /// the run unless promoted (--deny warnings).
    pub severity: Severity,
```

- [ ] **Step 2: Fix every construction site**

`grep -rn "Finding {" crates/` — the exhaustive list (from exploration): `engine.rs:390` (production — set `severity: Severity::Error`), `scc_cache.rs:522` (decode — Step 3), `scc_cache.rs:637` (test), CLI test fixtures `main.rs:829`, `render.rs:128`, `json.rs:96`, `sarif.rs:234`, `sarif.rs:309`, `fingerprint.rs:69`, `baseline.rs:92`. All test fixtures: `severity: goverify_analysis::Severity::Error`.

- [ ] **Step 3: SCC codec**

In `scc_cache.rs`:

1. `SCC_CACHE_VERSION` 1→2 and `SCC_ENTRY_FORMAT` 1→2 (doc comment at line 21 mandates it for entry-format changes; Task 4 shares the same bump).
2. `encode_finding`: after `message`, emit one severity byte; `decode_finding`: read it back:

```rust
    out.push(match f.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
    });
```

```rust
    let severity = match take_u8(input)? {
        0 => Severity::Error,
        1 => Severity::Warning,
        _ => return None,
    };
```

Keep the byte position identical in both (after message, before trace).

- [ ] **Step 4: Tests + commit**

Extend the existing scc_cache round-trip test (near `scc_cache.rs:637`) with a `Severity::Warning` finding and assert it survives encode/decode. Run:

```bash
mise x -- cargo test -p goverify-analysis
mise x -- cargo test --workspace
git add -A && git commit --no-gpg-sign -m "phase6: Finding severity + scc codec (SCC_CACHE_VERSION 2)"
```

---

### Task 4: Per-clause provenance

**Files:**
- Modify: `crates/goverify-analysis/src/summary.rs` (Provenance variant, Clause field)
- Modify: `crates/goverify-analysis/src/scc_cache.rs` (clause + provenance codec)
- Modify: `crates/goverify-checkers/src/*.rs` (Clause literals gain `provenance`)
- Test: in-file

**Interfaces:**
- Produces: `Provenance::Annotated` variant; `Clause.provenance: Provenance` (checker-inferred clauses = `Inferred`). Tasks 6–8 consume. `Summary.provenance` keeps its summary-level meaning (`Inferred`/`Havoc`); `Annotated` is never used at summary level.

- [ ] **Step 1: summary.rs**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Inferred,
    Havoc,
    /// Human-stated (//goverify: pragma). Per-CLAUSE only; a Summary's
    /// own provenance is never Annotated. Annotated clauses are
    /// constants, not fixpoint state: widening preserves them, and
    /// encode_call_ensures trusts them even inside Havoc summaries.
    Annotated,
}
```

Add to `Clause`:

```rust
    pub provenance: Provenance,
```

- [ ] **Step 2: Fix Clause literals**

`grep -rn "Clause {" crates/` — every checker site (`nil.rs:53`, `nil.rs:62`, `nil.rs:146`, bounds.rs equivalents, tests) gains `provenance: Provenance::Inferred`. `push_clause`'s `contains` dedup now distinguishes provenance — correct: cross-provenance dedup is an explicit engine rule (Task 7), not accidental.

- [ ] **Step 3: Codec**

In `scc_cache.rs` (version already bumped in Task 3): `encode_provenance`/`decode_provenance` gain `Provenance::Annotated => 2` / `2 => Some(Provenance::Annotated)`. `encode_clause` emits the provenance byte after the term; `decode_clause` reads it:

```rust
fn encode_clause(c: &Clause, out: &mut Vec<u8>) {
    put_str(out, &c.tag);
    encode_term(&c.formula.term, out);
    encode_provenance(&c.provenance, out);
}
```

- [ ] **Step 4: Tests + commit**

Extend the summary/clause round-trip test with an `Annotated` clause. Full workspace test, lint, commit `phase6: per-clause provenance (Annotated)`.

---

### Task 5: goverify-spec — pragma parser (AST + caps)

**Files:**
- Modify: `Cargo.toml` (workspace dep entry for goverify-spec)
- Modify: `crates/goverify-spec/Cargo.toml`
- Create: `crates/goverify-spec/src/ast.rs`, `crates/goverify-spec/src/parse.rs`
- Modify: `crates/goverify-spec/src/lib.rs`

**Interfaces:**
- Produces (all `pub`, consumed by Task 6 and the fuzz target):
  - `ast::Expr`, `ast::CmpOp`, `ast::Directive`
  - `parse::parse_pragma(text: &str) -> Result<Directive, String>` — total, never panics. Input is the FULL pragma line including `//goverify:`.
  - `pub const ANNOTATION_VERSION: u32 = 1;` in lib.rs — bump on ANY semantics change to parse/resolve/lower.

- [ ] **Step 1: Workspace wiring**

Root `Cargo.toml` `[workspace.dependencies]`: add `goverify-spec = { path = "crates/goverify-spec" }`. `crates/goverify-spec/Cargo.toml` gains (Task 6 adds ir/solver/analysis; this task needs none):

```toml
[dependencies]
```

(leave empty for now — the parser is pure).

- [ ] **Step 2: AST**

`crates/goverify-spec/src/ast.rs`:

```rust
//! Annotation expression AST (phase-6 spec §2). The grammar is a small
//! Go-syntax subset; field selection is PARSED but rejected at
//! resolution (reserved syntax, v1 has no interface-level heap terms).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    Requires(Expr),
    Ensures(Expr),
    /// Checker name to suppress within the annotated function.
    Ignore(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
    /// base.field — parsed, rejected at resolution (spec §2).
    Select(Box<Expr>, String),
    Int(i128),
    Bool(bool),
    Nil,
    Len(Box<Expr>),
    Cap(Box<Expr>),
    /// old(e) — accepted synonym for entry values; resolution unwraps it.
    Old(Box<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Implies(Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
}
```

- [ ] **Step 3: Parser**

`crates/goverify-spec/src/parse.rs` — hand-rolled recursive descent. Complete implementation:

```rust
//! Pragma-line parser (phase-6 spec §2). Untrusted input (parent spec
//! §11): total, never panics, depth/node caps. Errors are one-line
//! human strings quoted into bad-annotation findings.

use crate::ast::{CmpOp, Directive, Expr};

const PREFIX: &str = "//goverify:";
/// Recursion cap: expressions deeper than this are rejected.
const MAX_DEPTH: u32 = 32;
/// Token cap: pragma lines are single source lines; anything past this
/// is hostile or generated.
const MAX_TOKENS: usize = 256;

/// Parse a full pragma line (including the `//goverify:` prefix).
/// A trailing `// comment` on the payload is stripped before parsing.
pub fn parse_pragma(text: &str) -> Result<Directive, String> {
    let rest = text
        .strip_prefix(PREFIX)
        .ok_or_else(|| "not a //goverify: pragma".to_string())?;
    // Strip a trailing //-comment; expressions cannot contain "//"
    // (no division, no strings in the grammar).
    let rest = match rest.find("//") {
        Some(i) => &rest[..i],
        None => rest,
    };
    let rest = rest.trim();
    let (directive, payload) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    match directive {
        "requires" | "ensures" => {
            if payload.is_empty() {
                return Err(format!("`{directive}` needs an expression"));
            }
            let e = parse_expr_str(payload)?;
            Ok(if directive == "requires" {
                Directive::Requires(e)
            } else {
                Directive::Ensures(e)
            })
        }
        "ignore" => {
            let name = payload;
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !valid {
                return Err("`ignore` needs a checker name ([a-z0-9-]+)".to_string());
            }
            Ok(Directive::Ignore(name.to_string()))
        }
        "effects" | "pure" => Err(format!(
            "directive `{directive}` is not supported in this version"
        )),
        "" => Err("empty pragma".to_string()),
        other => Err(format!("unknown directive `{other}`")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Int(i128),
    Punct(&'static str), // one of: ( ) . ! == != <= >= < > && || ==> -
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if out.len() >= MAX_TOKENS {
            return Err("expression too long".to_string());
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(s[start..i].to_string()));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i] as char).is_ascii_digit() {
                i += 1;
            }
            let n: i128 = s[start..i]
                .parse()
                .map_err(|_| format!("integer literal `{}` out of range", &s[start..i]))?;
            out.push(Tok::Int(n));
            continue;
        }
        // Longest-match punctuation. "==>" before "==".
        let rest = &s[i..];
        let p = ["==>", "==", "!=", "<=", ">=", "&&", "||", "(", ")", ".", "!", "<", ">", "-"]
            .into_iter()
            .find(|p| rest.starts_with(p));
        match p {
            Some(p) => {
                out.push(Tok::Punct(p));
                i += p.len();
            }
            None => return Err(format!("unexpected character `{c}`")),
        }
    }
    Ok(out)
}

struct P {
    toks: Vec<Tok>,
    at: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if let Some(Tok::Punct(q)) = self.peek()
            && *q == p
        {
            self.at += 1;
            return true;
        }
        false
    }
}

fn parse_expr_str(s: &str) -> Result<Expr, String> {
    let toks = lex(s)?;
    let mut p = P { toks, at: 0 };
    let e = expr(&mut p, 0)?;
    if p.at != p.toks.len() {
        return Err("trailing tokens after expression".to_string());
    }
    Ok(e)
}

// expr := or ("==>" expr)?   (right-assoc, lowest precedence)
fn expr(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let lhs = or(p, d)?;
    if p.eat_punct("==>") {
        let rhs = expr(p, d)?;
        return Ok(Expr::Implies(Box::new(lhs), Box::new(rhs)));
    }
    Ok(lhs)
}

fn or(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let mut lhs = and(p, d)?;
    while p.eat_punct("||") {
        let rhs = and(p, d)?;
        lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn and(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let mut lhs = cmp(p, d)?;
    while p.eat_punct("&&") {
        let rhs = cmp(p, d)?;
        lhs = Expr::And(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

// cmp := unary (op unary)?   — non-associative: a < b < c is an error.
fn cmp(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let lhs = unary(p, d)?;
    let op = [
        ("==", CmpOp::Eq),
        ("!=", CmpOp::Ne),
        ("<=", CmpOp::Le),
        (">=", CmpOp::Ge),
        ("<", CmpOp::Lt),
        (">", CmpOp::Gt),
    ]
    .into_iter()
    .find(|(t, _)| p.eat_punct(t));
    match op {
        Some((_, op)) => {
            let rhs = unary(p, d)?;
            Ok(Expr::Cmp(op, Box::new(lhs), Box::new(rhs)))
        }
        None => Ok(lhs),
    }
}

fn unary(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    if p.eat_punct("!") {
        let e = unary(p, d)?;
        return Ok(Expr::Not(Box::new(e)));
    }
    if p.eat_punct("-") {
        return match p.peek().cloned() {
            Some(Tok::Int(n)) => {
                p.at += 1;
                Ok(Expr::Int(-n))
            }
            _ => Err("`-` is only supported on integer literals".to_string()),
        };
    }
    primary(p, d)
}

fn primary(p: &mut P, d: u32) -> Result<Expr, String> {
    let d = depth(d)?;
    let e = match p.peek().cloned() {
        Some(Tok::Int(n)) => {
            p.at += 1;
            Expr::Int(n)
        }
        Some(Tok::Punct("(")) => {
            p.at += 1;
            let e = expr(p, d)?;
            if !p.eat_punct(")") {
                return Err("missing `)`".to_string());
            }
            e
        }
        Some(Tok::Ident(id)) => {
            p.at += 1;
            match id.as_str() {
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "nil" => Expr::Nil,
                "len" | "cap" | "old" => {
                    if !p.eat_punct("(") {
                        return Err(format!("`{id}` needs `(`"));
                    }
                    let inner = expr(p, d)?;
                    if !p.eat_punct(")") {
                        return Err("missing `)`".to_string());
                    }
                    let b = Box::new(inner);
                    match id.as_str() {
                        "len" => Expr::Len(b),
                        "cap" => Expr::Cap(b),
                        _ => Expr::Old(b),
                    }
                }
                _ => Expr::Ident(id),
            }
        }
        _ => return Err("expected expression".to_string()),
    };
    // Postfix field selection (parsed; resolution rejects it).
    let mut e = e;
    while p.eat_punct(".") {
        match p.peek().cloned() {
            Some(Tok::Ident(f)) => {
                p.at += 1;
                e = Expr::Select(Box::new(e), f);
            }
            _ => return Err("expected field name after `.`".to_string()),
        }
    }
    Ok(e)
}

fn depth(d: u32) -> Result<u32, String> {
    if d >= MAX_DEPTH {
        return Err("expression too deeply nested".to_string());
    }
    Ok(d + 1)
}
```

`lib.rs` becomes:

```rust
//! Summary/annotation format: parse, resolve, lower (parent spec §6,
//! phase-6 spec). The compiler is pure and deterministic; ANNOTATION_VERSION
//! is salted into the SCC cache — bump it on ANY semantics change to
//! parse/resolve/lower.

pub mod ast;
pub mod parse;

/// Cache-key version of annotation-compilation semantics.
pub const ANNOTATION_VERSION: u32 = 1;
```

- [ ] **Step 4: Accept/reject unit tables**

In `parse.rs` `#[cfg(test)]`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{CmpOp, Directive, Expr};

    #[test]
    fn accepts_spec_examples() {
        for text in [
            "//goverify:requires p != nil && n >= 0",
            "//goverify:ensures err == nil ==> ret != nil",
            "//goverify:requires len(p) > 0",
            "//goverify:requires old(n) >= -1",
            "//goverify:ignore nil-deref   // trailing comment ok",
            "//goverify:requires (a || b) && !c",
        ] {
            parse_pragma(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn precedence_implies_lowest_right_assoc() {
        let Directive::Requires(e) =
            parse_pragma("//goverify:requires a ==> b ==> c").unwrap()
        else {
            panic!("directive")
        };
        // a ==> (b ==> c)
        assert!(matches!(e, Expr::Implies(_, ref r)
            if matches!(**r, Expr::Implies(_, _))));
    }

    #[test]
    fn cmp_non_associative() {
        assert!(parse_pragma("//goverify:requires a < b < c").is_err());
    }

    #[test]
    fn rejects_table() {
        for (text, needle) in [
            ("//goverify:effects locks(mu)", "not supported"),
            ("//goverify:pure", "not supported"),
            ("//goverify:frobnicate x", "unknown directive"),
            ("//goverify:requires", "needs an expression"),
            ("//goverify:ignore Nil_Deref", "checker name"),
            ("//goverify:ignore", "checker name"),
            ("//goverify:requires p +", "unexpected character"),
            ("//goverify:requires p != nil extra", "trailing tokens"),
            ("//goverify:requires 99999999999999999999999999999999999999999", "out of range"),
        ] {
            let err = parse_pragma(text).expect_err(text);
            assert!(err.contains(needle), "{text}: got {err}");
        }
    }

    #[test]
    fn depth_cap_rejects_not_panics() {
        let deep = format!("//goverify:requires {}p{}", "(".repeat(100), ")".repeat(100));
        assert!(parse_pragma(&deep).is_err());
    }

    #[test]
    fn field_selection_parses() {
        // Reserved syntax: parse succeeds; Task 6 resolution rejects.
        let d = parse_pragma("//goverify:requires p.buf != nil").unwrap();
        assert!(matches!(d, Directive::Requires(_)));
    }
}
```

Run: `mise x -- cargo test -p goverify-spec`. Expected: PASS.

- [ ] **Step 5: Lint + commit**

`mise run lint`; commit `phase6: goverify-spec pragma parser (AST, caps, accept/reject tables)`.

---

### Task 6: goverify-spec — resolve, lower, `compile_program`

**Files:**
- Create: `crates/goverify-analysis/src/annotations.rs` (data types only, this task)
- Modify: `crates/goverify-analysis/src/lib.rs` (export)
- Create: `crates/goverify-spec/src/compile.rs`
- Modify: `crates/goverify-spec/src/lib.rs`, `crates/goverify-spec/Cargo.toml`
- Test: `crates/goverify-spec/src/compile.rs` unit tests + a corpus-backed test

**Interfaces:**
- Consumes: Task 2 (`Program::pragmas`, `param_names`/`result_names`), Task 5 (parser), `goverify_analysis::{sort_of, int_repr, Clause, Formula, Provenance, iface_var_name, IfaceVar, Finding, Severity}`, `goverify_solver::{Term, Sort, BvCmpOp, ptr_sort, ptr_is_nil, seq_datatype}`.
- Produces:
  - In `goverify-analysis/src/annotations.rs`:

    ```rust
    pub const CONTRACT: &str = "contract";
    pub const BAD_ANNOTATION: &str = "bad-annotation";
    pub const UNVERIFIED_ANNOTATION: &str = "unverified-annotation";

    /// One compiled annotated clause plus what findings need: the
    /// expression source text (position-free, quotable in messages)
    /// and the pragma position (finding anchor).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AnnClause {
        pub clause: Clause,
        pub text: String,
        pub pos: Option<Pos>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FuncAnnotations {
        pub requires: Vec<AnnClause>,
        pub ensures: Vec<AnnClause>,
        /// Checker names suppressed within this function (validated).
        pub ignores: Vec<String>,
    }

    #[derive(Debug, Clone, Default)]
    pub struct Annotations {
        pub funcs: BTreeMap<FuncId, FuncAnnotations>,
        /// bad-annotation findings from compilation (parse/resolve
        /// errors, unmatched pragmas, unknown ignore names). The CLI
        /// appends these to the analysis findings — they are cheap to
        /// recompute and never enter the SCC cache.
        pub findings: Vec<Finding>,
    }
    ```
  - In `goverify-spec`: `pub fn compile_program(p: &Program, known_checkers: &[&str]) -> Annotations`.
- Rules implemented here (spec §3): identifiers bind receiver/params (`param_names`) and named results (`result_names`); unnamed results get `ret` (single) / `ret0..retN`; declared names win on collision; `requires` may reference **params only**; `old(e)` unwraps to `e`; field selection → error; nil comparisons only against ptr-sorted operands via `ptr_is_nil`; `>`/`>=` lower by operand swap; int literals adopt the other operand's width with range check; top-level sort must be Bool.

- [ ] **Step 1: annotations.rs data types**

Create `crates/goverify-analysis/src/annotations.rs` with exactly the types above (imports: `std::collections::BTreeMap`, `goverify_ir::{FuncId, Pos}`, `crate::checker::{Finding, Severity}`, `crate::summary::Clause`). Add `pub mod annotations;` to `lib.rs` and re-export `pub use annotations::{AnnClause, Annotations, FuncAnnotations, BAD_ANNOTATION, CONTRACT, UNVERIFIED_ANNOTATION};`.

- [ ] **Step 2: spec crate deps**

`crates/goverify-spec/Cargo.toml`:

```toml
[dependencies]
goverify-ir.workspace = true
goverify-solver.workspace = true
goverify-analysis.workspace = true
```

(match the exact workspace-dep style used by `crates/goverify-checkers/Cargo.toml`).

- [ ] **Step 3: compile.rs**

Complete implementation:

```rust
//! Resolve + lower parsed annotations against a function's signature
//! (phase-6 spec §3). Total: every failure is a bad-annotation error
//! finding; the function is analyzed as if unannotated.

use std::collections::BTreeMap;

use goverify_analysis::annotations::{
    AnnClause, Annotations, FuncAnnotations, BAD_ANNOTATION, CONTRACT,
};
use goverify_analysis::{
    Clause, Finding, Formula, IfaceVar, Provenance, Severity, iface_var_name, int_repr, sort_of,
};
use goverify_ir::{FuncId, Program};
use goverify_solver::{BvCmpOp, Sort, Term, ptr_is_nil, seq_datatype};

use crate::ast::{CmpOp, Directive, Expr};
use crate::parse::parse_pragma;

/// Compile every //goverify: pragma in the program. `known_checkers`
/// is the valid `ignore` name set (checker names + the annotation
/// finding classes) — the CLI owns the list.
pub fn compile_program(p: &Program, known_checkers: &[&str]) -> Annotations {
    let mut out = Annotations::default();
    for f in p.func_ids() {
        let prs = p.pragmas(f);
        if prs.is_empty() {
            continue;
        }
        let mut fa = FuncAnnotations::default();
        for pr in prs {
            match compile_one(p, f, &pr.text, known_checkers) {
                Ok(Compiled::Requires(c)) => fa.requires.push(AnnClause {
                    clause: c,
                    text: expr_text(&pr.text),
                    pos: pr.pos.clone(),
                }),
                Ok(Compiled::Ensures(c)) => fa.ensures.push(AnnClause {
                    clause: c,
                    text: expr_text(&pr.text),
                    pos: pr.pos.clone(),
                }),
                Ok(Compiled::Ignore(name)) => fa.ignores.push(name),
                Err(msg) => out.findings.push(bad(p.func_name(f), pr.pos.clone(), &msg)),
            }
        }
        if fa != FuncAnnotations::default() {
            out.funcs.insert(f, fa);
        }
    }
    for pr in p.unmatched_pragmas() {
        out.findings.push(bad(
            "-",
            pr.pos.clone(),
            "annotation is not attached to a function declaration",
        ));
    }
    out
}

/// The expression/payload part of the pragma line, for messages.
/// Control characters are stripped: this string is quoted into finding
/// messages, and the human renderer sanitizes file paths but NOT
/// messages — untrusted bytes must not reach the terminal raw.
fn expr_text(text: &str) -> String {
    let rest = text.strip_prefix("//goverify:").unwrap_or(text);
    let rest = match rest.find("//") {
        Some(i) => &rest[..i],
        None => rest,
    };
    rest.trim().chars().filter(|c| !c.is_control()).collect()
}

fn bad(func: &str, pos: Option<goverify_ir::Pos>, msg: &str) -> Finding {
    Finding {
        checker: BAD_ANNOTATION.to_string(),
        tag: BAD_ANNOTATION.to_string(),
        func: func.to_string(),
        pos,
        message: format!("invalid annotation: {msg}"),
        trace: Vec::new(),
        model: Vec::new(),
        severity: Severity::Error,
    }
}

enum Compiled {
    Requires(Clause),
    Ensures(Clause),
    Ignore(String),
}

fn compile_one(
    p: &Program,
    f: FuncId,
    text: &str,
    known_checkers: &[&str],
) -> Result<Compiled, String> {
    let d = parse_pragma(text)?;
    match d {
        Directive::Ignore(name) => {
            if !known_checkers.contains(&name.as_str()) {
                return Err(format!(
                    "`ignore` names unknown checker `{name}` (known: {})",
                    known_checkers.join(", ")
                ));
            }
            Ok(Compiled::Ignore(name))
        }
        Directive::Requires(e) => {
            let term = lower_bool(p, f, &e, /*allow_results=*/ false)?;
            Ok(Compiled::Requires(Clause {
                tag: CONTRACT.to_string(),
                formula: Formula { term },
                provenance: Provenance::Annotated,
            }))
        }
        Directive::Ensures(e) => {
            let term = lower_bool(p, f, &e, /*allow_results=*/ true)?;
            Ok(Compiled::Ensures(Clause {
                tag: CONTRACT.to_string(),
                formula: Formula { term },
                provenance: Provenance::Annotated,
            }))
        }
    }
}

/// A resolved name: its interface var and Go type.
struct Binding {
    var: IfaceVar,
    ty: goverify_ir::TypeId,
}

/// Name table for one function: receiver+params by declared name,
/// results by declared name or ret/ret<i>. Declared names win.
fn bindings(p: &Program, f: FuncId) -> Result<BTreeMap<String, Binding>, String> {
    let func = p.func(f).ok_or("annotated function has no body in .gvir")?;
    let mut map: BTreeMap<String, Binding> = BTreeMap::new();
    for (i, name) in func.param_names.iter().enumerate() {
        if name.is_empty() || name == "_" {
            continue;
        }
        let ty = func.value(func.params[i]).ty;
        map.insert(name.clone(), Binding { var: IfaceVar::Param(i as u32), ty });
    }
    let goverify_ir::TypeKind::Signature { results, .. } = p.types().kind(func.sig) else {
        return Err("function signature not available".to_string());
    };
    let results = results.clone();
    if results.len() != func.result_names.len() {
        return Err("signature/result_names arity mismatch (stale .gvir?)".to_string());
    }
    for (i, (name, ty)) in func.result_names.iter().zip(&results).enumerate() {
        let keys: Vec<String> = if !name.is_empty() && name != "_" {
            vec![name.clone()]
        } else if results.len() == 1 {
            vec!["ret".to_string(), "ret0".to_string()]
        } else {
            vec![format!("ret{i}")]
        };
        for k in keys {
            // Declared names win: don't clobber an existing entry.
            map.entry(k).or_insert(Binding { var: IfaceVar::Result(i as u32), ty: *ty });
        }
    }
    Ok(map)
}

fn lower_bool(p: &Program, f: FuncId, e: &Expr, allow_results: bool) -> Result<Term, String> {
    let names = bindings(p, f)?;
    let t = lower(p, &names, e, allow_results)?;
    match t.sort() {
        Sort::Bool => Ok(t),
        s => Err(format!("expression is {s:?}, expected a boolean condition")),
    }
}

fn lower(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    e: &Expr,
    allow_results: bool,
) -> Result<Term, String> {
    match e {
        Expr::Old(inner) => lower(p, names, inner, allow_results),
        Expr::Select(..) => Err("field selection is not supported in v1".to_string()),
        Expr::Nil => Err("`nil` is only usable in == / != comparisons".to_string()),
        Expr::Bool(b) => Ok(Term::bool_lit(*b)),
        Expr::Int(_) => Err("bare integer literal is not a condition".to_string()),
        Expr::Ident(name) => {
            let b = names
                .get(name)
                .ok_or_else(|| format!("unknown name `{name}` (params/results only)"))?;
            if !allow_results && matches!(b.var, IfaceVar::Result(_)) {
                return Err(format!("`{name}` is a result; `requires` may only reference parameters"));
            }
            let sort = sort_of(p.types(), b.ty)
                .ok_or_else(|| format!("type of `{name}` is not modeled by the analyzer"))?;
            Ok(Term::var(&iface_var_name(&b.var), sort))
        }
        Expr::Len(inner) | Expr::Cap(inner) => {
            let base = lower(p, names, inner, allow_results)?;
            if base.sort() != &seq_datatype().sort() {
                return Err("len/cap need a slice or string operand".to_string());
            }
            let field = if matches!(e, Expr::Len(_)) { "seq-len" } else { "seq-cap" };
            Term::dt_get(&seq_datatype(), "seq-val", field, base)
                .map_err(|e| format!("internal sort error: {e}"))
        }
        Expr::Not(inner) => {
            let t = lower(p, names, inner, allow_results)?;
            Term::not(t).map_err(|_| "`!` needs a boolean operand".to_string())
        }
        Expr::And(a, b) => {
            let (a, b) = (lower(p, names, a, allow_results)?, lower(p, names, b, allow_results)?);
            Term::and(vec![a, b]).map_err(|_| "`&&` needs boolean operands".to_string())
        }
        Expr::Or(a, b) => {
            let (a, b) = (lower(p, names, a, allow_results)?, lower(p, names, b, allow_results)?);
            Term::or(vec![a, b]).map_err(|_| "`||` needs boolean operands".to_string())
        }
        Expr::Implies(a, b) => {
            let (a, b) = (lower(p, names, a, allow_results)?, lower(p, names, b, allow_results)?);
            Term::implies(a, b).map_err(|_| "`==>` needs boolean operands".to_string())
        }
        Expr::Cmp(op, a, b) => lower_cmp(p, names, *op, a, b, allow_results),
    }
}

fn lower_cmp(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    op: CmpOp,
    a: &Expr,
    b: &Expr,
    allow_results: bool,
) -> Result<Term, String> {
    // nil comparisons: ptr tester, either side.
    let nil_side = match (a, b) {
        (Expr::Nil, Expr::Nil) => return Err("`nil == nil` is not a useful condition".to_string()),
        (Expr::Nil, other) | (other, Expr::Nil) => Some(other),
        _ => None,
    };
    if let Some(other) = nil_side {
        if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
            return Err("`nil` only supports == and !=".to_string());
        }
        let t = lower(p, names, other, allow_results)?;
        if t.sort() != &goverify_solver::ptr_sort() {
            return Err("nil comparison needs a pointer/interface operand".to_string());
        }
        let is_nil = ptr_is_nil(t).map_err(|e| format!("internal sort error: {e}"))?;
        return match op {
            CmpOp::Eq => Ok(is_nil),
            _ => Term::not(is_nil).map_err(|e| format!("internal sort error: {e}")),
        };
    }
    // Integer-literal sides adopt the other side's width+signedness.
    let (lt, rt, signed) = match (a, b) {
        (Expr::Int(_), Expr::Int(_)) => {
            return Err("comparing two integer literals is not a useful condition".to_string());
        }
        (Expr::Int(n), other) => {
            let (t, w, s) = int_operand(p, names, other, allow_results)?;
            (lit(*n, w, s)?, t, s)
        }
        (other, Expr::Int(n)) => {
            let (t, w, s) = int_operand(p, names, other, allow_results)?;
            (t, lit(*n, w, s)?, s)
        }
        _ => {
            let ta = lower(p, names, a, allow_results)?;
            let tb = lower(p, names, b, allow_results)?;
            if ta.sort() != tb.sort() {
                return Err("comparison operands have different types".to_string());
            }
            match (ta.sort(), op) {
                (_, CmpOp::Eq) => {
                    return Term::eq(ta, tb).map_err(|e| format!("internal sort error: {e}"));
                }
                (_, CmpOp::Ne) => {
                    let eq = Term::eq(ta, tb).map_err(|e| format!("internal sort error: {e}"))?;
                    return Term::not(eq).map_err(|e| format!("internal sort error: {e}"));
                }
                (Sort::BitVec(_), _) => {
                    // Ordered compare: need signedness from the Go type.
                    let s = expr_signedness(p, names, a, allow_results)
                        .or_else(|| expr_signedness(p, names, b, allow_results))
                        .ok_or("cannot determine signedness of comparison")?;
                    (ta, tb, s)
                }
                _ => return Err("ordered comparison needs integer operands".to_string()),
            }
        }
    };
    // == / != on the literal paths:
    match op {
        CmpOp::Eq => return Term::eq(lt, rt).map_err(|e| format!("internal sort error: {e}")),
        CmpOp::Ne => {
            let eq = Term::eq(lt, rt).map_err(|e| format!("internal sort error: {e}"))?;
            return Term::not(eq).map_err(|e| format!("internal sort error: {e}"));
        }
        _ => {}
    }
    // No Ugt/Sgt in BvCmpOp: > and >= swap operands (house convention,
    // encode.rs binop_term).
    let (cmp, l, r) = match (op, signed) {
        (CmpOp::Lt, true) => (BvCmpOp::Slt, lt, rt),
        (CmpOp::Lt, false) => (BvCmpOp::Ult, lt, rt),
        (CmpOp::Le, true) => (BvCmpOp::Sle, lt, rt),
        (CmpOp::Le, false) => (BvCmpOp::Ule, lt, rt),
        (CmpOp::Gt, true) => (BvCmpOp::Slt, rt, lt),
        (CmpOp::Gt, false) => (BvCmpOp::Ult, rt, lt),
        (CmpOp::Ge, true) => (BvCmpOp::Sle, rt, lt),
        (CmpOp::Ge, false) => (BvCmpOp::Ule, rt, lt),
        (CmpOp::Eq | CmpOp::Ne, _) => unreachable!("handled above"),
    };
    Term::bv_cmp(cmp, l, r).map_err(|e| format!("internal sort error: {e}"))
}

/// Lower a non-literal integer operand; return (term, width, signed).
/// len()/cap() are unsigned 64-bit by construction.
fn int_operand(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    e: &Expr,
    allow_results: bool,
) -> Result<(Term, u32, bool), String> {
    let t = lower(p, names, e, allow_results)?;
    let Sort::BitVec(w) = t.sort() else {
        return Err("ordered comparison needs an integer operand".to_string());
    };
    let w = *w;
    let signed = expr_signedness(p, names, e, allow_results).unwrap_or(true);
    Ok((t, w, signed))
}

/// Signedness of an expression from its Go type: idents via int_repr;
/// len/cap are non-negative 64-bit values, compared signed (they fit
/// in i64 — Go len() is int) which matches bounds.rs's Slt/Sle use.
fn expr_signedness(
    p: &Program,
    names: &BTreeMap<String, Binding>,
    e: &Expr,
    allow_results: bool,
) -> Option<bool> {
    match e {
        Expr::Old(inner) | Expr::Not(inner) => expr_signedness(p, names, inner, allow_results),
        Expr::Len(_) | Expr::Cap(_) => Some(true),
        Expr::Ident(name) => {
            let b = names.get(name)?;
            int_repr(p.types(), b.ty).map(|(_, s)| s)
        }
        _ => None,
    }
}

/// An integer literal at the operand's width; range-checked, negative
/// values two's-complement-masked (bounds.rs lit_sext convention).
/// w <= 64 always holds (int_repr caps at 64, len/cap are 64-bit);
/// the guard keeps the shifts below well-defined regardless.
fn lit(n: i128, w: u32, signed: bool) -> Result<Term, String> {
    if w == 0 || w > 64 {
        return Err(format!("unsupported operand width {w}"));
    }
    let fits = if signed {
        let min = -(1i128 << (w - 1));
        let max = (1i128 << (w - 1)) - 1;
        n >= min && n <= max
    } else {
        n >= 0 && (w == 128 || n < (1i128 << w))
    };
    if !fits {
        return Err(format!("literal {n} does not fit the operand's {w}-bit type"));
    }
    let mask = if w == 128 { u128::MAX } else { (1u128 << w) - 1 };
    Ok(Term::bv_lit(w, (n as u128) & mask))
}
```

Add `pub mod compile;` and `pub use compile::compile_program;` to `lib.rs`.

- [ ] **Step 4: Tests**

Unit tests in `compile.rs` build a corpus-backed program (`goverify_ir::testutil::load_corpus("hello")` — dev-dependency on goverify-ir's testutil is already how checker tests work; if testutil is behind `#[cfg(test)]`-visibility, use a `tests/compile_corpus.rs` integration test instead):

```rust
// tests/compile_corpus.rs
use goverify_analysis::annotations::CONTRACT;
use goverify_ir::testutil::load_corpus;

#[test]
fn hello_requires_compiles_to_nonnil_clause() {
    let p = load_corpus("hello");
    let ann = goverify_spec::compile_program(&p, &["nil", "bounds"]);
    let f = p.lookup_func("example.com/hello.Deref").unwrap();
    let fa = ann.funcs.get(&f).expect("Deref annotations");
    assert_eq!(fa.requires.len(), 1);
    assert_eq!(fa.requires[0].clause.tag, CONTRACT);
    assert_eq!(fa.requires[0].text, "p != nil");
    // Free vars: exactly p0.
    let vars = fa.requires[0].clause.formula.term.free_vars();
    assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["p0"]);
    assert!(ann.findings.is_empty(), "hello has no bad annotations");
}
```

Plus pure-unit rejection tests (no corpus): construct error paths via a small synthetic program if `testpkg` helpers are reachable, else defer synthetic-program error tests to Task 8's engine tests and keep this task's rejection coverage at the resolution-error strings exercised through `compile_one` with the hello program (`unknown name`, `requires references result`, `field selection`, `nil on int param` — write one hello-incompatible pragma each via a direct `compile_one` call if visible, or via fixtures in Task 11). Run:

```bash
mise x -- cargo test -p goverify-spec
```

- [ ] **Step 5: Lint + commit**

`mise run lint`; commit `phase6: annotation compiler (resolve+lower+compile_program)`.

---

### Task 7: Engine — merge annotated clauses; widening/bodyless/encode interplay

**Files:**
- Modify: `crates/goverify-analysis/src/engine.rs` (EngineConfig, analyze_function, widening, bodyless)
- Modify: `crates/goverify-analysis/src/summary.rs` (havoc_with helper)
- Modify: `crates/goverify-analysis/src/encode.rs` (`encode_call_ensures` per-clause trust rule)
- Test: engine unit tests (testpkg-based)

**Interfaces:**
- Consumes: `Annotations`/`FuncAnnotations` (Task 6 types), per-clause provenance (Task 4).
- Produces: `EngineConfig.annotations: Annotations`; summaries whose `requires`/`ensures` contain the annotated clauses (dedup rule below); widening and bodyless functions preserve annotated clauses. Task 8 builds on the merged summaries.
- **Dedup rule (spec §4):** after checker inference, an annotated clause is dropped iff its `formula.term` equals any inferred clause's `formula.term` (tag-blind, provenance-blind) — prevents double call-site reporting for contracts the checkers already infer.
- **Widening rule (spec §4):** annotated clauses are constants, not fixpoint state. The widening branch replaces `Summary::havoc()` with havoc-plus-annotated-clauses.
- **Trust rule for callee ensures:** `encode_call_ensures` asserts a callee ensures clause iff (summary provenance is `Inferred`) OR (the clause's own provenance is `Annotated`) — so annotations on bodyless/widened functions still help callers.

- [ ] **Step 1: EngineConfig + threading**

In `engine.rs`:

```rust
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    pub opts: Options,
    pub cache_dir: Option<PathBuf>,
    pub emit_smt: Option<PathBuf>,
    /// Compiled //goverify: annotations (phase-6). Default empty.
    pub annotations: crate::annotations::Annotations,
}
```

In `analyze_full`, before the wave loop: `let annotations = &cfg.annotations;`. Thread `annotations.funcs.get(&m)` into `analyze_function` as a new parameter `ann: Option<&FuncAnnotations>`, and into the widening branch:

```rust
                if rounds >= cfg.opts.widen_after {
                    for &m in members {
                        // Widening discards fixpoint state, never human
                        // facts: annotated clauses survive (phase-6 §4).
                        current.insert(m, havoc_with(annotations.funcs.get(&m)));
                    }
                    break;
                }
```

- [ ] **Step 2: merge + havoc_with**

In `summary.rs` add (imports: `crate::annotations::FuncAnnotations`):

```rust
/// Merge annotated clauses into a summary (phase-6 spec §4): dedup is
/// FORMULA-equality, tag- and provenance-blind — a contract the
/// checkers already infer keeps single ownership (no double call-site
/// findings).
pub fn merge_annotations(s: &mut Summary, ann: Option<&FuncAnnotations>) {
    let Some(ann) = ann else { return };
    for ac in &ann.requires {
        if !s.requires.iter().any(|c| c.formula == ac.clause.formula) {
            s.requires.push(ac.clause.clone());
        }
    }
    for ac in &ann.ensures {
        if !s.ensures.iter().any(|c| c.formula == ac.clause.formula) {
            s.ensures.push(ac.clause.clone());
        }
    }
}

/// Havoc plus annotated clauses — the widened/bodyless shape.
pub fn havoc_with(ann: Option<&FuncAnnotations>) -> Summary {
    let mut s = Summary::havoc();
    merge_annotations(&mut s, ann);
    s
}
```

In `analyze_function`, the bodyless early return becomes `return havoc_with(ann);` and the successful body path merges before returning:

```rust
        let mut s = Summary {
            effects,
            requires,
            ensures,
            ..Summary::default()
        };
        crate::summary::merge_annotations(&mut s, ann);
        s
```

The panic fallback (`Err(_)` arm) also becomes `havoc_with(ann)`. The final assembly loop's `unwrap_or_else(Summary::havoc)` (engine.rs:287) becomes `unwrap_or_else(|| havoc_with(annotations.funcs.get(&f)))` — external functions annotated via pragmas in *their defining package* still carry their contracts.

- [ ] **Step 3: encode_call_ensures trust rule**

In `encode.rs:820-826`, replace the summary-level gate:

```rust
            let s = summary_of(*c);
            // Trust rule (phase-6 §4): Inferred summaries contribute all
            // their ensures; Havoc summaries contribute ONLY Annotated
            // clauses (human facts survive widening/bodylessness).
            let usable: Vec<crate::summary::Clause> = s
                .ensures
                .iter()
                .filter(|cl| {
                    s.provenance == crate::summary::Provenance::Inferred
                        || cl.provenance == crate::summary::Provenance::Annotated
                })
                .cloned()
                .collect();
            if usable.is_empty() {
                continue;
            }
            let s = crate::summary::Summary { ensures: usable, ..s };
```

(then the existing `instantiate_ensures(&s, ...)` loop is unchanged).

- [ ] **Step 4: Tests**

Engine unit tests (follow the existing testpkg-based test style at `engine.rs:800+`; testpkg `Function` literals now need `param_names`/`result_names` — give params real names):

1. **Merge + dedup:** a function with an annotated requires whose formula duplicates an inferred one → summary contains it ONCE (the inferred clause, its original tag); a non-duplicate annotated requires → present with tag `contract`, provenance `Annotated`.
2. **Widening preserves annotations:** a 2-function recursive SCC with `widen_after: 0` and an annotated requires on one member → after `analyze_full`, that member's summary is havoc-shaped (top effects) but `requires` contains exactly the annotated clause.
3. **encode_call_ensures trust:** a Havoc-provenance callee summary containing one `Annotated` ensures and one `Inferred` ensures → caller encoding asserts exactly the annotated one (inspect `enc.asserts` count before/after or via a distinguishing formula).

Run: `mise x -- cargo test -p goverify-analysis`. Expected: PASS.

- [ ] **Step 5: Corpus + commit**

`mise run corpus` — the hello corpus now merges `p != nil` into `Deref`'s summary. **Check `testdata/goldens/conc.summary.txt`** (summary dump golden) — hello isn't in it, but if any summary-count golden churns, inspect: only annotated modules may change. Commit `phase6: engine merges annotated clauses (dedup, widening, callee-ensures trust)`.

### Task 8: Engine findings pass — contract obligations, ensures verification, cache salt

**Files:**
- Modify: `crates/goverify-analysis/src/annotations.rs` (the two passes)
- Modify: `crates/goverify-analysis/src/engine.rs` (findings-pass integration, CacheConfigKey)
- Modify: `crates/goverify-analysis/src/scc_cache.rs` (CacheConfigKey field + salt input)
- Test: engine unit tests

**Interfaces:**
- Consumes: merged summaries (Task 7), `AnnClause.text/pos`, `discharge_query`, `EncodedFunc::reach_query`, `instantiate_requires`/`instantiate_ensures`, `Severity` (Task 3).
- Produces:
  - `annotations::contract_obligations(p, func, enc, own, ann_of) -> Vec<Obligation>` — call-site obligations for callees' **annotated** requires, message `call to <callee> violates annotated requires \`<text>\``.
  - `annotations::verify_ensures(p, f, enc: Option<&EncodedFunc>, ann, discharge) -> Vec<Finding>` — `unverified-annotation` warnings.
  - `EngineConfig.annotation_version: u32` (CLI sets `goverify_spec::ANNOTATION_VERSION`; default 0) salted into the SCC cache via a new `CacheConfigKey.annotation_version` field.
- **Cache-correctness recap** (why this is sound): pragma text is inside `ctx_hash` → `func_ir_hash` → SCC keys, so pragma edits rotate keys; compiler-semantics changes rotate via `annotation_version` in the salt; engine-pass changes are covered by the `SCC_CACHE_VERSION` 2 bump. Contract/unverified findings are produced inside the per-function findings block, so they enter `fresh_out` → the SCC entry payload → warm replay.

- [ ] **Step 1: contract_obligations**

Add to `annotations.rs` (imports: `crate::checker::Obligation`, `crate::encode::EncodedFunc`, `crate::summary::{instantiate_requires, Summary}`, `goverify_ir::{Callee, Function, Op, Program}`):

```rust
/// Call-site obligations for callees' ANNOTATED requires (phase-6 §4).
/// Mirrors goverify-checkers shared::call_site_obligations but iterates
/// the callee's FuncAnnotations directly (order-parallel with texts —
/// merged summaries interleave inferred clauses, so indexes there don't
/// line up with pragma texts). `pre` is the CALLER's own requires terms
/// (annotated included), for precision parity with the checkers.
pub fn contract_obligations(
    p: &Program,
    func: &Function,
    enc: &EncodedFunc,
    own: &Summary,
    ann_of: &dyn Fn(goverify_ir::FuncId) -> Option<FuncAnnotations>,
) -> Vec<Obligation> {
    let pre: Vec<goverify_solver::Term> =
        own.requires.iter().map(|c| c.formula.term.clone()).collect();
    let mut out = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            let Op::Call { callee: Callee::Static(c), args, .. } = &ins.op else {
                continue;
            };
            let Some(fa) = ann_of(*c) else { continue };
            if fa.requires.is_empty() {
                continue;
            }
            let arg_terms: Vec<Option<goverify_solver::Term>> =
                args.iter().map(|a| enc.value(*a).cloned()).collect();
            let tmp = Summary {
                requires: fa.requires.iter().map(|a| a.clause.clone()).collect(),
                ..Summary::default()
            };
            for (bc, ac) in instantiate_requires(&tmp, &arg_terms).into_iter().zip(&fa.requires) {
                let Some(v) = bc.violation else { continue };
                let mut extra = pre.clone();
                extra.push(v);
                out.push(Obligation {
                    tag: CONTRACT.to_string(),
                    message: format!(
                        "call to {} violates annotated requires `{}`",
                        p.func_name(*c),
                        ac.text
                    ),
                    pos: ins.pos.clone(),
                    query: enc.reach_query(bi, extra),
                });
            }
        }
    }
    out
}
```

- [ ] **Step 2: verify_ensures**

Add to `annotations.rs` (pattern: nil.rs `infer_ensures` — per return site, bind, discharge; `Unsat` everywhere = proven):

```rust
/// Best-effort verification of ANNOTATED ensures (phase-6 §4): for each
/// clause, body ∧ ¬clause per return site — Unsat at every site means
/// verified (silent). Sat, Unknown, unbindable, no body, or no return
/// sites all yield the unverified-annotation WARNING: the clause is
/// still trusted by callers (used and flagged, never silently either).
pub fn verify_ensures(
    p: &Program,
    f: goverify_ir::FuncId,
    enc: Option<&EncodedFunc>,
    ann: &FuncAnnotations,
    discharge: &mut dyn FnMut(&goverify_solver::Query) -> goverify_solver::SatResult,
) -> Vec<Finding> {
    use goverify_solver::SatResult;
    let mut out = Vec::new();
    let mut warn = |ac: &AnnClause| {
        out.push(Finding {
            checker: UNVERIFIED_ANNOTATION.to_string(),
            tag: UNVERIFIED_ANNOTATION.to_string(),
            func: p.func_name(f).to_string(),
            pos: ac.pos.clone(),
            message: format!(
                "annotated ensures `{}` not proven against the body; callers still assume it",
                ac.text
            ),
            trace: Vec::new(),
            model: Vec::new(),
            severity: Severity::Warning,
        });
    };
    let (Some(func), Some(enc)) = (p.func(f), enc) else {
        for ac in &ann.ensures {
            warn(ac);
        }
        return out;
    };
    // Return sites, arity-checked (nil.rs pattern).
    let goverify_ir::TypeKind::Signature { results, .. } = p.types().kind(func.sig) else {
        for ac in &ann.ensures {
            warn(ac);
        }
        return out;
    };
    let results_len = results.len();
    let mut sites: Vec<(usize, Vec<goverify_ir::ValueId>)> = Vec::new();
    let mut malformed = false;
    for (bi, b) in func.blocks.iter().enumerate() {
        for ins in &b.instrs {
            if let Op::Return { vals } = &ins.op {
                if vals.len() != results_len {
                    malformed = true;
                }
                sites.push((bi, vals.clone()));
            }
        }
    }
    let arg_terms: Vec<Option<goverify_solver::Term>> =
        func.params.iter().map(|&v| enc.value(v).cloned()).collect();
    for ac in &ann.ensures {
        if malformed || sites.is_empty() {
            warn(ac);
            continue;
        }
        let tmp = Summary {
            ensures: vec![ac.clause.clone()],
            ..Summary::default()
        };
        let mut proven = true;
        for (bi, vals) in &sites {
            let result_terms: Vec<Option<goverify_solver::Term>> =
                vals.iter().map(|&v| enc.value(v).cloned()).collect();
            let bcs = crate::summary::instantiate_ensures(&tmp, &arg_terms, &result_terms);
            let Some(v) = bcs.into_iter().next().and_then(|bc| bc.violation) else {
                proven = false;
                break;
            };
            if discharge(&enc.reach_query(*bi, vec![v])) != SatResult::Unsat {
                proven = false;
                break;
            }
        }
        if !proven {
            warn(ac);
        }
    }
    out
}
```

(Adjust `instantiate_ensures`'s exact signature/return to what `summary.rs` defines — it is used by `encode_call_ensures` at encode.rs:831; if its `BoundClause` has no `violation`, negate `bound` with `Term::not` instead.)

- [ ] **Step 3: Engine findings-pass integration**

In `engine.rs`, inside the per-function findings block (the `catch_unwind` closure, after the `for checker in checkers` loop, before returning `per_func`):

```rust
                // Phase-6 annotation findings: contract call-site
                // obligations + ensures verification. Inside this block
                // so they enter the SCC payload and replay warm.
                let ann_of = |c: goverify_ir::FuncId| annotations.funcs.get(&c).cloned();
                if let Some(func) = p.func(f) {
                    if let Ok(enc) = crate::encode::encode_func_with(p, f, &summary_of) {
                        let own = summary_of(f);
                        for ob in crate::annotations::contract_obligations(
                            p, func, &enc, &own, &ann_of,
                        ) {
                            let outcome = discharge_query(
                                &ob.query, &mut *backend, cache.as_ref(), emit_dir.as_deref(),
                            );
                            if outcome.result == SatResult::Sat {
                                let trace = outcome.model.as_deref()
                                    .and_then(|m| trace_for(p, f, m)).unwrap_or_default();
                                let model = outcome.model.as_deref()
                                    .map(|m| crate::encode::model_bindings(m).into_iter()
                                        .filter(|(name, _)| is_param_binding(name))
                                        .collect())
                                    .unwrap_or_default();
                                per_func.push(Finding {
                                    checker: crate::annotations::CONTRACT.to_string(),
                                    tag: ob.tag.clone(),
                                    func: p.func_name(f).to_string(),
                                    pos: ob.pos,
                                    message: ob.message,
                                    trace,
                                    model,
                                    severity: Severity::Error,
                                });
                            }
                        }
                        if let Some(fa) = annotations.funcs.get(&f) {
                            let mut discharge = |q: &Query| {
                                discharge_query(q, &mut *backend, cache.as_ref(), emit_dir.as_deref())
                                    .result
                            };
                            per_func.extend(crate::annotations::verify_ensures(
                                p, f, Some(&enc), fa, &mut discharge,
                            ));
                        }
                    } else if let Some(fa) = annotations.funcs.get(&f) {
                        // Encoding failed (oversize): can't verify.
                        let mut discharge = |_: &Query| SatResult::Unknown;
                        per_func.extend(crate::annotations::verify_ensures(
                            p, f, None, fa, &mut discharge,
                        ));
                    }
                }
```

Also handle bodyless annotated functions with ensures: after the `p.func_ids()` findings loop (still cache-aware — bodyless funcs have no SCC entry of their own? They DO appear in sccs — check `Sccs::compute`; if bodyless functions get findings slots, put the `p.func(f).is_none() && annotations.funcs.contains_key(&f)` warn-all case inside the same per-function block, mirroring the `else` arm above with `enc = None`).

**Note the guard interaction:** the findings pass runs only when `!checkers.is_empty()` — acceptable: no-checker runs are debug paths (`analyze`), and annotation findings there are not promised. State this in a comment.

- [ ] **Step 4: Salt**

`scc_cache.rs` `CacheConfigKey` gains `pub annotation_version: u32`; in `SccCache::open`, after `widen_after`:

```rust
        h.update(&cfg.annotation_version.to_le_bytes());
```

`engine.rs` populates it from the new `EngineConfig.annotation_version: u32` field (add to the struct with `Default` = 0). Fix the `CacheConfigKey` literal in `analyze_full` and any test literals.

- [ ] **Step 5: Tests**

Engine tests (testpkg style, hand-built `Annotations` — no goverify-spec dependency; build `AnnClause` with `Term::var("p0", ...)` formulas):

1. **Contract fires:** callee with annotated requires `p0 != (bv 0)` (or ptr non-nil), caller passing a violating constant → exactly one finding, `checker == "contract"`, `severity == Error`, message contains the annotation text.
2. **Contract satisfied:** compliant caller → no contract finding.
3. **Ensures verified:** function returning a constant satisfying the annotated ensures → no `unverified-annotation` finding.
4. **Ensures unprovable:** violated ensures → exactly one Warning at the given pragma pos.
5. **Warm replay:** with a cache dir, run `analyze_full` twice on the same program+annotations; second run findings (incl. contract + unverified) identical and `scc_cache_hits > 0`. Mirror the structure of existing scc-cache engine tests.

Run: `mise x -- cargo test -p goverify-analysis`. Expected: PASS.

- [ ] **Step 6: Lint + workspace test + commit**

`mise run lint && mise x -- cargo test --workspace`; commit `phase6: contract call-site pass + ensures verification + annotation cache salt`.

---

### Task 9: CLI — severity surface (exit code, --deny warnings, render, JSON v2, SARIF level)

**Files:**
- Modify: `crates/goverify-cli/src/main.rs` (flag, promotion, exit predicate)
- Modify: `crates/goverify-cli/src/render.rs` (warning token)
- Modify: `crates/goverify-cli/src/json.rs` (severity, schema 2, suppressed_pragma field)
- Modify: `crates/goverify-cli/src/sarif.rs` (level from severity)
- Modify: `crates/goverify-cli/tests/report_integration.rs`, `tests/cli.rs`, goldens
- Test: in-file unit tests

**Interfaces:**
- Consumes: `Finding.severity` (Task 3).
- Produces: `--deny warnings` (repeatable `--deny <CLASS>`, one class for now); exit 1 iff any kept finding has `severity == Error` (after promotion); `JSON_SCHEMA_VERSION = 2`; JSON per-finding `"severity": "error"|"warning"` and summary `suppressed_pragma` (wired to 0 until Task 10); SARIF `level` from severity.

- [ ] **Step 1: Flag + promotion + exit**

`CheckArgs` gains:

```rust
    /// Promote finding classes for CI gating. `--deny warnings` makes
    /// warning-severity findings (unverified-annotation) fail the run
    /// and report as errors in every format.
    #[arg(long, value_enum, value_name = "CLASS")]
    deny: Vec<DenyClass>,
```

```rust
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DenyClass {
    Warnings,
}
```

In `run_check`, after the baseline filter (Task 10 will settle the final position after both suppression filters) and before summary construction:

```rust
    let mut scoped = scoped;
    if ca.deny.contains(&DenyClass::Warnings) {
        for f in &mut scoped {
            f.severity = goverify_analysis::Severity::Error;
        }
    }
```

Exit predicate (main.rs:679-683) becomes:

```rust
    Ok(if scoped.iter().any(|f| f.severity == goverify_analysis::Severity::Error) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
```

- [ ] **Step 2: Human renderer**

In `render.rs` `render_one` (line 48): error findings keep the EXACT current format (bbolt G1 byte-identity); warnings get a `warning: ` marker:

```rust
    let sev = match f.severity {
        Severity::Error => "",
        Severity::Warning => "warning: ",
    };
    lines.push(format!("{pos_str}: {sev}{}: {} [{}]", f.tag, f.message, f.func));
```

Existing frozen in-file goldens (render.rs:164+) stay green (all Error). Add one warning-finding test pinning the `warning: ` form.

- [ ] **Step 3: JSON schema 2**

`json.rs`: `JSON_SCHEMA_VERSION` → 2. `JsonFinding` gains `severity: &'static str` (populate `"error"`/`"warning"`). `Summary` gains `pub suppressed_pragma: usize` (declared after `suppressed_by_baseline`); `main.rs:658` passes `suppressed_pragma: 0` for now. Update in-file goldens (`json.rs:130-162`) and the `OutputFormat::Json` doc comment (`main.rs:67`) to say schema_version 2.

- [ ] **Step 4: SARIF level**

`sarif.rs:184`: `level: "warning"` → per-finding:

```rust
                level: match f.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                },
```

**Deliberate behavior change:** existing findings (all Error) flip SARIF level `"warning"` → `"error"`. Correct per SARIF semantics; documented in the shakeout addendum (Task 14) as the one non-additive machine-format change.

- [ ] **Step 5: Update integration tests + goldens**

- `report_integration.rs`: `"schema_version": 1` → 2 (lines 117, 250); severity fields in asserted JSON snippets; SARIF level assertions.
- Regenerate `tests/goldens/hello_check.json` / `hello_check.sarif`: run the formats_corpus `check()` path manually (`mise x -- cargo test -p goverify-cli --test formats_corpus` will print the diff; copy actual bytes into the goldens after eyeballing: schema_version 2, `suppressed_pragma: 0`, SARIF unchanged results-empty shape). Verify no absolute paths in the new bytes.
- `tests/cli.rs`: `--help` surface includes `--deny <CLASS>`.

Run: `mise x -- cargo test -p goverify-cli`. Expected: PASS.

- [ ] **Step 6: Commit**

`phase6: severity CLI surface (--deny warnings, exit on errors only, JSON v2, SARIF levels)`.

---

### Task 10: CLI — wire the compiler; pragma-ignore filter; SARIF suppressions

**Files:**
- Modify: `crates/goverify-cli/Cargo.toml` (goverify-spec dep)
- Modify: `crates/goverify-cli/src/main.rs` (compile+thread, ignore filter, scope exemption)
- Modify: `crates/goverify-cli/src/sarif.rs` (suppressions array)
- Modify: `crates/goverify-cli/src/json.rs` (wire suppressed_pragma)
- Test: unit tests + `report_integration.rs` additions

**Interfaces:**
- Consumes: `goverify_spec::{compile_program, ANNOTATION_VERSION}`, `Annotations` (Task 6), `goverify_checkers` checker list.
- Produces:
  - `goverify_checkers::default_checkers() -> Vec<&'static dyn Checker>` (new, in goverify-checkers `lib.rs`) — single source for the CLI's checker vec and the valid-ignore-names list.
  - Filter chain (final): scope → diff-base → fingerprints → **pragma-ignore** → baseline → **deny promotion** → render/exit.
  - `apply_baseline` and the new `apply_pragma_ignores` both return their suppressed findings (with fingerprints) for SARIF.
  - SARIF: suppressed findings are now EMITTED as results carrying `suppressions: [{"kind": "inSource"}]` (pragma) / `[{"kind": "external"}]` (baseline) — previously baseline-suppressed results were omitted entirely; this is the spec-mandated behavior change. `RunProperties` gains `suppressed_by_pragma`.

- [ ] **Step 1: default_checkers + compile wiring**

In `crates/goverify-checkers/src/lib.rs`:

```rust
/// The production checker set — single source for the CLI and for
/// validating `//goverify:ignore` names.
pub fn default_checkers() -> Vec<&'static dyn goverify_analysis::Checker> {
    vec![&NilChecker, &BoundsChecker]
}
```

`main.rs:471-476` uses it. In `analyze_module`, after `program` is loaded and before `analyze_full`:

```rust
    let checkers = goverify_checkers::default_checkers();
    let known: Vec<&str> = checkers
        .iter()
        .map(|c| c.name())
        .chain([
            goverify_analysis::CONTRACT,
            goverify_analysis::BAD_ANNOTATION,
            goverify_analysis::UNVERIFIED_ANNOTATION,
        ])
        .collect();
    let ann = goverify_spec::compile_program(&program, &known);
    let pre_findings = ann.findings.clone();
    let pragma_ignores: Vec<(String, String)> = ann
        .funcs
        .iter()
        .flat_map(|(f, fa)| {
            let name = program.func_name(*f).to_string();
            fa.ignores.iter().map(move |n| (name.clone(), n.clone()))
        })
        .collect();
    cfg.annotations = ann;
    cfg.annotation_version = goverify_spec::ANNOTATION_VERSION;
```

After `analyze_full`, append `pre_findings` to `a.findings` BEFORE the scope filter, and exempt them from scoping — in `scope_findings`/`in_module`, keep any finding with `checker == goverify_analysis::BAD_ANNOTATION` unconditionally (config errors must never be scope-filtered; add a comment). Thread `pragma_ignores` out through the `Analyzed` struct (add field `pragma_ignores: Vec<(String, String)>`).

- [ ] **Step 2: apply_pragma_ignores + apply_baseline return suppressed**

In `main.rs`, mirror `apply_baseline`'s shape:

```rust
/// Pragma-ignore filtering (phase-6 spec §5): (func, checker) pairs
/// from //goverify:ignore. Runs AFTER fingerprints (it may split
/// identical-sibling groups) and BEFORE baseline (a finding matching
/// both counts once, under ignore). Returns (kept, kept_fps,
/// suppressed-with-fps).
#[allow(clippy::type_complexity)]
fn apply_pragma_ignores(
    ignores: &[(String, String)],
    findings: Vec<goverify_analysis::Finding>,
    fps: Vec<String>,
) -> (Vec<goverify_analysis::Finding>, Vec<String>, Vec<(goverify_analysis::Finding, String)>) {
    if ignores.is_empty() {
        return (findings, fps, Vec::new());
    }
    let set: std::collections::HashSet<(&str, &str)> =
        ignores.iter().map(|(f, c)| (f.as_str(), c.as_str())).collect();
    let mut kept = Vec::new();
    let mut kept_fps = Vec::new();
    let mut suppressed = Vec::new();
    for (f, fp) in findings.into_iter().zip(fps) {
        if set.contains(&(f.func.as_str(), f.checker.as_str())) {
            suppressed.push((f, fp));
        } else {
            kept.push(f);
            kept_fps.push(fp);
        }
    }
    (kept, kept_fps, suppressed)
}
```

Change `apply_baseline`'s return to `(Vec<Finding>, Vec<String>, Vec<(Finding, String)>)` (suppressed findings + fps instead of a bare count); update its doc and the counting at the call site (`suppressed_by_baseline = suppressed.len()`).

`run_check` filter chain becomes:

```rust
    let fps = goverify_cli::fingerprint::fingerprints(&scoped);
    let (scoped, fps, sup_pragma) = apply_pragma_ignores(&pragma_ignores, scoped, fps);
    let (scoped, fps, sup_baseline) = apply_baseline(&ca, scoped, fps)?;
    let mut scoped = scoped;
    if ca.deny.contains(&DenyClass::Warnings) { /* Task 9 promotion */ }
    let summary = json::Summary {
        total: scoped.len(),
        suppressed_by_baseline: sup_baseline.len(),
        suppressed_pragma: sup_pragma.len(),
        diff_base_scoped,
    };
```

Human arm gains:

```rust
            if !sup_pragma.is_empty() {
                println!("goverify: pragma: {} finding(s) suppressed", sup_pragma.len());
            }
```

(before the baseline line, matching filter order).

- [ ] **Step 3: SARIF suppressions**

`sarif.rs`:

```rust
#[derive(Serialize)]
struct Suppression {
    kind: &'static str, // "inSource" (pragma) | "external" (baseline)
}
```

`SarifResult` gains:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    suppressions: Option<[Suppression; 1]>,
```

`render_sarif` signature becomes:

```rust
pub fn render_sarif(
    findings: &[Finding],
    fps: &[String],
    sup_pragma: &[(Finding, String)],
    sup_baseline: &[(Finding, String)],
) -> String
```

Emit results in this deterministic order: kept findings (`suppressions: None`), then pragma-suppressed (`Some([Suppression { kind: "inSource" }])`), then baseline-suppressed (`"external"`), each group in input order, reusing the existing per-finding result construction (factor it into a helper taking `(f, fp, suppressions)`). `RunProperties`:

```rust
struct RunProperties {
    suppressed_by_baseline: usize,
    suppressed_by_pragma: usize,
}
```

Update `main.rs:671` call site. The rule table (`RULES`) gains the three annotation classes:

```rust
    ("contract", "annotated requires violated at a call site"),
    ("bad-annotation", "invalid //goverify: annotation"),
    ("unverified-annotation", "annotated ensures not proven against the body"),
```

(only emitted when referenced, matching existing driver-rules construction).

- [ ] **Step 4: Tests**

- Unit: `apply_pragma_ignores` keeps/splits correctly (incl. two identical siblings where one function is ignored — fingerprints computed BEFORE the filter stay stable); both-match counts under ignore (construct a baseline set containing an ignored finding's fp; assert baseline suppressed-count excludes it).
- `sarif.rs` in-file test: one kept + one pragma-suppressed + one baseline-suppressed finding → assert `suppressions` arrays and properties counts; determinism (two renders byte-identical).
- `report_integration.rs`: existing baseline test now also asserts the suppressed results appear in SARIF with `"kind": "external"`.

Run: `mise x -- cargo test -p goverify-cli`. Expected: PASS. Regenerate `hello_check.sarif` golden if the properties shape changed (`suppressedByPragma: 0` appears).

- [ ] **Step 5: Commit**

`phase6: wire annotation compiler; pragma-ignore filter; SARIF suppression kinds`.

---

### Task 11: Corpus fixture module `annot` + corpus/e2e tests + golden regen

**Files:**
- Create: `testdata/corpus/annot/go.mod`, `contract.go`, `verify.go`, `bad.go`, `suppress.go`
- Create: `crates/goverify-spec/tests/annot_corpus.rs`
- Create: `crates/goverify-cli/tests/annot_integration.rs`
- Modify: `mise.toml` (`[tasks.corpus]` — add both test targets)

**Interfaces:** end-to-end validation of Tasks 1–10; the fixture is also the extractor regression surface for result names/method pragmas.

- [ ] **Step 1: Fixture module**

`testdata/corpus/annot/go.mod`:

```
module example.com/annot

go 1.25
```

`contract.go`:

```go
// Package annot is the phase-6 annotation-language corpus module.
package annot

//goverify:requires n >= 1
func Positive(n int) int { return n }

func CallsPositiveBad() int {
	return Positive(0) // want: contract
}

func CallsPositiveOK() int {
	return Positive(2)
}
```

`verify.go`:

```go
package annot

//goverify:ensures ret >= 1
func One() int { return 1 }

// Zero's ensures is FALSE: pins the unverified-annotation warning at
// this pragma's line (bespoke assertion — pragma lines can't carry
// want-pins).
//goverify:ensures ret >= 1
func Zero() int { return 0 }

// Named results resolve by declared name (extractor result_names).
//goverify:ensures err == nil ==> ret >= 0
func Checked(n int) (ret int, err error) {
	if n < 0 {
		return -1, errAnnot
	}
	return n, nil
}

var errAnnot = &annotError{}

type annotError struct{}

func (*annotError) Error() string { return "annot" }
```

(`Checked` is verification-best-effort: if the engine can't prove it, the pinned expectation below treats it as *may-warn* — see Step 3 contingency.)

`bad.go`:

```go
package annot

//goverify:requires q != nil
func UnknownName(p *int) bool { return p == nil }

//goverify:requires ret != 0
func ResultInRequires(p int) (ret int) { return p }

//goverify:requires p.buf != nil
func FieldSel(p *int) bool { return p != nil }

//goverify:effects locks(mu)
func Effects() {}

//goverify:pure
func Pure() {}

//goverify:frobnicate
func UnknownDirective() {}

//goverify:ignore no-such-checker
func BadIgnore() {}

//goverify:requires x >
func ParseError(x int) {}
```

(**Every fixture file must compile** or the whole file is silently skipped and every pin masked — run `go build ./...` in the module to verify before committing.)

`suppress.go`:

```go
package annot

func maybe() *int { return nil }

// The unsuppressed twin pins that the finding exists at all.
func Reported() int {
	p := maybe()
	return *p // want: nil-deref
}

//goverify:ignore nil
func Suppressed() int {
	p := maybe()
	return *p
}
```

(**Verify the tag**: the pin must use the checker's finding TAG (`nil-deref`), while `ignore` names the CHECKER (`nil`) — checker `name()` is `"nil"`, clause tag is `"nil-deref"`. If `Reported` doesn't actually fire under the nil checker's templates, adapt the body to a shape pinned in the `nil` corpus — copy a firing shape from `testdata/corpus/nil/nil.go`.)

- [ ] **Step 2: Corpus analysis test**

`crates/goverify-spec/tests/annot_corpus.rs` — mirror the harness of `crates/goverify-checkers/tests/nil_corpus.rs` (same backend construction, same `analyze_full` invocation, plus `cfg.annotations` from `compile_program`); assertions:

```rust
// Sketch — mirror nil_corpus.rs's solver/backend setup exactly.
let p = goverify_ir::testutil::load_corpus("annot");
let known = ["nil", "bounds", "contract", "bad-annotation", "unverified-annotation"];
let ann = goverify_spec::compile_program(&p, &known);

// (1) bad.go: exactly these bad-annotation findings (bespoke tuples —
// pragma lines can't carry want-pins). Lines are the PRAGMA lines.
let bad: Vec<(String, u32)> = ann
    .findings
    .iter()
    .filter(|f| f.checker == "bad-annotation")
    .map(|f| {
        let pos = f.pos.as_ref().expect("bad-annotation carries pragma pos");
        (pos.file.clone(), pos.line)
    })
    .collect();
assert_eq!(bad.len(), 8, "one per bad.go pragma: {bad:?}");

// (2) ignore set compiled and validated.
let f = p.lookup_func("example.com/annot.Suppressed").unwrap();
assert_eq!(ann.funcs[&f].ignores, vec!["nil".to_string()]);

// (3) run the engine with annotations; want-pins cover contract +
// nil-deref; bespoke tuple covers Zero's warning.
let a = /* analyze_full as in nil_corpus.rs, cfg.annotations = ann */;
let got: BTreeSet<(String, u32, String)> = a.findings.iter()
    .filter(|f| f.func.contains("example.com/annot"))
    .filter(|f| f.checker != "unverified-annotation") // bespoke below
    .filter_map(|f| f.pos.as_ref().map(|p| (p.file.clone(), p.line, f.tag.clone())))
    .collect();
let want: BTreeSet<(String, u32, String)> =
    goverify_ir::testutil::wants("annot").into_iter().collect();
assert_eq!(got, want, "findings vs want pins");

// (4) Zero's ensures warning at its pragma line; One clean.
let warns: Vec<&Finding> = a.findings.iter()
    .filter(|f| f.checker == "unverified-annotation").collect();
assert!(warns.iter().any(|f| f.func.ends_with("annot.Zero")), "Zero warns");
assert!(!warns.iter().any(|f| f.func.ends_with("annot.One")), "One verified");
assert!(warns.iter().all(|f| f.severity == Severity::Warning));

// (5) Positive's summary carries the contract clause.
let fp = p.lookup_func("example.com/annot.Positive").unwrap();
assert!(a.summaries[&fp].requires.iter()
    .any(|c| c.tag == "contract" && c.provenance == Provenance::Annotated));
```

Add `goverify-checkers`, `goverify-solver` (whatever nil_corpus.rs uses) as dev-dependencies of goverify-spec. **Contingency (state in the test as a comment):** `Checked`'s implication may verify or not depending on solver strength — assert nothing about it beyond "if it warns, the warning is at its pragma line" (it exists to exercise named-result resolution, asserted via `compile_program` output: `ann.funcs[&checked].ensures.len() == 1`).

- [ ] **Step 3: CLI e2e test**

`crates/goverify-cli/tests/annot_integration.rs` — follow `formats_corpus.rs`'s `check()` helper pattern (one extraction, `XDG_CACHE_HOME` tempdir):

1. `check annot` (human): exit 1 (contract + bad-annotation errors present); stdout contains `goverify: pragma: 1 finding(s) suppressed` and a `warning: unverified-annotation` line for Zero; no `nil-deref` finding inside `Suppressed`.
2. `check annot --format json`: `schema_version: 2`; `summary.suppressed_pragma == 1`; Zero's finding has `"severity": "warning"`.
3. `check annot --format sarif`: exactly one result with `"suppressions"` containing `"kind": "inSource"`.
4. `--deny warnings` on a fixture WITHOUT error findings — copy `verify.go`'s One/Zero into a tempdir mini-module (no bad.go/contract.go) — exit 0 without the flag, exit 1 with it.

- [ ] **Step 4: mise task + goldens**

`mise.toml` `[tasks.corpus]` gains:

```toml
  "cargo test -p goverify-spec --test annot_corpus",
  "cargo test -p goverify-cli --test annot_integration",
```

Run `mise run corpus` — hello goldens: hello's `p != nil` dedups against the inferred nil-deref requires, no callers violate, no ensures → hello stays finding-empty; goldens already regenerated for schema v2 in Tasks 9–10, so expect NO further churn here. If hello churns, STOP and investigate (dedup rule regression).

- [ ] **Step 5: Commit**

`phase6: annot corpus module + e2e annotation tests`.

---

### Task 12: Fuzz target #7 — annotation_parse

**Files:**
- Create: `fuzz/fuzz_targets/annotation_parse.rs`
- Modify: `fuzz/Cargo.toml`
- Create: `fuzz/seeds/annotation_parse/` (seed files)
- Modify: `.github/workflows/nightly.yml`
- Modify: `mise.toml` (`[tasks.fuzz]`)

- [ ] **Step 1: Target**

`fuzz/fuzz_targets/annotation_parse.rs`:

```rust
//! Parse arbitrary bytes as a //goverify: pragma line. Annotation text
//! is repo-authored but untrusted (parent spec §11; phase-6 spec §7) —
//! the parser must reject, never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = goverify_spec::parse::parse_pragma(s);
    }
});
```

`fuzz/Cargo.toml`: add `goverify-spec = { path = "../crates/goverify-spec" }` to `[dependencies]` and a `[[bin]]` block mirroring `baseline_parse` (`name = "annotation_parse"`, `path = "fuzz_targets/annotation_parse.rs"`, `test = false`, `doc = false`, `bench = false`).

Seeds (`fuzz/seeds/annotation_parse/`): five files containing the spec's example pragmas (one per file, exact bytes like `//goverify:requires p != nil && n >= 0`).

- [ ] **Step 2: CI budget + local smoke**

`.github/workflows/nightly.yml` fuzz job: add

```yaml
      - run: mkdir -p fuzz/corpus/annotation_parse
      - run: cargo +nightly fuzz run annotation_parse fuzz/corpus/annotation_parse fuzz/seeds/annotation_parse -- -max_total_time=900
```

(match the exact arg shape of the `ir_encode` step, which also passes corpus+seed dirs). Bump `timeout-minutes: 120` → `135` and update the budget comment (seven 900s runs = 105 min + ~20 min cold Z3 build + headroom).

`mise.toml` `[tasks.fuzz]`: add the same run at `-max_total_time=60`.

- [ ] **Step 3: Smoke run + commit**

```bash
mise x -- cargo +nightly fuzz run annotation_parse fuzz/corpus/annotation_parse fuzz/seeds/annotation_parse -- -max_total_time=60
```

Expected: 0 crashes (EDR-stall caveat: if the fresh binary stalls ~50 min, park and continue — memory `sentinelone-exec-stall`). Commit `phase6: annotation_parse fuzz target (#7) + nightly budget`.

---

### Task 13: Cache & diff-base invalidation regression tests

**Files:**
- Modify: `crates/goverify-checkers/tests/scc_cache_invalidation.rs`
- Modify: `crates/goverify-ir/tests/lower_corpus.rs` (or the test file holding semantic-hash tests)

**Interfaces:** pins the two invariants the design leans on: pragma edits rotate SCC keys (`ctx_hash`), and pragma edits mark functions changed for `--diff-base` (`semantic_ctx_hash`) while pragma position shifts do NOT.

- [ ] **Step 1: SCC-cache pragma-edit miss**

In `scc_cache_invalidation.rs`, following the file's existing copy-module-to-tempdir/edit/re-extract structure: cold-run the `annot` (or `hello`) module with a cache dir → warm-run: all hits. Edit ONLY a pragma line's text (e.g. `//goverify:requires p != nil` → `//goverify:requires p != nil && true` — still parses), re-extract, run again → assert `scc_cache_misses > 0` (the edited package's SCCs miss; whole-package granularity is EXPECTED — pragma bytes are in every member's ctx hash — note it in the test comment).

- [ ] **Step 2: Semantic-hash behavior**

In the goverify-ir test file: extract the same module twice into tempdirs — variant A unchanged; variant B with a blank line inserted ABOVE the pragma (position shift only); variant C with the pragma text edited. Assert for the annotated function: `func_semantic_hash(A) == func_semantic_hash(B)` (comment shifts don't mark it changed for --diff-base) and `!= func_semantic_hash(C)` (annotation edits do). Also `func_ir_hash(A) != func_ir_hash(B)` (position-sensitive, cache key). Reuse `testutil::load_module` for each variant; this costs three sidecar extractions — keep all three variants in ONE test to reuse the built sidecar (EDR hazard).

- [ ] **Step 3: Commit**

`mise x -- cargo test -p goverify-checkers --test scc_cache_invalidation -p goverify-ir`; commit `phase6: pragma cache/diff-base invalidation pins`.

---

### Task 14: Docs + bbolt shakeout gates + addendum

**Files:**
- Modify: `README.md` (annotations section)
- Modify: `ARCHITECTURE.md` (goverify-spec is real; dependency direction)
- Modify: `docs/superpowers/specs/2026-07-25-phase6-annotations-design.md` (record the CLI-compiles dependency-direction deviation, one paragraph in §3)
- Create: shakeout addendum (follow the phase-5b addendum's location/format)

- [ ] **Step 1: README**

Add an "Annotations" section after the baseline/CI section: the three pragmas with the spec's exact examples, the severity story (`unverified-annotation` is a warning; `--deny warnings` for CI), `ignore` vs baseline (in-source vs external suppression; SARIF kinds), and the never-silent rule (bad annotations are error findings). Copy syntax lines verbatim from the design spec §2.

- [ ] **Step 2: ARCHITECTURE**

Update the `goverify-spec` row: "annotation compiler: parse → resolve → lower; depends on ir+solver+analysis; the CLI compiles and passes `Annotations` into the engine (no analysis→spec edge)". Update the crate-graph description accordingly.

- [ ] **Step 3: bbolt shakeout**

Re-pin bbolt at the same ref as the phase-5b addendum. Run from a bbolt checkout (mirror the 5b addendum's exact commands):

- **G1 (findings identical):** `goverify check` (human) vs base-commit binary: 457 findings, identical fingerprints (`goverify baseline write` on both, diff the files), exit 1 both. Human output **byte-identical** (bbolt has no pragmas: no warnings, no suppressed lines, error lines unchanged).
- **G2 (machine formats):** cold + 3 warm `--format json` and `--format sarif` runs byte-identical to each other; diff vs base limited to: `schema_version` 2, `severity` fields, `suppressed_pragma`/`suppressedByPragma` zeros, the three new SARIF rules, and SARIF `level` `"warning"`→`"error"` (the documented non-additive change).
- **G3 (report-only):** warm wall-clock vs the 3.35 s phase-5b baseline; annotation compilation with zero pragmas must be noise-level.
- **G4 (dogfood):** in a bbolt WORKING COPY (never the pinned tree), add `//goverify:requires` to one function with a known call-site relationship; demonstrate: the annotated clause appears in `debug summary` output, and a deliberately violating call produces a `contract` finding. Record commands + output in the addendum.

- [ ] **Step 4: Addendum + final gate + commit**

Write the addendum (gates table, deviations, the SARIF-level change callout). Run the full blocking gate:

```bash
mise run lint && mise run test && mise run secrets && mise run audit
```

Commit `phase6: shakeout addendum (G1-G4) + README/ARCHITECTURE annotation docs`.

---

## Follow-up queue (plan owner; do NOT implement)

- 6b wave: `effects`/`pure` pragmas, field selection (interface-level heap terms), `.gvspec` third-party overrides (`goverify/overrides/<import/path>.gvspec`; the `encode.rs:879` extern-ctor allowlist comment marks the seam).
- `--deny` is accepted-and-ignored by `baseline write` (same pre-existing pattern as `--format`; queued with it).
- `debug findings` uses `vec![&NilChecker]`, not `default_checkers()` (pre-existing divergence, now more visible).
- Arithmetic in annotation expressions — add when a fixture or user needs it.
- Generic-origin pragma fan-out matches by `id.starts_with(decl_id + "[")` — revisit when a generics corpus fixture lands (phase-5a follow-up §16).
