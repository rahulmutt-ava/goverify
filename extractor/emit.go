package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
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
	for _, fn := range fns {
		e.out.Functions = append(e.out.Functions, e.emitFunction(fn))
	}
	_ = sp // Task 5: method sets, pragmas, canonicalize
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

// emitFunction serializes one ssa.Function using the value-numbering
// scheme documented in gvir.proto: params, free vars, value-producing
// instructions (two passes so phi nodes can reference later blocks),
// then aux values at first operand encounter.
func (e *emitter) emitFunction(fn *ssa.Function) *gvirpb.Function {
	f := &gvirpb.Function{
		Id:   fn.String(),
		Name: fn.Name(),
		Type: e.typeID(fn.Signature),
		Pos:  e.position(fn.Pos()),
	}
	ids := map[ssa.Value]uint32{}
	next := uint32(1)
	assign := func(v ssa.Value) uint32 {
		id := next
		ids[v] = id
		next++
		return id
	}
	for _, p := range fn.Params {
		f.Params = append(f.Params, &gvirpb.Param{
			Id:   assign(p),
			Name: p.Name(),
			Type: e.typeID(p.Type()),
		})
	}
	for _, fv := range fn.FreeVars {
		f.Aux = append(f.Aux, &gvirpb.AuxValue{
			Id:   assign(fv),
			Kind: "FreeVar",
			Repr: fv.Name(),
			Type: e.typeID(fv.Type()),
		})
	}
	// Pass 1: number every value-producing instruction so operands can
	// reference values defined later (phi edges).
	for _, b := range fn.Blocks {
		for _, ins := range b.Instrs {
			if v, ok := ins.(ssa.Value); ok {
				assign(v)
			}
		}
	}
	// Pass 2: emit; operands not yet numbered (consts, globals,
	// functions, builtins) become AuxValues at first encounter.
	operandID := func(v ssa.Value) uint32 {
		if id, ok := ids[v]; ok {
			return id
		}
		id := assign(v)
		f.Aux = append(f.Aux, &gvirpb.AuxValue{
			Id:   id,
			Kind: auxKind(v),
			Repr: v.String(),
			Type: e.typeID(v.Type()),
		})
		return id
	}
	var rands []*ssa.Value
	for _, b := range fn.Blocks {
		bb := &gvirpb.BasicBlock{Index: uint32(b.Index)}
		for _, s := range b.Succs {
			bb.Succs = append(bb.Succs, uint32(s.Index))
		}
		for _, ins := range b.Instrs {
			pi := &gvirpb.Instruction{
				Kind:   strings.TrimPrefix(fmt.Sprintf("%T", ins), "*ssa."),
				Pos:    e.position(ins.Pos()),
				Detail: ins.String(),
			}
			if v, ok := ins.(ssa.Value); ok {
				pi.Register = ids[v]
				pi.Type = e.typeID(v.Type())
			}
			rands = ins.Operands(rands[:0])
			for _, vp := range rands {
				if vp == nil || *vp == nil {
					pi.Operands = append(pi.Operands, 0)
					continue
				}
				pi.Operands = append(pi.Operands, operandID(*vp))
			}
			bb.Instrs = append(bb.Instrs, pi)
		}
		f.Blocks = append(f.Blocks, bb)
	}
	return f
}

func auxKind(v ssa.Value) string {
	switch v.(type) {
	case *ssa.Const:
		return "Const"
	case *ssa.Global:
		return "Global"
	case *ssa.Function:
		return "Function"
	case *ssa.Builtin:
		return "Builtin"
	case *ssa.FreeVar:
		return "FreeVar"
	default:
		return "Value"
	}
}
