package main

import (
	"cmp"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"go/ast"
	"go/constant"
	"go/token"
	"go/types"
	"math"
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
	schemaVersion    = "4"
	extractorVersion = "0.1.0"
)

// emitter builds one canonical gvirpb.Package. All interning maps are
// filled in deterministic walk order, so the assigned ids are stable.
type emitter struct {
	fset    *token.FileSet
	pkg     *packages.Package
	prog    *ssa.Program // for resolving a *types.Func to its SSA function (pragma decl-id alignment)
	goroot  string       // resolved via `go env GOROOT` by the caller; "" if unresolved
	out     *gvirpb.Package
	typeIDs map[string]uint32
	fileIDs map[string]uint32
}

func extractPackage(fset *token.FileSet, p *packages.Package, sp *ssa.Package, fns []*ssa.Function, goroot string) *gvirpb.Package {
	e := &emitter{
		fset:    fset,
		pkg:     p,
		prog:    sp.Prog,
		goroot:  goroot,
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
	e.emitMethodSets(sp)
	e.emitPragmas()
	e.canonicalize()
	return e.out
}

// relPath rewrites a source filename to a stable machine-independent
// form: module-root-relative when inside a module (covers the target
// module and module-cache deps alike), $GOROOT-relative for stdlib,
// otherwise the base name. Never absolute (spec §3).
//
// GOROOT comes from e.goroot (resolved once per run via `go env GOROOT`
// in extract.go's run()), never from runtime.GOROOT(): in a -trimpath
// build, runtime.GOROOT() returns "" whenever the GOROOT env var is
// unset, which would silently degrade every stdlib file to its
// basename and break byte-identical determinism across machines.
func (e *emitter) relPath(filename string) string {
	if m := e.pkg.Module; m != nil && m.Dir != "" {
		if r, err := filepath.Rel(m.Dir, filename); err == nil && !strings.HasPrefix(r, "..") {
			return filepath.ToSlash(r)
		}
	}
	if e.goroot != "" {
		if r, err := filepath.Rel(e.goroot, filename); err == nil && !strings.HasPrefix(r, "..") {
			return "$GOROOT/" + filepath.ToSlash(r)
		}
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
	repr := canonType(t)
	if id, ok := e.typeIDs[repr]; ok {
		return id
	}
	id := uint32(len(e.typeIDs) + 1)
	e.typeIDs[repr] = id
	pb := &gvirpb.Type{Id: id, Repr: repr}
	e.out.Types = append(e.out.Types, pb) // append BEFORE fill: recursion sees the id
	e.fillType(pb, t)
	return id
}

func (e *emitter) fillType(pb *gvirpb.Type, t types.Type) {
	switch t := t.(type) {
	case *types.Basic:
		pb.Kind, pb.Name = gvirpb.TypeKind_TYPE_KIND_BASIC, t.Name()
	case *types.Named:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_NAMED
		if t.Obj().Pkg() != nil {
			pb.Name = t.Obj().Pkg().Path() + "." + t.Obj().Name()
		} else {
			pb.Name = t.Obj().Name() // universe scope: "error"
		}
		pb.Elem = e.typeID(t.Underlying())
	case *types.Alias:
		e.fillType(pb, types.Unalias(t)) // aliases are transparent
	case *types.Pointer:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_POINTER, e.typeID(t.Elem())
	case *types.Slice:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_SLICE, e.typeID(t.Elem())
	case *types.Array:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_ARRAY, e.typeID(t.Elem())
		pb.ArrayLen = uint64(t.Len())
	case *types.Map:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_MAP
		pb.Key, pb.Elem = e.typeID(t.Key()), e.typeID(t.Elem())
	case *types.Chan:
		pb.Kind, pb.Elem = gvirpb.TypeKind_TYPE_KIND_CHAN, e.typeID(t.Elem())
		pb.ChanDir = uint32(t.Dir())
	case *types.Struct:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_STRUCT
		for i := range t.NumFields() {
			f := t.Field(i)
			pb.Fields = append(pb.Fields, &gvirpb.Field{
				Name: f.Name(), Type: e.typeID(f.Type()), Embedded: f.Embedded(),
			})
		}
	case *types.Interface:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_INTERFACE
	case *types.Signature:
		pb.Kind, pb.Variadic = gvirpb.TypeKind_TYPE_KIND_SIGNATURE, t.Variadic()
		for i := range t.Params().Len() {
			pb.Params = append(pb.Params, e.typeID(t.Params().At(i).Type()))
		}
		for i := range t.Results().Len() {
			pb.Results = append(pb.Results, e.typeID(t.Results().At(i).Type()))
		}
	case *types.Tuple:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_TUPLE
		for i := range t.Len() {
			pb.Params = append(pb.Params, e.typeID(t.At(i).Type()))
		}
	case *types.TypeParam:
		pb.Kind = gvirpb.TypeKind_TYPE_KIND_TYPE_PARAM
	}
	// anything else stays TYPE_KIND_UNSPECIFIED — the Rust side treats
	// unspecified as opaque/unknown (degrade, never die)
}

// canonicalize rebuilds t so that two go/types representations that are
// type-identical but rendered differently under concurrent package
// loading collapse to one deterministic string. It resolves two
// load-order-raced sources of nondeterminism (both proven to flip bbolt
// extraction run-to-run):
//
//  1. Type aliases. types.Unalias is shallow (it only follows a
//     top-level alias chain), so a slice/signature/instance whose
//     COMPONENT is an alias — []os.DirEntry, func(os.DirEntry) int,
//     Foo[os.DirEntry] — still renders the alias spelling unless every
//     component is unaliased. A shared generic instance
//     (slices.SortFunc[fs.DirEntry]) records whichever alias spelling
//     (os.DirEntry vs io/fs.DirEntry) won the type-check race. We resolve
//     every alias to its underlying named type.
//
//  2. Signature parameter/result/receiver NAMES. These are not part of a
//     type's identity (types.Identical ignores them), but they DO appear
//     in types.TypeString. go/types canonicalizes structurally-identical
//     signatures into one shared object whose parameter names come from
//     whichever call site was registered first — another race. We blank
//     every signature's parameter names so the rendered type is
//     name-invariant. (Struct field names ARE part of type identity and
//     are declared once, so they are preserved.)
//
// Determinism > pretty names (spec): the result always uses the
// underlying type's fully-qualified name and unnamed signature params.
// Non-signature, alias-free types return the ORIGINAL object unchanged,
// so their types.TypeString rendering is byte-identical to before.
func canonicalize(t types.Type) types.Type {
	switch t := t.(type) {
	case *types.Alias:
		return canonicalize(types.Unalias(t))
	case *types.Pointer:
		if e := canonicalize(t.Elem()); e != t.Elem() {
			return types.NewPointer(e)
		}
	case *types.Slice:
		if e := canonicalize(t.Elem()); e != t.Elem() {
			return types.NewSlice(e)
		}
	case *types.Array:
		if e := canonicalize(t.Elem()); e != t.Elem() {
			return types.NewArray(e, t.Len())
		}
	case *types.Chan:
		if e := canonicalize(t.Elem()); e != t.Elem() {
			return types.NewChan(t.Dir(), e)
		}
	case *types.Map:
		k, v := canonicalize(t.Key()), canonicalize(t.Elem())
		if k != t.Key() || v != t.Elem() {
			return types.NewMap(k, v)
		}
	case *types.Tuple:
		// A bare tuple type: blank element names like a signature.
		return blankTuple(t)
	case *types.Struct:
		n := t.NumFields()
		fields := make([]*types.Var, n)
		tags := make([]string, n)
		changed := false
		for i := range fields {
			f := t.Field(i)
			ft := canonicalize(f.Type())
			if ft != f.Type() {
				changed = true
			}
			// Field names are part of struct identity: preserve them.
			fields[i] = types.NewField(f.Pos(), f.Pkg(), f.Name(), ft, f.Embedded())
			tags[i] = t.Tag(i)
		}
		if changed {
			return types.NewStruct(fields, tags)
		}
	case *types.Interface:
		// Anonymous/structural interfaces render their explicit method
		// SIGNATURES via types.TypeString — with parameter names and
		// alias spellings — but fillType's Interface case emits only Kind,
		// so those spellings survive solely in Type.Repr and race exactly
		// like standalone signatures. Rebuild with each method signature
		// canonicalized (param names blanked, aliases resolved) and each
		// embedded type canonicalized. Named interfaces (io.Reader) never
		// reach here as a *types.Interface — they are *types.Named and
		// render as a bare qualified name, so the Named barrier holds.
		nm, ne := t.NumExplicitMethods(), t.NumEmbeddeds()
		if nm == 0 && ne == 0 {
			return t // interface{} / any: nothing to canonicalize
		}
		methods := make([]*types.Func, nm)
		for i := range methods {
			m := t.ExplicitMethod(i)
			sig := m.Type().(*types.Signature)
			// Interface methods carry no explicit receiver here;
			// NewInterfaceType sets it. Blank names, canonicalize types.
			csig := types.NewSignatureType(nil, nil, nil, blankTuple(sig.Params()), blankTuple(sig.Results()), sig.Variadic())
			methods[i] = types.NewFunc(token.NoPos, m.Pkg(), m.Name(), csig)
		}
		embeddeds := make([]types.Type, ne)
		for i := range embeddeds {
			embeddeds[i] = canonicalize(t.EmbeddedType(i))
		}
		return types.NewInterfaceType(methods, embeddeds).Complete()
	case *types.Signature:
		// Uninstantiated generic signatures carry free type params that a
		// rebuilt tuple would still reference; leave them be (they render
		// type-param names, not aliases, and are not shared-instance
		// canonicalized).
		if t.TypeParams().Len() > 0 || t.RecvTypeParams().Len() > 0 {
			return t
		}
		var recv *types.Var
		if r := t.Recv(); r != nil {
			recv = types.NewVar(token.NoPos, nil, "", canonicalize(r.Type()))
		}
		// Always rebuild with blanked param/result names — the fix for
		// the parameter-name race — deep-canonicalizing each type.
		return types.NewSignatureType(recv, nil, nil, blankTuple(t.Params()), blankTuple(t.Results()), t.Variadic())
	case *types.Named:
		targs := t.TypeArgs()
		if targs == nil || targs.Len() == 0 {
			return t
		}
		args := make([]types.Type, targs.Len())
		changed := false
		for i := range args {
			args[i] = canonicalize(targs.At(i))
			if args[i] != targs.At(i) {
				changed = true
			}
		}
		if changed {
			if inst, err := types.Instantiate(nil, t.Origin(), args, false); err == nil {
				return inst
			}
		}
	}
	return t
}

// blankTuple rebuilds tup with every element's type canonicalized and its
// NAME blanked, so the rendered signature is parameter-name-invariant.
func blankTuple(tup *types.Tuple) *types.Tuple {
	if tup == nil {
		return nil
	}
	vars := make([]*types.Var, tup.Len())
	for i := range vars {
		vars[i] = types.NewVar(token.NoPos, nil, "", canonicalize(tup.At(i).Type()))
	}
	return types.NewTuple(vars...)
}

func pathQualifier(p *types.Package) string { return p.Path() }

// canonType renders t's fully-qualified, alias- and param-name-canonical
// string. For alias-free non-signature types it is byte-identical to the
// previous types.TypeString(t, path-qualifier) rendering.
func canonType(t types.Type) string {
	return types.TypeString(canonicalize(t), pathQualifier)
}

// canonizeTypeArgs canonicalizes the type-argument spellings baked into a
// generic instance's rendered ssa string s (its String()/Name(), or a
// call detail that embeds it). ssa bakes the raced alias spelling into
// the instance name at instantiation, so we replace each type argument's
// raw spelling with its alias-canonical one. This substitutes the
// per-argument type substrings rather than the bracketed list as a whole,
// so it is robust to ssa's "[a, b]" comma formatting (an earlier
// suffix-reconstruction attempt failed because it assumed fmt's
// space-separated slice formatting). Longest raws first so a shorter arg
// spelling can't partially rewrite a longer one. A no-op when no type
// argument contains an alias.
func canonizeTypeArgs(s string, targs []types.Type) string {
	type sub struct{ raw, can string }
	var subs []sub
	for _, a := range targs {
		raw := types.TypeString(a, pathQualifier)
		can := canonType(a)
		if raw != can {
			subs = append(subs, sub{raw, can})
		}
	}
	if len(subs) == 0 {
		return s
	}
	slices.SortFunc(subs, func(x, y sub) int { return cmp.Compare(len(y.raw), len(x.raw)) })
	for _, p := range subs {
		s = strings.ReplaceAll(s, p.raw, p.can)
	}
	return s
}

// canonFuncID renders an ssa.Function's fully-qualified id with any alias
// spelling in its type arguments canonicalized.
func canonFuncID(fn *ssa.Function) string {
	return canonizeTypeArgs(fn.String(), fn.TypeArgs())
}

// canonDetail canonicalizes the human-readable instruction detail
// (ins.String()), which bakes the raced alias spelling of any generic
// instance it references — the static callee of a call, or a
// function-valued operand.
func canonDetail(ins ssa.Instruction) string {
	detail := ins.String()
	if ci, ok := ins.(ssa.CallInstruction); ok {
		if callee := ci.Common().StaticCallee(); callee != nil {
			detail = canonizeTypeArgs(detail, callee.TypeArgs())
		}
	}
	for _, op := range ins.Operands(nil) {
		if op == nil || *op == nil {
			continue
		}
		if fn, ok := (*op).(*ssa.Function); ok && len(fn.TypeArgs()) > 0 {
			detail = canonizeTypeArgs(detail, fn.TypeArgs())
		}
	}
	return detail
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
		Id:   canonFuncID(fn),
		Name: canonizeTypeArgs(fn.Name(), fn.TypeArgs()),
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
	// Result names come from the ORIGINAL signature, never the
	// canonicalized one (canonicalization blanks names for determinism).
	sig := fn.Signature
	for i := range sig.Results().Len() {
		f.ResultNames = append(f.ResultNames, sig.Results().At(i).Name())
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
		// A Function operand's String() bakes the raced generic-instance
		// type-arg spelling; canonicalize it like the Function.Id above.
		repr := v.String()
		if fn, ok := v.(*ssa.Function); ok {
			repr = canonFuncID(fn)
		}
		aux := &gvirpb.AuxValue{
			Id:   id,
			Kind: auxKind(v),
			Repr: repr,
			Type: e.typeID(v.Type()),
		}
		if c, ok := v.(*ssa.Const); ok {
			aux.Const = constValue(c)
		}
		f.Aux = append(f.Aux, aux)
		return id
	}
	var rands []*ssa.Value
	for _, b := range fn.Blocks {
		bb := &gvirpb.BasicBlock{Index: uint32(b.Index)}
		for _, s := range b.Succs {
			bb.Succs = append(bb.Succs, uint32(s.Index))
		}
		for _, pred := range b.Preds {
			bb.Preds = append(bb.Preds, uint32(pred.Index))
		}
		for _, ins := range b.Instrs {
			pi := &gvirpb.Instruction{
				Kind:   strings.TrimPrefix(fmt.Sprintf("%T", ins), "*ssa."),
				Pos:    e.position(ins.Pos()),
				Detail: canonDetail(ins),
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
			switch ins := ins.(type) {
			case *ssa.BinOp:
				pi.Sem = &gvirpb.Instruction_Binop{Binop: &gvirpb.BinOpSem{Op: ins.Op.String()}}
			case *ssa.UnOp:
				pi.Sem = &gvirpb.Instruction_Unop{Unop: &gvirpb.UnOpSem{Op: ins.Op.String(), CommaOk: ins.CommaOk}}
			case *ssa.Field:
				if st, ok := ins.X.Type().Underlying().(*types.Struct); ok {
					pi.Sem = &gvirpb.Instruction_Field{Field: &gvirpb.FieldSem{
						Index: uint32(ins.Field), Name: st.Field(ins.Field).Name()}}
				}
			case *ssa.FieldAddr:
				if pt, ok := ins.X.Type().Underlying().(*types.Pointer); ok {
					if st, ok := pt.Elem().Underlying().(*types.Struct); ok {
						pi.Sem = &gvirpb.Instruction_Field{Field: &gvirpb.FieldSem{
							Index: uint32(ins.Field), Name: st.Field(ins.Field).Name()}}
					}
				}
			case *ssa.TypeAssert:
				pi.Sem = &gvirpb.Instruction_TypeAssert{TypeAssert: &gvirpb.TypeAssertSem{
					Asserted: e.typeID(ins.AssertedType), CommaOk: ins.CommaOk}}
			case *ssa.Extract:
				pi.Sem = &gvirpb.Instruction_Extract{Extract: &gvirpb.ExtractSem{Index: uint32(ins.Index)}}
			case *ssa.Lookup:
				pi.Sem = &gvirpb.Instruction_Lookup{Lookup: &gvirpb.LookupSem{CommaOk: ins.CommaOk}}
			case *ssa.Alloc:
				pi.Sem = &gvirpb.Instruction_Alloc{Alloc: &gvirpb.AllocSem{Heap: ins.Heap}}
			case *ssa.Select:
				sem := &gvirpb.SelectSem{Blocking: ins.Blocking}
				for _, st := range ins.States {
					s := &gvirpb.SelectState{Dir: uint32(st.Dir), ChanOperand: operandID(st.Chan)}
					if st.Send != nil {
						s.SendOperand = operandID(st.Send)
					}
					sem.States = append(sem.States, s)
				}
				pi.Sem = &gvirpb.Instruction_Select{Select: sem}
			case ssa.CallInstruction: // *ssa.Call, *ssa.Defer, *ssa.Go
				cc := ins.Common()
				sem := &gvirpb.CallSem{}
				if cc.IsInvoke() {
					sem.Invoke = true
					sem.Method = cc.Method.Name()
					sem.IfaceType = e.typeID(cc.Value.Type())
					sem.MethodSig = e.typeID(cc.Method.Type())
				} else {
					if f := cc.StaticCallee(); f != nil {
						sem.StaticCallee = canonFuncID(f)
					} else if b, ok := cc.Value.(*ssa.Builtin); ok {
						sem.Builtin = b.Name()
					}
				}
				pi.Sem = &gvirpb.Instruction_Call{Call: sem}
			}
			bb.Instrs = append(bb.Instrs, pi)
		}
		f.Blocks = append(f.Blocks, bb)
	}
	return f
}

func constValue(c *ssa.Const) *gvirpb.ConstValue {
	if c.Value == nil { // nil pointer/interface/map/…, or zero value
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Nil{Nil: true}}
	}
	switch c.Value.Kind() {
	case constant.Bool:
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Bool{Bool: constant.BoolVal(c.Value)}}
	case constant.Int:
		if i, exact := constant.Int64Val(c.Value); exact {
			return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Int{Int: i}}
		}
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_BigInt{BigInt: c.Value.ExactString()}}
	case constant.Float:
		f, _ := constant.Float64Val(c.Value)
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_FloatBits{FloatBits: math.Float64bits(f)}}
	case constant.String:
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Str{Str: []byte(constant.StringVal(c.Value))}}
	case constant.Complex:
		return &gvirpb.ConstValue{Value: &gvirpb.ConstValue_Complex{Complex: c.Value.ExactString()}}
	}
	return nil // Unknown kind: leave unset; Rust treats as opaque
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

// emitMethodSets records, for each named type declared in the package,
// the full method set of *T (or of T itself for interfaces), as
// Method entries (plain name + signature type id) in the method-set
// order (which types.NewMethodSet defines deterministically, by name).
func (e *emitter) emitMethodSets(sp *ssa.Package) {
	names := make([]string, 0, len(sp.Members))
	for name, m := range sp.Members {
		if _, ok := m.(*ssa.Type); ok {
			names = append(names, name)
		}
	}
	slices.Sort(names)
	for _, name := range names {
		T := sp.Members[name].(*ssa.Type).Type()
		var ms *types.MethodSet
		if types.IsInterface(T) {
			ms = types.NewMethodSet(T)
		} else {
			ms = types.NewMethodSet(types.NewPointer(T))
		}
		if ms.Len() == 0 {
			continue
		}
		pb := &gvirpb.MethodSet{Type: e.typeID(T)}
		for i := range ms.Len() {
			sel := ms.At(i)
			obj := sel.Obj().(*types.Func)
			m := &gvirpb.Method{Name: obj.Name(), Sig: e.typeID(sel.Type())}
			if !types.IsInterface(T) {
				if fn := sp.Prog.MethodValue(sel); fn != nil {
					m.FuncId = canonFuncID(fn)
				}
			}
			pb.Methods = append(pb.Methods, m)
		}
		e.out.MethodSets = append(e.out.MethodSets, pb)
	}
}

// emitPragmas captures //goverify: doc-comment lines verbatim; parsing
// and validation belong to goverify-spec (phase 6, spec §6).
func (e *emitter) emitPragmas() {
	for _, file := range e.pkg.Syntax {
		ast.Inspect(file, func(n ast.Node) bool {
			var doc *ast.CommentGroup
			var declID string
			switch d := n.(type) {
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
			case *ast.GenDecl:
				doc = d.Doc
				if len(d.Specs) == 1 {
					switch s := d.Specs[0].(type) {
					case *ast.TypeSpec:
						declID = e.pkg.PkgPath + "." + s.Name.Name
					case *ast.ValueSpec:
						if len(s.Names) > 0 {
							declID = e.pkg.PkgPath + "." + s.Names[0].Name
						}
					}
				}
			default:
				return true
			}
			if doc == nil || declID == "" {
				return true
			}
			for _, c := range doc.List {
				if strings.HasPrefix(c.Text, "//goverify:") {
					e.out.Pragmas = append(e.out.Pragmas, &gvirpb.Pragma{
						DeclId: declID,
						Text:   c.Text,
						Pos:    e.position(c.Pos()),
					})
				}
			}
			return true
		})
	}
}

// canonicalize enforces the sort orders documented in gvir.proto.
// Files, types, and functions are already deterministic by
// construction; method sets and pragmas are sorted here.
func (e *emitter) canonicalize() {
	slices.SortFunc(e.out.MethodSets, func(a, b *gvirpb.MethodSet) int {
		return cmp.Compare(a.GetType(), b.GetType())
	})
	slices.SortFunc(e.out.Pragmas, func(a, b *gvirpb.Pragma) int {
		if c := strings.Compare(a.GetDeclId(), b.GetDeclId()); c != 0 {
			return c
		}
		return strings.Compare(a.GetText(), b.GetText())
	})
}
