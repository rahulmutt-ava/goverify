package main

import (
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"golang.org/x/tools/go/packages"
	"golang.org/x/tools/go/ssa"
	"golang.org/x/tools/go/ssa/ssautil"
	"google.golang.org/protobuf/proto"
)

// run extracts the packages matched by patterns (resolved in dir; ""
// means cwd) into outDir, one .gvir per package. With deps, the whole
// import closure is emitted. Returns the sorted list of written paths.
func run(dir string, patterns []string, outDir string, deps bool) ([]string, error) {
	cfg := &packages.Config{
		Dir: dir,
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedCompiledGoFiles |
			packages.NeedImports | packages.NeedDeps | packages.NeedTypes |
			packages.NeedSyntax | packages.NeedTypesInfo | packages.NeedTypesSizes |
			packages.NeedModule,
		// cgo-generated files embed machine-specific paths, which would
		// break byte-identical determinism (spec §3).
		Env: append(os.Environ(), "CGO_ENABLED=0"),
	}
	roots, err := packages.Load(cfg, patterns...)
	if err != nil {
		return nil, err
	}
	// go/packages surfaces two very different "nothing here" situations,
	// and only one of them is fatal:
	//
	//  - An unresolvable pattern (e.g. a dir with no go.mod, or a bogus
	//    import path) yields one or more synthetic placeholder roots:
	//    Errors is set, but GoFiles/CompiledGoFiles/OtherFiles/
	//    IgnoredFiles are ALL empty — there was never a real package
	//    behind it. That's "no packages matched" (fatal).
	//  - A glob pattern (e.g. "./...") that legitimately expands to zero
	//    packages (every candidate's files are excluded by build
	//    constraints, or there are simply no Go files under it) returns
	//    zero roots at all, with no error — a real, empty result, not a
	//    failure (spec §11: degrade, never die). len(roots) == 0 alone
	//    must NOT be treated as fatal.
	//
	// A root that legitimately matched a real package but is entirely
	// excluded by build constraints (e.g. pattern "." on such a package)
	// also has zero GoFiles/CompiledGoFiles, but non-empty OtherFiles/
	// IgnoredFiles — that's how it's told apart from a true placeholder.
	// It still carries an Errors entry, so the per-package degrade loop
	// below skips it with a diagnostic rather than emitting it.
	if len(roots) > 0 {
		anyReal := false
		for _, p := range roots {
			if len(p.GoFiles) > 0 || len(p.CompiledGoFiles) > 0 || len(p.OtherFiles) > 0 || len(p.IgnoredFiles) > 0 {
				anyReal = true
				break
			}
		}
		if !anyReal {
			return nil, errors.New("no packages matched")
		}
	}

	targets := roots
	if deps {
		targets = nil
		packages.Visit(roots, nil, func(p *packages.Package) { targets = append(targets, p) })
	}

	prog, _ := ssautil.AllPackages(roots, ssa.BuilderMode(0))
	prog.Build()

	// Group every function (incl. anon funcs, methods, wrappers) by
	// package, sorted by stable id BEFORE emission — type-table ids are
	// first-encounter order, so emission order must be deterministic.
	fnsByPkg := map[*ssa.Package][]*ssa.Function{}
	for fn := range ssautil.AllFunctions(prog) {
		if fn.Pkg != nil {
			fnsByPkg[fn.Pkg] = append(fnsByPkg[fn.Pkg], fn)
		}
	}
	for _, fns := range fnsByPkg {
		slices.SortFunc(fns, func(a, b *ssa.Function) int {
			return strings.Compare(a.String(), b.String())
		})
	}

	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return nil, err
	}
	var written []string
	for _, p := range targets {
		if p.PkgPath == "unsafe" {
			continue // no SSA representation
		}
		// Degrade, never die (spec §11): skip broken packages with a
		// diagnostic; callers see them as absent.
		if len(p.Errors) > 0 {
			fmt.Fprintf(os.Stderr, "goverify: skipping %s: %v\n", p.PkgPath, p.Errors[0])
			continue
		}
		sp := prog.Package(p.Types)
		if sp == nil {
			fmt.Fprintf(os.Stderr, "goverify: skipping %s: no SSA package\n", p.PkgPath)
			continue
		}
		pb := extractPackage(prog.Fset, p, sp, fnsByPkg[sp])
		raw, err := proto.MarshalOptions{Deterministic: true}.Marshal(pb)
		if err != nil {
			return nil, fmt.Errorf("marshal %s: %w", p.PkgPath, err)
		}
		path := filepath.Join(outDir, url.PathEscape(p.PkgPath)+".gvir")
		if err := os.WriteFile(path, raw, 0o644); err != nil {
			return nil, err
		}
		written = append(written, path)
	}
	slices.Sort(written)
	return written, nil
}
