package main

import (
	"crypto/sha256"
	"encoding/hex"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"

	"golang.org/x/tools/go/packages"
	"golang.org/x/tools/go/ssa"

	"goverify.dev/extractor/gvirpb"
)

const (
	schemaVersion    = "1"
	extractorVersion = "0.1.0"
)

// emitter builds one canonical gvirpb.Package. All interning maps are
// filled in deterministic walk order, so the assigned ids are stable.
type emitter struct {
	fset    *token.FileSet
	pkg     *packages.Package
	out     *gvirpb.Package
	typeIDs map[string]uint32
	fileIDs map[string]uint32
}

func extractPackage(fset *token.FileSet, p *packages.Package, sp *ssa.Package, fns []*ssa.Function) *gvirpb.Package {
	e := &emitter{
		fset:    fset,
		pkg:     p,
		typeIDs: map[string]uint32{},
		fileIDs: map[string]uint32{},
		out: &gvirpb.Package{
			SchemaVersion:    schemaVersion,
			GoVersion:        runtime.Version(),
			ExtractorVersion: extractorVersion,
			ImportPath:       p.PkgPath,
		},
	}
	e.emitFiles()
	_ = sp
	_ = fns // Task 4: function emission; Task 5: method sets, pragmas, canonicalize
	return e.out
}

// relPath rewrites a source filename to a stable machine-independent
// form: module-root-relative when inside a module (covers the target
// module and module-cache deps alike), $GOROOT-relative for stdlib,
// otherwise the base name. Never absolute (spec §3).
func (e *emitter) relPath(filename string) string {
	if m := e.pkg.Module; m != nil && m.Dir != "" {
		if r, err := filepath.Rel(m.Dir, filename); err == nil && !strings.HasPrefix(r, "..") {
			return filepath.ToSlash(r)
		}
	}
	if r, err := filepath.Rel(runtime.GOROOT(), filename); err == nil && !strings.HasPrefix(r, "..") {
		return "$GOROOT/" + filepath.ToSlash(r)
	}
	return filepath.Base(filename)
}

func (e *emitter) emitFiles() {
	abs := map[string]string{} // rel -> abs
	rels := make([]string, 0, len(e.pkg.CompiledGoFiles))
	for _, f := range e.pkg.CompiledGoFiles {
		r := e.relPath(f)
		rels = append(rels, r)
		abs[r] = f
	}
	slices.Sort(rels)
	for i, r := range rels {
		sum := ""
		if b, err := os.ReadFile(abs[r]); err == nil {
			h := sha256.Sum256(b)
			sum = hex.EncodeToString(h[:])
		}
		e.out.Files = append(e.out.Files, &gvirpb.File{Path: r, Sha256: sum})
		e.fileIDs[r] = uint32(i + 1)
	}
}

// typeID interns a type by its fully-qualified canonical string.
// Ids are first-encounter order — deterministic because every walk
// that reaches here is deterministic.
func (e *emitter) typeID(t types.Type) uint32 {
	repr := types.TypeString(t, func(p *types.Package) string { return p.Path() })
	if id, ok := e.typeIDs[repr]; ok {
		return id
	}
	id := uint32(len(e.typeIDs) + 1)
	e.typeIDs[repr] = id
	e.out.Types = append(e.out.Types, &gvirpb.Type{Id: id, Repr: repr})
	return id
}

func (e *emitter) position(pos token.Pos) *gvirpb.Position {
	if pos == token.NoPos {
		return nil
	}
	p := e.fset.Position(pos)
	return &gvirpb.Position{
		File: e.fileIDs[e.relPath(p.Filename)], // 0 if not a package file
		Line: uint32(p.Line),
		Col:  uint32(p.Column),
	}
}
