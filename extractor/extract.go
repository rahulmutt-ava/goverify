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
	// go/packages reports an unresolvable pattern (e.g. a dir with no
	// go.mod) as a single synthetic placeholder package carrying a
	// ListError and no files, not as zero roots — so len(roots) == 0
	// alone doesn't catch it. Distinguish that from a real package that
	// merely has parse/type errors (degraded per spec §11 in the loop
	// below) by requiring at least one root to have actual source files.
	anyFiles := false
	for _, p := range roots {
		if len(p.GoFiles) > 0 || len(p.CompiledGoFiles) > 0 {
			anyFiles = true
			break
		}
	}
	if !anyFiles {
		return nil, errors.New("no packages matched")
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
