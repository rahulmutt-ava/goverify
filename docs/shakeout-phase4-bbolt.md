# Phase-4 shakeout: etcd-io/bbolt @ v1.4.0

Status: COMPLETE (2026-07-19) — all 1006 findings triaged into 459
classes (408 parent classes, 44 split into 2-3 subclasses each after a
mixed-verdict refinement round); FP rate recorded; dispatch-precision
observations recorded for phase-5 planning. Exit criteria (spec §7): see
disposition section below.

## Run parameters
- goverify commit: c0655e4
- bbolt ref: v1.4.0
- timeouts: infer 100 ms / obligation 250 ms (defaults)
- findings: 1006 (ledger's last recorded run: 1006; no delta)
- wall clock: cold 372.47 s / warm 29.74 s

## Class triage

1006 findings were grouped by a normalized `tag|source-shape` class key
into 408 parent classes. Every parent class was triaged from its
sampled representatives (`work/reps/C*.tsv`) and recorded in
`verdicts/C*.md`; 44 classes came back MIXED and were split once more
(subclasses `a`/`b`/`c`) — after that single refinement round only
`C015b` remained heterogeneous (see Totals). One row per class below,
459 total, in descending-count order (subclasses grouped under their
parent's position); `pattern` is the class key's source part with `|`
escaped; `representatives` are the parent class's sampled positions,
or (for subclasses) up to 3 of the subclass's own member positions.


| class | tag | pattern | count | verdict | reason (one line) | representatives | note |
|---|---|---|---|---|---|---|---|
| C001 | nil-deref | I.I(I.I()) | 43 | FP/encoding | receiver in each finding is address-of-stack-local/composite-literal or unsafe pointer arithmetic on a non-empty buffer (&buf[k], &value[0]), which Go guarantees is never nil | cmd/bbolt/command_check.go:32:12, cmd/bbolt/command_surgery_meta.go:133:32, cmd/bbolt/command_surgery.go:65:12, internal/common/inode.go:58:29, internal/surgeon/surgeon.go:133:18 | second opinion: blind disjoint-sample second pass (verdicts/C001-second.md) AGREED — FP/encoding, unanimous across both independent samples |
| C002a | nil-deref | I.I(I) | 9 | FP/invariant | InBucket/Interface/rootNode fields are set immediately after construction (newBucket/openBucket/NewArrayFreelist etc.) before the value is ever exposed to a caller, so later dereferences can't observe nil | bucket.go:557:17, bucket.go:843:9, bucket.go:843:9 |  |
| C002b | nil-deref | I.I(I) | 11 | FP/encoding | flagged pointer is the address of an in-bounds slice element or composite literal (&buf[k], &n.inodes[i], &s local); Go guarantees this is never nil, an out-of-bounds index panics instead | bucket.go:691:7, db.go:845:8, db.go:795:8 |  |
| C003 | nil-deref | I.I(&I.I, S, S, S) | 16 | FP/encoding | fs assigned from flag.NewFlagSet and never reassigned before use; stdlib NewFlagSet always returns a freshly-allocated non-nil *FlagSet | cmd/bbolt/main.go:1083:14, cmd/bbolt/main.go:1084:14, cmd/bbolt/main.go:1090:14, cmd/bbolt/main.go:1092:14, cmd/bbolt/main.go:1689:14 |  |
| C004a | nil-deref | I := I.I() | 12 | FP/invariant | b.tx set once by newBucket and never nil'd; db returned non-nil exactly when err==nil in Open; hashMap only ever built via NewHashMapFreelist - all construction-time invariants | bucket.go:96:15, bucket.go:294:15, bucket.go:602:15 |  |
| C004b | nil-deref | I := I.I() | 3 | FP/requires-lifting | p is non-nil only because of the caller's (or same-function) preceding successful dereference (e.isLeaf()/p.Count()) that the analyzer doesn't carry across the call boundary or statement | cursor.go:362:30, cursor.go:326:32, internal/common/inode.go:51:24 |  |
| C005 | nil-deref | I.I(&I.I, S, I, S) | 14 | FP/invariant | fs assigned from flag.NewFlagSet on the immediately preceding line with no reassignment; NewFlagSet never returns nil | cmd/bbolt/main.go:1094:12, cmd/bbolt/main.go:1095:12, cmd/bbolt/main.go:1691:12, cmd/bbolt/main.go:384:12, cmd/bbolt/main.go:387:12 |  |
| C006 | nil-deref | I I := I.I(I); I != I { | 14 | FP/invariant | fs from never-nil NewFlagSet, and fileSize forced >= pageSize*2 by db.fileSize()'s own contract with pageSize always positive before mmap is called | cmd/bbolt/main.go:1099:20, cmd/bbolt/main.go:388:20, cmd/bbolt/main.go:783:20, cmd/bbolt/page_command.go:35:20, db.go:509:21 |  |
| C007a | nil-deref | I.I.I(I) | 5 | FP/encoding | analyzer's own printed path trail crosses a same-function dominating nil-check (tx.db==nil / n.parent==nil guard) but fails to carry that proven-non-nil fact forward to a later dereference of the identical unmutated field | tx.go:368:6, tx.go:368:17, tx.go:368:17 |  |
| C007b | nil-deref | I.I.I(I) | 7 | FP/invariant | non-nilness rests on a cross-function/whole-structure invariant: value-embedded struct addressed from a stack-local var (AddFlags), B+tree root-only-nil-parent discipline (node.go:433), or tx.meta init/close lifecycle pairing (tx.go:561) | tx.go:561:15, tx.go:561:15, node.go:433:28 |  |
| C008a | nil-deref | I I.I() { | 4 | FP/encoding | dereferenced pointer is an unsafe-pointer/arithmetic value derived from a byte buffer or mmap slice (tx.page/db.page/&buf[0]), which is never Go-nil regardless of index validity | tx.go:613:19, tx_check.go:105:25, cmd/bbolt/page_command.go:167:21 |  |
| C008b | nil-deref | I I.I() { | 5 | FP/invariant | pointer flows through Bucket.pageNode's page-XOR-node mutual-exclusivity invariant or openBucket's never-nil map-write invariant, both established at construction | bucket.go:622:18, bucket.go:749:22, cursor.go:173:16 |  |
| C008c | nil-deref | I I.I() { | 2 | FP/requires-lifting | readMetaPage's err==nil implies meta!=nil postcondition (built on ReadMetaPageAt/LoadPageMeta) is never carried across the call boundary to the meta.IsFreelistPersisted() call site | cmd/bbolt/command_surgery.go:145:29, cmd/bbolt/command_surgery_freelist.go:91:29 |  |
| C009a | nil-deref | I I := I.I(); I != I { | 2 | FP/invariant | iterated/returned value is sourced exclusively from a never-nil-returning constructor helper (openBucket for b.buckets map values, NewRootCommand's composite literal) - a shared data-structure invariant | bucket.go:753:25, cmd/bbolt/main.go:78:27 |  |
| C009b | nil-deref | I I := I.I(); I != I { | 7 | FP/encoding | receiver is the implicit address of a stack-local struct var o T declared with 'var o T' in the same function; (&o).Validate() can never be nil - an analyzer address-of modeling defect | cmd/bbolt/command_surgery.go:182:24, cmd/bbolt/command_surgery.go:249:24, cmd/bbolt/command_surgery.go:118:24 |  |
| C009c | nil-deref | I I := I.I(); I != I { | 2 | FP/requires-lifting | DB.Begin/beginRWTx/beginTx's and ReadMetaPageAt's err==nil implies result!=nil postcondition is never propagated across the caller's own err-check guard to the Commit()/Validate() call site | compact.go:26:23, cmd/bbolt/command_surgery_meta.go:59:32 |  |
| C010 | nil-deref | I.I(I()) | 10 | FP/invariant | cmd/surgeryCmd is the address of a cobra.Command composite literal constructed a few lines above with no intervening reassignment before AddCommand | cmd/bbolt/command_surgery_freelist.go:20:16, cmd/bbolt/command_surgery_meta.go:29:16, cmd/bbolt/command_surgery.go:26:23, cmd/bbolt/command_surgery.go:28:23, cmd/bbolt/command_surgery.go:31:23 |  |
| C011 | nil-deref | I.I(&I.I, S, N, S) | 10 | FP/encoding | fs from flag.NewFlagSet never reassigned before Int64Var/IntVar calls; NewFlagSet unconditionally returns a non-nil &FlagSet{} | cmd/bbolt/main.go:1086:13, cmd/bbolt/main.go:1087:13, cmd/bbolt/main.go:1088:11, cmd/bbolt/main.go:1089:11, cmd/bbolt/main.go:1690:13 |  |
| C012 | nil-deref | I.I = I | 10 | FP/encoding | the same receiver is already dereferenced earlier in the identical function (Assert/opened-check/db.db==nil guard) before the later field-store the analyzer flags as a fresh nil-receiver opportunity | bucket.go:870:5, db.go:690:5, internal/btesting/btesting.go:92:6, node.go:172:5, tx.go:375:5 |  |
| C013 | nil-deref | I.I.I() | 9 | FP/invariant | timer/tx.meta are set synchronously at construction before run()/Commit is reachable; *DB/*Tx receivers are only nil via caller misuse of Open/Begin's paired non-nil-error contract; n.parent is refuted by a dominating same-function guard | db.go:1012:14, db.go:674:5, node.go:412:21, tx_check.go:40:5, tx.go:63:18 |  |
| C014a | nil-deref | I I.I.I() | 3 | FP/encoding | the flagged callee's own adjacent guard (n.parent!=nil) or the stdlib callee's own internal nil-check (os.File.Sync's checkValid) already refutes nil on the analyzer's own printed path, just not carried forward | node.go:29:22, node.go:357:24, boltsync_unix.go:7:21 |  |
| C014b | nil-deref | I I.I.I() | 6 | FP/invariant | entire *DB/*Tx receiver of an exported, internally-uncalled API (Close/Cursor/Inspect) is only nil via external caller misuse of Open/Begin's paired non-nil-error postcondition; cursor.go:423 rests on elemRef's page-XOR-node construction invariant | db.go:672:11, db.go:675:11, db.go:678:11 |  |
| C015a | nil-deref | I I.I != I { | 4 | FP/encoding | flagged check is not the receiver's first dereference in the enclosing function - an earlier line already read a field/promoted method off the identical never-reassigned receiver, proving non-nil in-function | bucket.go:106:7, bucket.go:934:8, bucket.go:941:7 |  |
| C015b | nil-deref | I I.I != I { | 5 | mixed | first-dereference-in-function cases split: some (tx.go:586, bucket.go:698, btesting.go:82) are plain construction invariants while others (bucket.go:711, bucket.go:89) require substituting a caller's already-proven seek/traversal-order precondition across the call boundary - never collapsed to one label | bucket.go:89:7, bucket.go:698:7, bucket.go:711:7 |  |
| C016 | nil-deref | I := I.I(S, I, S) | 9 | FP/invariant | fs used in a straight-line block immediately after flag.NewFlagSet with no intervening reassignment; NewFlagSet unconditionally allocates and returns non-nil | cmd/bbolt/main.go:198:17, cmd/bbolt/main.go:558:17, cmd/bbolt/main.go:782:17, cmd/bbolt/main.go:922:17, cmd/bbolt/page_command.go:32:16 |  |
| C017a | nil-deref | I.I(N) | 1 | FP/encoding | analyzer's own reported path already proves b.InBucket non-nil via an earlier promoted-method call (RootPage()) on the identical unreassigned field before the later SetRootPage() dereference | bucket.go:911:15 |  |
| C017b | nil-deref | I.I(N) | 5 | FP/invariant | InBucket is set non-nil at every Bucket construction site (openBucket/CreateBucket literals) before free() is reachable; db.pageSize/tx.db.pageSize is forced positive at Open() before init()/Tx exist, so buffer-derived pointers are never nil | bucket.go:911:2, db.go:635:16, db.go:637:12 |  |
| C017c | nil-deref | I.I(N) | 2 | FP/requires-lifting | results traces to &writeResults (address-of-local, never nil) but that fact must be substituted through 2-3 layers of unchanged parameter forwarding across wrapper functions before AddCompletedOps | cmd/bbolt/main.go:1260:28, cmd/bbolt/main.go:1208:28 |  |
| C018 | nil-deref | I.I(I.I, S, I.I(), I.I(), I.I(), I.I()) | 8 | FP/encoding | receiver is the implicit address of a local BenchResults value (var writeResults/readResults BenchResults); never nil despite the pointer-receiver method call | cmd/bbolt/main.go:1062:100, cmd/bbolt/main.go:1062:125, cmd/bbolt/main.go:1062:181, cmd/bbolt/main.go:1063:148, cmd/bbolt/main.go:1063:98 |  |
| C019 | nil-deref | I.I(&I.I, S, S, I.I, S) | 8 | FP/invariant | fs traces to cobra's (*Command).Flags(), which lazily initializes c.flags and never returns nil - an external library invariant invisible to the analyzer | cmd/bbolt/command_check.go:18:15, cmd/bbolt/command_surgery_meta.go:90:15, cmd/bbolt/command_surgery.go:224:15, cmd/bbolt/command_surgery.go:226:12, cmd/bbolt/command_surgery.go:96:15 |  |
| C020 | nil-deref | I.I() | 8 | FP/invariant | promoted-method receivers/collection values (InBucket, b.nodes, b.buckets, n.children) are sourced exclusively from never-nil constructor helpers (openBucket/newBucket/&node{}) - a data-structure invariant | bucket.go:576:15, bucket.go:851:14, bucket.go:921:20, internal/freelist/array.go:18:2, node.go:486:20 |  |
| C021a | nil-deref | I.I.I.I(I.I.I()) | 4 | FP/encoding | tx.db is already checked non-nil by name via a same-function guard (tx.db==nil return) a few lines above with no intervening write; analyzer fails to carry that refinement to later reads of the identical field selector | tx.go:317:6, tx.go:328:6, tx.go:335:8 |  |
| C021b | nil-deref | I.I.I.I(I.I.I()) | 4 | FP/invariant | tx.meta is never locally guarded, but its non-nilness follows from the cross-function paired-lifecycle invariant that tx.db and tx.meta are set together in init and cleared together in close | tx.go:317:30, tx.go:317:39, tx.go:328:30 |  |
| C022a | nil-deref | I I.I() == N { | 1 | FP/encoding | the RootPage()==0 check sits inside openBucket itself, right after its own assignment of child.InBucket via address-of-slice-element - an intraprocedural flow-sensitivity gap in the unsafe-pointer encoding | bucket.go:138:19 |  |
| C022b | nil-deref | I I.I() == N { | 7 | FP/invariant | InBucket was already fully constructed by an earlier, different function (openBucket/tx.init/Bucket{InBucket:&...} literal) before the checking function or closure was ever entered - an interprocedural construction invariant | bucket.go:618:15, bucket.go:641:17, bucket.go:899:5 |  |
| C023 | nil-deref | I.I(I.I) | 7 | FP/invariant | fs from never-nil flag.NewFlagSet; m/page are address-of-slice-element offsets into a buffer sized by db.pageSize, which is forced positive at Open() before init()/Tx can exist | cmd/bbolt/main.go:1098:14, cmd/bbolt/main.go:1688:14, db.go:632:13, db.go:633:15, tx.go:404:15 |  |
| C024a | nil-deref | I.I = I.I() | 1 | FP/encoding | r BenchResults is passed by value; r.Duration() desugars to (&r).Duration(), the address of an addressable local/parameter, which can never be nil - analyzer mismodels a value type as a nilable pointer | cmd/bbolt/main.go:1072:30 |  |
| C024b | nil-deref | I.I = I.I() | 2 | FP/invariant | os.CreateTemp and Tx.allocate/DB.allocate never return (nil, nilError) - a paired-return invariant established at construction inside the callee | node.go:333:19, cmd/bbolt/main.go:1119:24 |  |
| C024c | nil-deref | I.I = I.I() | 4 | FP/requires-lifting | p passed into (*node).read is non-nil only via caller Bucket.node's branch structure plus Tx.page/DB.page's never-nil address-of-slice-element postcondition, which read itself never checks | node.go:163:4, node.go:163:15, node.go:164:4 |  |
| C025 | nil-deref | I I.I(I(I *I.I) I { | 7 | FP/invariant | bolt.Open's err==nil postcondition guarantees non-nil *DB, checked via immediate err!=nil guard before every db.View call. | cmd/bbolt/command_check.go:56:16, cmd/bbolt/command_inspect.go:37:16, cmd/bbolt/main.go:674:16, cmd/bbolt/main.go:806:16, cmd/bbolt/main.go:958:16 |  |
| C026a | nil-deref | I I := I.I.I(); I != I { | 2 | FP/encoding | same field (b.rootNode / db.file) checked nil/non-nil then read again later in the same function with no intervening reassignment; analyzer fails to correlate the two reads. | bucket.go:786:28, db.go:712:16 |  |
| C026b | nil-deref | I I := I.I.I(); I != I { | 5 | FP/invariant | embedded surgeryBaseOptions/db.file value field's sole construction site is a local stack variable, so its address can never be nil regardless of any guard. | db.go:1212:25, cmd/bbolt/command_surgery.go:233:41, cmd/bbolt/command_surgery.go:166:41 |  |
| C027 | bounds | I I(I[N], I) | 7 | FP/requires-lifting | cobra's Args: ExactArgs(1) is enforced by (*Command).execute before RunE runs, so args[0] is always in-bounds inside the RunE closure. | cmd/bbolt/command_surgery_freelist.go:36:42, cmd/bbolt/command_surgery_freelist.go:71:42, cmd/bbolt/command_surgery.go:121:35, cmd/bbolt/command_surgery.go:185:36, cmd/bbolt/command_surgery.go:62:41 |  |
| C028 | nil-deref | I.I(S, I.I.I(), I(I.I.I())), | 6 | FP/encoding | receiver is &stats.TxStats, the address of a value-typed embedded field of a local addressable variable, which Go guarantees is never nil. | internal/btesting/btesting.go:204:105, internal/btesting/btesting.go:204:57, internal/btesting/btesting.go:205:53, internal/btesting/btesting.go:205:97, internal/btesting/btesting.go:206:93 |  |
| C029a | nil-deref | I.I(I(I(I.I()))) | 3 | FP/requires-lifting | elem receivers (SetKsize/SetVsize) come from p.LeafPageElement/BranchPageElement pointer arithmetic on the p *Page parameter, whose non-nilness both callers of WriteInodeToPage already establish before calling it. | internal/common/inode.go:89:17, internal/common/inode.go:90:17, internal/common/inode.go:94:17 |  |
| C029b | nil-deref | I.I(I(I(I.I()))) | 3 | FP/encoding | item is the by-value range variable of []Inode; the address of a stack-local range variable can never be nil for any caller argument. | internal/common/inode.go:89:37, internal/common/inode.go:90:39, internal/common/inode.go:94:37 |  |
| C030 | nil-deref | I.I.I.I(I.I.I(I.I.I().I())) | 6 | FP/encoding | tx.db is proven non-nil by the early-return guard at tx.go:324 and is re-read three more times later in the same function with no intervening reassignment before close() nils it. | tx.go:338:30, tx.go:338:37, tx.go:338:41, tx.go:338:48, tx.go:338:8 |  |
| C031a | nil-deref | I, I := I.I(I) | 4 | FP/requires-lifting | receiver (top/tx/n/db) is proven non-nil in the same calling function either by an immediately preceding err!=nil guard on the call that produced it, or by prior dereferences of the identical unreassigned pointer, but the fact isn't lifted across the later call. | node.go:212:24, cmd/bbolt/main.go:1239:41, compact.go:42:31 |  |
| C031b | nil-deref | I, I := I.I(I) | 1 | FP/invariant | safety rests on walkBucket's parent-before-child traversal order: a bucket is always created (and its callback fired) before Compact ever descends into it, not on any local error guard. | compact.go:65:30 |  |
| C031c | nil-deref | I, I := I.I(I) | 1 | TP | exported Compact's dst *DB parameter is never nil-checked, and beginRWTx dereferences db.readOnly with no guard — a genuinely reachable nil-pointer panic. | compact.go:11:22 |  |
| C032 | nil-deref | I I.I[I(I.I)-N].I() == N { | 6 | FP/invariant | elemRef pushed onto the cursor stack is always built from pageNode()'s (page,node) pair, which can never be jointly nil, so stack-top field reads can't hit nil. | cursor.go:241:20, cursor.go:241:35, cursor.go:241:8, cursor.go:52:19, cursor.go:52:7 |  |
| C033 | nil-deref | I I.I(), I.I(), I.I() | 6 | FP/encoding | flagged receivers are addresses of in-bounds slice elements or unsafe pointer-arithmetic offsets (LeafPageElement), which Go/the arithmetic guarantees can never be nil; analyzer misencodes these as nilable dereferences. | cursor.go:381:19, cursor.go:381:34, cursor.go:381:49, cursor.go:386:17, cursor.go:386:45 |  |
| C034 | nil-deref | I := I.I(N) | 6 | FP/encoding | fs is bound directly from flag.NewFlagSet, which always returns a non-nil *FlagSet, and is never reassigned before the flagged fs.Arg(0) call. | cmd/bbolt/main.go:207:16, cmd/bbolt/main.go:262:16, cmd/bbolt/main.go:400:16, cmd/bbolt/main.go:567:16, cmd/bbolt/page_command.go:43:16 |  |
| C035 | nil-deref | I.I.I.I.I.I(I.I.I.I.I(), I.I.I.I(I.I)) | 5 | FP/invariant | node.bucket, Bucket.tx, and Tx.meta are set once at construction and remain non-nil for the entire lifetime a node can reach free(), foreclosed by tx.close()-ordering guarantees the analyzer can't see locally. | node.go:496:34, node.go:496:5, node.go:496:53, node.go:496:59, node.go:496:76 |  |
| C036a | nil-deref | I I.I() | 2 | TP | Sequence()/Root() getters omit the closed-tx guard their sibling setters (SetSequence) have, and Tx.Cursor() hands out a &tx.root alias that tx.close() nils in place — reproduced live as a real panic. | bucket.go:63:19, bucket.go:539:21 |  |
| C036b | nil-deref | I I.I() | 2 | FP/invariant | keyValue's elemRef can never have both page and node nil, since every construction path (pageNode/openBucket) sets at least one field before the bucket is inline. | cursor.go:245:20, cursor.go:165:19 |  |
| C036c | nil-deref | I I.I() | 1 | FP/requires-lifting | common.Assert(!tx.managed) panics inside Commit/Rollback before tx.db could be nilled if fn(t) tries to close t itself, so tx.db is unchanged when DB.Update calls t.Commit(). | db.go:915:17 |  |
| C037a | nil-deref | I I.I { | 1 | TP | Options.Logger's nil check only rejects a nil interface, not a non-nil interface wrapping a nil *DefaultLogger, so a caller-supplied typed-nil logger reaches a real nil-receiver panic in DefaultLogger.Debugf. | logger.go:64:7 |  |
| C037b | nil-deref | I I.I { | 1 | FP/invariant | childAt's receiver n is always minted non-nil by (*Bucket).node (cache hit or fresh &node{...}), and the only callers that could pass a nil parent already guard against it before recursing. | node.go:75:7 |  |
| C037c | nil-deref | I I.I { | 3 | FP/encoding | tx.db is dereferenced through the same tx pointer three lines earlier in the same function (if tx.db==nil return) with no reassignment before the flagged tx.writable check. | tx.go:349:8, tx.go:316:8, tx.go:327:8 |  |
| C038 | nil-deref | I I, I := I I.I { | 5 | FP/encoding | ranging over a nil Go map is legal and performs zero iterations (no dereference at all), and the map fields are also always non-nil after construction/Init. | internal/freelist/hashmap.go:237:29, internal/freelist/hashmap.go:255:29, internal/freelist/hashmap.go:271:27, internal/freelist/shared.go:113:27, internal/freelist/shared.go:224:24 |  |
| C039a | nil-deref | I = I.I() | 1 | FP/requires-lifting | child.inlineable()==true (checked immediately before) proves child.rootNode!=nil, and the intervening child.free() call never writes rootNode, so the fact must be carried across that sibling call into child.write(). | bucket.go:751:23 |  |
| C039b | nil-deref | I = I.I() | 4 | FP/encoding | analyzer fails to correlate multi-return-value postconditions (err==nil⇒result!=nil) across ReadMetaPageAt/DB.Begin, or treats unsafe pointer-arithmetic accessors (LeafPageElement) as dereferencing a nilable receiver. | tx_check.go:212:25, compact.go:80:17, cmd/bbolt/command_surgery_meta.go:153:25 |  |
| C040 | nil-deref | I[I.I.I()+I.I(I)] = I.I(I.I.I()) | 4 | FP/invariant | tx.db and tx.meta are always set/nilled jointly (co-nullity invariant), so tx.meta==nil would force tx.db==nil, which already nil-panics 19 lines earlier at the unconditional tx.db.loadFreelist() call. | tx_check.go:59:17, tx_check.go:59:30, tx_check.go:59:62, tx_check.go:59:75 |  |
| C041 | nil-deref | I(I.I(), I.I(S, I.I)) | 4 | FP/invariant | every *Page reaching these functions is constructed by taking the address of a live byte buffer (LoadPage/db.page), never returned as a nil literal, across every call site. | internal/common/page.go:125:106, internal/common/page.go:125:25, internal/common/page.go:143:104, internal/common/page.go:143:25 |  |
| C042 | nil-deref | I.I[I(I.I)-N].I = I | 4 | FP/requires-lifting | searchNode/searchPage's n/p parameters are proven non-nil at their sole call sites by the dispatching n!=nil branch condition (and pageNode's never-both-nil postcondition for the else branch), but the fact isn't lifted into the callee. | cursor.go:318:16, cursor.go:318:4, cursor.go:341:16, cursor.go:341:4 |  |
| C043 | nil-deref | I.I(S, I.I.I(), I.I.I()), | 4 | FP/encoding | receiver is the address of a value-typed TxStats field of a local addressable variable, used only for atomic-load getters, and Go guarantees such an address is never nil. | internal/btesting/btesting.go:199:54, internal/btesting/btesting.go:199:84, internal/btesting/btesting.go:201:56, internal/btesting/btesting.go:201:86 |  |
| C044 | nil-deref | I.I.I.I() | 4 | FP/encoding | db.rwtx/tx.db are checked non-nil by an adjacent guard in the same function with no intervening reassignment before the flagged dereference. | db.go:481:27, tx.go:357:6, tx.go:360:6, tx.go:366:6 |  |
| C045 | nil-deref | I.I.I.I.I(&I.I) | 4 | FP/encoding | tx.db is guarded non-nil at tx.go:346-348 with no reassignment before line 365, and &tx.stats/&tx.db.stats.TxStats are address-of struct fields Go guarantees non-nil regardless of receiver nilness. | tx.go:365:26, tx.go:365:26, tx.go:365:31, tx.go:365:6 |  |
| C046 | nil-deref | I.I = I(I.I, I{I: I, I: I, I: N}) | 4 | FP/invariant | pageNode()'s (page,node) pair can never be jointly nil at construction (cache/tx.page/db.page all non-nil), and the root bucket is never inline since RootPage is seeded to page 3. | cursor.go:185:22, cursor.go:185:5, cursor.go:47:21, cursor.go:47:4 |  |
| C047a | nil-deref | I.I = I.I[N].I() | 3 | FP/encoding | n.inodes[0] access is guarded by if len(n.inodes)>0 in the same function; the address of an in-bounds slice element can never be nil, and n itself was already dereferenced two statements earlier on the same path. | node.go:169:5, node.go:169:13, node.go:169:26 |  |
| C047b | nil-deref | I.I = I.I[N].I() | 1 | FP/invariant | node.inodes[0]'s non-emptiness is guaranteed cross-procedurally by splitTwo's size arithmetic (each split fragment gets >=2 inodes) and by rebalance deleting zero-inode non-root nodes before spill ever runs. | node.go:345:33 |  |
| C048 | nil-deref | I.I = I.I[:N] | 4 | FP/invariant | the *Cursor receiver c is always produced non-nil by the sole Cursor{} construction site (bucket.Cursor()), and every caller of seek/first obtains c directly from it. | cursor.go:161:14, cursor.go:161:4, cursor.go:45:14, cursor.go:45:4 |  |
| C049a | nil-deref | I, I := I.I.I(I.I.I()) | 2 | FP/invariant | col:12 c.bucket never nil (Cursor ctors deref receiver before building literal); col:69:45 Last() has same-function Assert(tx.db!=nil) immediately before, which by db/InBucket lockstep also proves InBucket non-nil. | cursor.go:69:45, cursor.go:46:12 |  |
| C049b | nil-deref | I, I := I.I.I(I.I.I()) | 2 | TP | recursivelyInspect calls unexported first() with no tx-closed guard (unlike First()/ForEachBucket); tx.Inspect() after Commit/Rollback reaches nil InBucket, confirmed by repro panic. | cursor.go:46:30, cursor.go:46:45 |  |
| C050 | nil-deref | I I() { I.I(I) }() | 4 | FP/requires-lifting | results param is pass-through chain from runReads to four unexported runReadsXxx methods, ultimately &readResults (stack-local value, never nil) at benchCommand.Run's sole call site. | cmd/bbolt/main.go:1325:43, cmd/bbolt/main.go:1363:43, cmd/bbolt/main.go:1403:43, cmd/bbolt/main.go:1439:43 |  |
| C051a | nil-deref | I I.I(S, I.I, I.I) | 2 | FP/invariant | PageError has exactly one construction site in the module (main.go:593), always &PageError{...} composite literal, so receiver e is never nil. | cmd/bbolt/main.go:1625:52, cmd/bbolt/main.go:1625:58 |  |
| C051b | nil-deref | I I.I(S, I.I, I.I) | 2 | FP/requires-lifting | InBucket.String() safety depends on caller-side guards not local to String(): page_command.go's IsBucketEntry() check gating leafPageElement.Bucket(), and bucket.go:377's prior successful RootPage() call on the same object. | internal/common/bucket.go:53:43, internal/common/bucket.go:53:51 |  |
| C052 | nil-deref | I I.I(): | 4 | FP/invariant | p := tx.page(pageId) never nil: either a dirty-page-map entry (only ever stored non-nil from successful db.allocate) or db.page's unsafe address-of-slice-element over the mmap'd buffer. | tx_check.go:102:19, tx_check.go:192:21, tx_check.go:207:19, tx_check.go:97:21 |  |
| C053a | nil-deref | I I.I == I { | 1 | FP/encoding | Rollback's own line-303 common.Assert(!tx.managed) already dereferences tx one line above the flagged tx.db==nil check, in the same function. | tx.go:304:8 |  |
| C053b | nil-deref | I I.I == I { | 3 | FP/requires-lifting | nonPhysicalRollback/rollback/close each start with the tx.db==nil check as their first statement; every caller has already dereferenced tx (directly or via its own guard) before invoking them. | tx.go:346:8, tx.go:313:8, tx.go:324:8 |  |
| C054a | nil-deref | I I = I.I() | 2 | FP/invariant | bucket is a fresh Bucket{...} literal with rootNode:&node{isLeaf:true} set in the same straight-line block immediately before the .write() call. | bucket.go:190:26, bucket.go:259:26 |  |
| C054b | nil-deref | I I = I.I() | 2 | FP/requires-lifting | PrintStats's sole caller (btesting.go:85) sits inside 'if db.DB != nil' (btesting.go:82), proving both the outer receiver and embedded db.DB non-nil, but this guard is never re-established inside PrintStats itself. | internal/btesting/btesting.go:197:14, internal/btesting/btesting.go:197:22 |  |
| C055 | nil-deref | I I := I(N); I < I.I(); I++ { | 4 | FP/encoding | flagged pointers are unsafe-pointer-cast/address-of-slice-element expressions (tx.page/db.page, LoadPage=&buf[0]) which Go semantics guarantee never yield nil; analyzer models these arithmetic-derived pointers as independently nullable. | bucket.go:653:36, cmd/bbolt/page_command.go:154:33, cmd/bbolt/page_command.go:193:33, internal/surgeon/xray.go:42:35 |  |
| C056 | nil-deref | I I := I I.I() { | 4 | FP/invariant | same tx.page(pageId) non-nil invariant as C052: dirty-page map only stores non-nil pointers, or db.page derives via unsafe address-of-slice-element over the mmap'd buffer. | tx_check.go:103:36, tx_check.go:195:38, tx_check.go:209:36, tx_check.go:98:38 |  |
| C057 | nil-deref | I = I.I(S, I(I.I())) | 4 | FP/encoding | flagged element e is UnsafeIndex pointer arithmetic (base+offset*i) off an already-dereferenced non-null base p=LoadPage(buf); analyzer treats the arithmetic-derived receiver as an independently nullable pointer. | cmd/bbolt/page_command.go:160:38, cmd/bbolt/page_command.go:162:38, cmd/bbolt/page_command.go:199:38, cmd/bbolt/page_command.go:201:38 |  |
| C058 | nil-deref | I += I + I(I(I.I())) + I(I(I.I())) | 4 | FP/encoding | item := &n.inodes[i] is address-of an in-bounds slice element, which Go guarantees non-nil regardless of slice contents; Key()/Value() have no further nilable indirection. | node.go:45:36, node.go:45:65, node.go:57:36, node.go:57:65 |  |
| C059 | nil-deref | I := I(I.I()) + I(I.I()) | 4 | FP/encoding | item is a value-typed range-loop variable (Inodes = []Inode, not []*Inode); &item is the address of the per-iteration stack copy, which Go can never make nil. | internal/common/inode.go:110:21, internal/common/inode.go:110:41, internal/common/inode.go:80:21, internal/common/inode.go:80:41 |  |
| C060a | nil-deref | I := I.I.I() | 3 | FP/encoding | db.meta0/meta1 are unsafe base+offset pointer arithmetic off db.data, proven non-nil by a preceding successful mmap; btesting.go:88 re-reads db.DB inside the same 'if db.DB!=nil' block with no intervening reassignment. | db.go:521:27, db.go:522:27, internal/btesting/btesting.go:88:13 |  |
| C060b | nil-deref | I := I.I.I() | 1 | FP/invariant | tx.db and tx.meta are only ever set together in Tx.init and cleared together in Tx.close; Commit's local tx.db==nil guard implies tx.meta non-nil only via this cross-method structural invariant, invisible from local control flow. | tx.go:200:23 |  |
| C061 | nil-deref | } I I I.I() { | 4 | FP/encoding | p already dereferenced earlier in the same if/else-if chain (IsBranchPage/IsLeafPage/etc. all read the identical p.flags field) on the same unmodified receiver, so later calls in the chain can't newly nil-deref. | bucket.go:662:27, internal/common/page.go:51:24, internal/common/page.go:53:24, internal/common/page.go:55:28 |  |
| C062 | bounds | I I(I).I(I[N:]...) | 4 | FP/encoding | args[1:] is always in-bounds because the line-113 guard forces len(args)>=1 before the switch is entered; solver posits an impossible slice header (nil data pointer with min-int64 length) no real []string can have. | cmd/bbolt/main.go:124:37, cmd/bbolt/main.go:126:39, cmd/bbolt/main.go:132:40, cmd/bbolt/main.go:134:35 |  |
| C063 | overflow | I.I.I(I, I()) | 3 | TP | options.KeySize is an unbounded CLI flag (default 8, no minimum check); binary.BigEndian.PutUint32 requires len>=4, so --key-size<4 reaches make([]byte,KeySize) then panics with index out of range. | cmd/bbolt/main.go:1199:31, cmd/bbolt/main.go:1236:30, cmd/bbolt/main.go:1251:31 |  |
| C064 | overflow | I !I.I(I) { | 3 | FP/encoding | ch is a rune decoded by range-over-a-utf8.ValidString-checked string, bounded to a valid/near-valid Unicode code point; analyzer's induction-variable model treats the decoded rune as an unconstrained 64-bit value instead. | cmd/bbolt/main.go:1634:22, cmd/bbolt/main.go:1634:22, cmd/bbolt/main.go:1634:22 |  |
| C065 | nil-deref | I(I.I(S, I, I.I)) | 3 | FP/encoding | f is already dereferenced by the enclosing range-over-f.freemaps/forwardMap/backwardMap loop header before the nested panic message re-reads the identical field on the same unmutated receiver. | internal/freelist/hashmap.go:242:88, internal/freelist/hashmap.go:259:89, internal/freelist/hashmap.go:275:90 |  |
| C066a | nil-deref | I(I.I, N, []I.I{I.I()}) | 2 | FP/invariant | the embedded *common.InBucket field is set immediately at every Bucket construction site (Tx.init, openBucket, the CreateBucket/CreateBucketIfNotExists literals), so it is never nil on a reachable *Bucket. | bucket.go:699:31, bucket.go:699:41 |  |
| C066b | nil-deref | I(I.I, N, []I.I{I.I()}) | 1 | FP/encoding | b is already dereferenced by the textually identical guard 'if b.page != nil' three lines above the flagged b.page access, on the same unmutated receiver. | bucket.go:699:8 |  |
| C067 | nil-deref | I(I.I, I) | 3 | FP/invariant | shared's pending/allocs/cache maps are always make()-initialized in newShared(), called by both freelist constructors, and no code path ever reassigns them to nil afterward. | internal/freelist/shared.go:110:11, internal/freelist/shared.go:115:13, internal/freelist/shared.go:96:12 |  |
| C068a | nil-deref | I.I[I] = I{}{} | 2 | FP/encoding | t.cache is unconditionally reassigned via make() two lines earlier in the same reindex function, with no intervening branch, before the flagged writes; analyzer misses the local heap-tracking update. | internal/freelist/shared.go:248:5, internal/freelist/shared.go:252:6 |  |
| C068b | nil-deref | I.I[I] = I{}{} | 1 | FP/invariant | Free has no local reassignment of t.cache; it relies solely on the package-wide invariant that newShared() always non-nil-initializes cache and it is never later set to nil. | internal/freelist/shared.go:85:5 |  |
| C069 | nil-deref | I.I[I] = I | 3 | FP/invariant | b.nodes write is guarded by an unconditional (not BOLT_VERIFY-gated) common.Assert(b.nodes!=nil) at the top of the same function; shared's allocs/pending maps are likewise always non-nil via newShared() construction invariant. | bucket.go:889:4, internal/freelist/shared.go:103:6, internal/freelist/shared.go:65:5 |  |
| C070 | nil-deref | I.I(I.I(I.I()).I()) | 3 | FP/invariant | receivers are addresses of local composite literals (&subStats, openBucket's unconditional &child) or bounded pointer arithmetic off an already-proven-nonnil mmap base (p proven non-nil by prior IsLeafPage() call), none of which Go can make nil. | bucket.go:658:19, bucket.go:658:32, bucket.go:658:40 |  |
| C071 | nil-deref | I.I(I, S, I.I()) | 3 | FP/encoding | p := LoadPage(buf) = &buf[0] can never be nil, and buf was already validated (Id/Overflow read successfully) by guts_cli.ReadPage before being dispatched to PrintLeaf/PrintBranch/PrintFreelist. | cmd/bbolt/page_command.go:150:44, cmd/bbolt/page_command.go:189:44, cmd/bbolt/page_command.go:217:45 |  |
| C072a | nil-deref | I.I(I, I) | 1 | FP/requires-lifting | cmd's non-nilness is proven only at the single external call site (newPageCommand(m) always returns &pageCommand{} literal); Run itself never unconditionally dereferences cmd before line 60 to reestablish it locally. | cmd/bbolt/page_command.go:60:20 |  |
| C072b | nil-deref | I.I(I, I) | 2 | FP/encoding | f is already unconditionally dereferenced via straight-line field writes (freePagesCount/freemaps/forwardMap/backwardMap) earlier in the same Init function, before either flagged addSpan call. | internal/freelist/hashmap.go:46:13, internal/freelist/hashmap.go:55:12 |  |
| C073a | nil-deref | I.I(&I.I, S, I.I, S) | 1 | FP/requires-lifting | fs threaded in via cobra.Command.Flags(), which always lazily returns non-nil pflag.FlagSet — postcondition analyzer never modeled. | cmd/bbolt/command_surgery.go:41:14 |  |
| C073b | nil-deref | I.I(&I.I, S, I.I, S) | 2 | FP/encoding | fs is a local flag.NewFlagSet(...) composite-literal allocation two lines above in the same function, never nil. | cmd/bbolt/main.go:1093:15, cmd/bbolt/main.go:1093:15 |  |
| C074 | nil-deref | I.I.I(I.I(I)) | 3 | FP/encoding | Pointer-receiver method address on value-typed tx.stats field (&tx.stats) misencoded as an independently nilable pointer; tx already proven non-nil. | tx.go:197:28, tx.go:209:23, tx.go:272:23 |  |
| C075 | nil-deref | I.I.I(I.I(I.I().I())) | 3 | FP/invariant | db.data/meta0/meta1 are only nilled by invalidate()/close() under locks that every loadFreelist caller (Open, tx.check) already holds, so page()/meta() calls can't see nil. | db.go:425:28, db.go:425:36, db.go:425:47 |  |
| C076 | nil-deref | I.I.I(I.I(), I) | 3 | FP/invariant | b.tx is set once in newBucket and never reassigned anywhere in the package; b.InBucket is set immediately at construction (root or openBucket) before any Bucket is returned to a caller. | bucket.go:704:19, bucket.go:704:29, bucket.go:704:4 |  |
| C077 | nil-deref | I.I.I().I(I.I.I()) | 3 | FP/invariant | tx.meta and tx.root.InBucket are set unconditionally in Tx.init and only cleared together in close(), which runs strictly after the flagged Commit line, so both are non-nil at tx.go:212. | tx.go:212:20, tx.go:212:34, tx.go:212:51 |  |
| C078 | nil-deref | I.I.I.I(N) | 3 | FP/invariant | Bucket.tx is assigned only at the two construction sites (newBucket, Tx.close's reset) and is unexported, so every live *Bucket has non-nil tx; parent==nil at bucket.go:892 is an explicitly handled case, not the deref cause. | bucket.go:76:27, bucket.go:892:25, bucket.go:892:4 |  |
| C079a | nil-deref | I.I.I.I(I.I.I(), I.I.I(I.I.I())) | 2 | FP/invariant | tx.meta's non-nilness at tx.go:216 follows only from the cross-function init/close pairing with tx.db, not from a local guard naming meta itself. | tx.go:216:35, tx.go:216:66 |  |
| C079b | nil-deref | I.I.I.I(I.I.I(), I.I.I(I.I.I())) | 1 | FP/encoding | tx.db at tx.go:216 is the exact field checked by Commit's own `if tx.db == nil` guard three lines above in the same function, with no intervening reassignment. | tx.go:216:49 |  |
| C080 | nil-deref | I.I.I.I.I(N) | 3 | FP/invariant | n.bucket and bucket.tx are set only at node/Bucket construction sites and propagated unchanged through split/rebalance/dereference, never reassigned to nil. | node.go:263:28, node.go:372:32, node.go:490:32 |  |
| C081 | nil-deref | I.I.I.I = I | 3 | FP/encoding | tx.db.stats writes sit inside the writable branch reached only via the fallthrough of close()'s own `if tx.db == nil { return }` guard in the same function. | tx.go:361:6, tx.go:362:6, tx.go:364:6 |  |
| C082 | nil-deref | I.I += I(I.I()) | 3 | FP/invariant | forEachPage's page parameter comes from tx.page/db.page, which resolve to pointer-arithmetic over live mmap memory (or a map entry) and can never be nil. | bucket.go:623:25, bucket.go:648:38, bucket.go:675:39 |  |
| C083 | nil-deref | I, I := I.I(I, I, &I, I) | 3 | TP | benchCommand.Run discards the error from CreateBucketIfNotExists (`b, _ := ...`) then unconditionally writes b.FillPercent, panicking when a prior -work run left an incompatible non-bucket value at the key. | cmd/bbolt/main.go:1035:28, cmd/bbolt/main.go:1035:28, cmd/bbolt/main.go:1035:28 |  |
| C084 | nil-deref | I, I := I.I.I(I.I.I, I.I\|I.I, N) | 3 | TP | WriteTo/Copy/CopyFile dereference tx.db/tx unguarded, unlike every other Tx/Bucket accessor that checks for ErrTxClosed, so calling WriteTo on an already-closed or nil Tx panics. | tx.go:391:15, tx.go:391:30, tx.go:391:54 |  |
| C085 | nil-deref | I I.I(S, I.I) | 3 | FP/invariant | GoString/String/Typ have no live callers holding a nil *DB anywhere in the tree; *DB only ever originates from Open, which pairs a nil db result with a non-nil error. | db.go:166:44, db.go:171:34, internal/common/page.go:58:40 |  |
| C086 | nil-deref | I I.I(S, I.I, I.I(), I.I, I.I) | 3 | FP/invariant | Page.String() has zero call sites anywhere in the module; every *Page producer (NewPage, db.page, db.pageInBuffer, tx.page) is guaranteed non-nil by construction. | internal/common/page.go:203:68, internal/common/page.go:203:83, internal/common/page.go:203:92 |  |
| C087a | nil-deref | I I.I(I, I) | 1 | FP/invariant | compact.go's bucket chain relies on walk/walkBucket's cross-function invariant that a bucket is always created before its children are visited, invisible from Compact's closure alone. | compact.go:76:15 |  |
| C087b | nil-deref | I I.I(I, I) | 2 | FP/encoding | tx.go MoveBucket has `if src == nil { src = &tx.root }` directly preceding the call with no intervening reassignment; analyzer fails to track the address-of-field reassignment it already traversed. | tx.go:150:23, tx.go:150:23 |  |
| C088 | nil-deref | I I.I.I(I) | 3 | FP/encoding | tx.root is a value field; pointer-receiver Bucket methods auto-address it as &tx.root, which cannot be nil since tx is already required non-nil to reach the enclosing method. | tx.go:112:12, tx.go:119:29, tx.go:126:40 |  |
| C089 | nil-deref | I I.I.I(I.I[I].I(), I) | 3 | FP/invariant | childAt panics on n.isLeaf before the flagged call; every node constructed with isLeaf==false always carries a non-nil bucket field inherited from its constructor or parent. | node.go:78:11, node.go:78:25, node.go:78:43 |  |
| C090a | nil-deref | I I.I.I() != I.I { | 1 | FP/invariant | tx.meta's non-nilness at tx_check.go:57 (col 21) relies only on the cross-field init/close pairing with tx.db, since tx.meta itself is never directly dereferenced earlier in check(). | tx_check.go:57:21 |  |
| C090b | nil-deref | I I.I.I() != I.I { | 2 | FP/encoding | The tx receiver and tx.meta are each re-dereferences of a value already dereferenced unconditionally earlier in the identical function (tx.db.loadFreelist at line 40; tx.meta.Pgid at tx.go:200). | tx.go:215:21, tx_check.go:57:8 |  |
| C091 | nil-deref | I I == I \|\| (I.I() == I.I() && I.I() != N) { | 3 | FP/invariant | MoveBucket's `if b.tx.db == nil \|\| dstBucket.tx.db == nil` gate precedes the RootPage() calls; InBucket only goes nil in lockstep with tx.db at close(), already excluded by that gate. | bucket.go:376:34, bucket.go:376:58, bucket.go:376:74 |  |
| C092 | nil-deref | I I = I.I.I.I() | 3 | FP/encoding | tx.db.freelist accesses are guarded three lines above by `if tx.db == nil { return }` in the same function block, with no intervening reassignment of tx.db. | tx.go:351:26, tx.go:352:29, tx.go:353:26 |  |
| C093 | nil-deref | I I := I(N); I <= I.I(I.I.I()).I(); I++ { | 3 | FP/invariant | tx.meta is set unconditionally in Tx.init before any Tx reaches check(); tx.page never returns nil since the dirty-page map only stores non-nil pages and the mmap fallback is pointer arithmetic. | tx_check.go:58:39, tx_check.go:58:52, tx_check.go:58:64 |  |
| C094 | nil-deref | I I := I.I(I, I, &I, I); I != I { | 3 | FP/encoding | Both db and cmd (the flagged call's candidates) are already dereferenced unconditionally earlier in the same function (main.go:1026 and 1047) before the flagged call at line 1049. | cmd/bbolt/main.go:1049:24, cmd/bbolt/main.go:1049:24, cmd/bbolt/main.go:1049:24 |  |
| C095 | nil-deref | I I := I.I(); I <= I.I()+I.I(I.I()); I++ { | 3 | FP/encoding | p is already dereferenced unconditionally via a panic-guard call (p.Id() at shared.go:56) before the same-path re-dereference three lines later. | internal/freelist/shared.go:77:16, internal/freelist/shared.go:77:30, internal/freelist/shared.go:77:55 |  |
| C096 | nil-deref | I I := &I.I[I(I.I)-N]; I.I != I && I.I() { | 3 | FP/encoding | ref is the address of a value-typed slice element (&c.stack[...]), which can never be a nil pointer regardless of stack contents; an Assert guarantees the index is in-bounds. | cursor.go:394:15, cursor.go:394:27, cursor.go:394:67 |  |
| C097 | nil-deref | I = I.I(I, I(S, I.I(I, I...))) | 3 | TP | DefaultLogger embeds *log.Logger with no exported constructor; zero-value struct passed via Options.Logger reaches Output() on the nil embedded pointer in Debugf/Infof/Errorf. | logger.go:65:7, logger.go:74:6, logger.go:82:6 |  |
| C098 | nil-deref | I = I.I.I(I.I[N].I.I(), I) | 3 | FP/invariant | c.bucket is set at every Cursor construction site (Bucket.Cursor/Tx.Cursor) so it's never nil, and pageNode's return-path invariant guarantees c.stack[0].page is non-nil whenever .node is nil. | cursor.go:401:23, cursor.go:401:39, cursor.go:401:9 |  |
| C099 | nil-deref | I += I(I.I() + I.I() + I.I()) | 3 | FP/invariant | lastElement is only computed inside the p.Count()!=0 guard on a p that tx.page/db.page never returns nil for, so LeafPageElement's pointer arithmetic can't yield a nil element. | bucket.go:638:36, bucket.go:638:58, bucket.go:638:80 |  |
| C100 | nil-deref | I := I.I(I(I *I.I) I { | 3 | FP/requires-lifting | MustCheck/Fill/CopyTempFile promote through embedded *bolt.DB with no local guard, but every grepped call site invokes them before any Close/MustClose on the same *btesting.DB. | internal/btesting/btesting.go:126:16, internal/btesting/btesting.go:165:19, internal/btesting/btesting.go:188:16 |  |
| C101 | overflow | I (*I)(I(I.I(I), I.I(*I), | 2 | FP/requires-lifting | index is declared uint16 so int(index) is provably in [0,65535], and with the constant 16-byte elemsz the UnsafeIndex product can never overflow uintptr on any real platform. | internal/common/page.go:110:41, internal/common/page.go:94:39 |  |
| C102 | nil-deref | S, I.I, I.I) | 2 | FP/encoding | p.id and p.flags at line 89 are dereferences of the same p already successfully dereferenced moments earlier on the same unconditional path (Assert at line 83, IsBranchPage/IsLeafPage/etc at 85-88). | internal/common/page.go:89:47, internal/common/page.go:89:53 |  |
| C103 | nil-deref | I(I(I.I()) > N, S) | 2 | FP/encoding | &inodes[i] is taken inside the loop's own i<len(inodes) bound and &item is the address of an addressable local range variable — neither can ever be the nil pointer under Go semantics. | internal/common/inode.go:64:23, internal/common/inode.go:76:22 |  |
| C104a | nil-deref | I(I.I(S, I.I(), I.I.I())) | 1 | FP/requires-lifting | node.spill only uses p after the err!=nil early return, but DB.allocate's 'err==nil => p!=nil' postcondition passes unchanged through Tx.allocate and is never lifted to node.spill's call site. | node.go:331:66 |  |
| C104b | nil-deref | I(I.I(S, I.I(), I.I.I())) | 1 | FP/invariant | tx.meta is set non-nil in Tx.init and only nilled in Tx.close, which always runs after tx.root.spill() completes in Commit, so tx.meta is a lifetime invariant during spill's recursive call chain. | node.go:331:82 |  |
| C105 | nil-deref | I(I.I(), S, I, I.I(), I, I, I, I, I) | 2 | FP/encoding | elem is pointer arithmetic (BranchPageElement) on p, which is already proven non-nil earlier in the same function via the dereferencing IsBranchPage()/Count() calls that select and drive this branch. | tx_check.go:197:28, tx_check.go:197:53 |  |
| C106a | nil-deref | I(I.I, I.I()) | 1 | FP/invariant | receiver t *shared is guaranteed non-nil for the freelist's whole lifetime because every fl.Interface constructor (NewArrayFreelist/NewHashMapFreelist) embeds a newShared()-produced shared that is never reassigned. | internal/freelist/shared.go:74:12 |  |
| C106b | nil-deref | I(I.I, I.I()) | 1 | FP/encoding | parameter p was already unconditionally dereferenced via the identical p.Id() call three lines earlier in the same function (the panic guard), proving p non-nil before line 74 reuses it. | internal/freelist/shared.go:74:24 |  |
| C107 | nil-deref | I(I.I == I, S, I, I.I) | 2 | FP/requires-lifting | FastCheck's p is proven non-nil only at its two call sites in Tx.page (tx.allocate/db.allocate's map branch and db.page's mmap branch, both address-of-slice-element), a fact never lifted into FastCheck's own body. | internal/common/page.go:83:11, internal/common/page.go:83:81 |  |
| C108 | nil-deref | I(I, I.I()) | 2 | FP/encoding | inode := &n.inodes[i] is the address of an in-bounds range-loop slice element, which Go guarantees is never the nil pointer, so Key()/Value() cannot nil-deref. | node.go:475:22, node.go:480:26 |  |
| C109 | nil-deref | I.I(S, I.I.I.I(), I.I.I.I()) | 2 | FP/encoding | b.tx.db and dstBucket.tx.db are proven non-nil by the disjunctive early-return guard three lines above and are already dereferenced one statement earlier in the same != comparison before the flagged reuse. | bucket.go:356:131, bucket.go:356:155 |  |
| C110 | nil-deref | I.I(I(I(I.I(&I[N])) - I(I.I(I)))) | 2 | FP/encoding | elem comes from UnsafeIndex's positive-offset pointer arithmetic on a non-null base; reaching the null address would require unrealizable 64-bit uintptr wraparound that no real Go allocation's address permits. | internal/common/inode.go:87:15, internal/common/inode.go:93:15 |  |
| C111 | nil-deref | I.I(I(I.I) > N, S) | 2 | FP/invariant | every *Cursor/*node is an address-of-composite-literal from Bucket.Cursor()/Bucket.node(), never nil, and c.stack's non-emptiness before .node() calls is guaranteed by the internal API's seek-before-node call-sequencing invariant. | cursor.go:391:22, node.go:170:23 |  |
| C112 | nil-deref | I.I(I(I.I()) > N, S) | 2 | FP/encoding | inode := &n.inodes[index]/&n.inodes[i] is the address of a slice element proven in-bounds by sort.Search/the enclosing range loop, so it can never be nil under Go semantics. | node.go:141:29, node.go:477:30 |  |
| C113 | nil-deref | I.I(I.I(I)) | 2 | FP/invariant | db.pageSize is forced positive in Open before init() ever runs (a construction-time invariant invisible to a purely local reading of init), so pageInBuffer/Meta's offset pointers are never nil. | db.go:627:10, db.go:638:12 |  |
| C114 | nil-deref | I.I(I.I(), N, I) | 2 | FP/invariant | forEachPageNode's only caller is (*Bucket).free(), invoked solely on already-constructed *Bucket values whose InBucket field openBucket()/Bucket() always populate before storage or return. | bucket.go:715:21, bucket.go:715:31 |  |
| C115 | nil-deref | I.I(I.I(), I+N, I) | 2 | FP/encoding | elem is BranchPageElement pointer arithmetic on a page p already proven non-null, and the second site's inode is a value-typed range variable whose implicit &inode address-of-local can never be nil — neither is a nullable Go pointer. | bucket.go:729:33, bucket.go:735:34 |  |
| C116a | nil-deref | I.I(I.I(), I, I, I, I) | 1 | FP/encoding | elem is BranchPageElement pointer arithmetic over the mmap-backed page p (itself from tx.page/db.page, never nil), so it cannot independently be a nullable pointer. | tx_check.go:100:45 |  |
| C116b | nil-deref | I.I(I.I(), I, I, I, I) | 1 | FP/invariant | b.InBucket is populated at every construction site (tx.root, tmpBucket literal, Bucket()/openBucket()) before recursivelyCheckBucket ever receives the value, so b.RootPage() cannot nil-deref. | tx_check.go:130:40 |  |
| C117 | nil-deref | I.I(I.I() \| I.I \| I.I) | 2 | TP | EnableTimestamps calls SetFlags/Flags on the same unguarded embedded *log.Logger as C097, reachable via a zero-value DefaultLogger since no exported constructor enforces initialization. | logger.go:50:12, logger.go:50:20 |  |
| C118a | nil-deref | I.I(I.I.I.I()) | 1 | FP/invariant | db.rwtx.meta is non-nil for the whole write transaction because beginRWTx sets db.rwtx via Tx.init (which unconditionally sets tx.meta) and only Tx.close clears it, strictly after the write/allocate phase, serialized by rwlock. | db.go:1165:27 |  |
| C118b | nil-deref | I.I(I.I.I.I()) | 1 | FP/encoding | p is address-of-slice-index (&buf[0]) constructed two lines earlier in the very same function, an intraprocedural fact the analyzer's unsafe.Pointer model fails to carry to the SetId call. | db.go:1165:9 |  |
| C119 | nil-deref | I.I(I.I.I.I != I, S) | 2 | FP/invariant | c.bucket and c.bucket.tx are set once at Cursor/Bucket construction and never reassigned; the assert deliberately probes tx.db (nilled only on close), which is the intended check, not an unguarded deref. | cursor.go:36:18, cursor.go:96:18 |  |
| C120 | nil-deref | I.I(I.I, S, I.I()) | 2 | FP/requires-lifting | guts_cli.ReadPage's uniform (nil,nil,err)/(p,buf,nil) return shape gives 'err==nil => p!=nil', guarded by an err!=nil early return at page_command.go:103-105, but the postcondition isn't lifted across the call. | cmd/bbolt/page_command.go:116:50, cmd/bbolt/page_command.go:119:60 |  |
| C121 | nil-deref | I.I(I, I.I.I()) | 2 | FP/invariant | Cursor.bucket is set from the non-nil method receiver at its sole construction site (Bucket.Cursor), and the embedded InBucket pointer is initialized at every Bucket-producing call site before the Bucket is ever returned. | cursor.go:162:19, cursor.go:162:34 |  |
| C122 | nil-deref | I.I(&I.I, S, S, S+I) | 2 | FP/encoding | flag.NewFlagSet always returns a fresh non-nil *FlagSet composite literal, assigned two lines above the flagged call with no reassignment. | cmd/bbolt/main.go:386:14, cmd/bbolt/main.go:386:14 |  |
| C123 | nil-deref | I.I(&I, S, S, S+I+S) | 2 | FP/encoding | fs is a freshly-constructed non-nil flag.NewFlagSet result and the flagged argument is the address of a local string var, which can never be nil. | cmd/bbolt/main.go:921:14, cmd/bbolt/main.go:921:14 |  |
| C124 | nil-deref | I.I(&I, S, S, S) | 2 | FP/encoding | Same pattern as C123: fs from flag.NewFlagSet is non-nil by construction and the StringVar argument is an address-of-local-var, never nil. | cmd/bbolt/main.go:920:14, cmd/bbolt/main.go:920:14 |  |
| C125 | nil-deref | I.I().I(I.I) | 2 | FP/invariant | db.meta() only returns after Validate() succeeds (else it panics rather than returning nil), and meta0/meta1 are set together with db.data under metalock/mmaplock so beginTx/beginRWTx's db.data==nil check rules out a nil meta before Tx.init runs. | tx.go:53:16, tx.go:53:16 |  |
| C126 | nil-deref | I.I().I(I.I().I()) | 2 | FP/encoding | page is an unsafe-pointer address into a non-empty freshly allocated buffer (indexing would panic first if empty), and Page.Meta() only adds a constant offset via UnsafeAdd, so it can never become nil. | tx.go:409:25, tx.go:419:25 |  |
| C127 | nil-deref | I.I() \|\| | 2 | FP/invariant | FastCheck's receiver p is always sourced from tx.pages (populated only from tx.allocate) or db.page (unsafe address of a mmap'd slice element), both provably non-nil, and IsBranchPage already dereferences the receiver earlier in the same \|\|-chain. | internal/common/page.go:86:15, internal/common/page.go:87:15 |  |
| C128 | nil-deref | I.I.I(S, I.I) | 2 | FP/invariant | The flagged db.f/db.t reads occur inside Close's `if db.DB != nil` branch (already passed on the recorded path), and both fields are set unconditionally at the sole constructor MustOpenDBWithOption and never reassigned. | internal/btesting/btesting.go:87:43, internal/btesting/btesting.go:87:6 |  |
| C129 | nil-deref | I.I.I(N) | 2 | FP/encoding | tx is dereferenced repeatedly (tx.db, tx.meta) earlier on the same recorded path before the flagged tx.stats access, so the pointer-receiver call on the value field &tx.stats cannot be a nil-tx dereference. | node.go:350:20, tx.go:577:19 |  |
| C130 | nil-deref | I.I.I(I.I()) | 2 | FP/invariant | p comes from tx.allocate, which only returns non-nil p paired with err==nil (both tx.allocate and the underlying db.allocate build p before any error branch), and commitFreelist already returns early on err!=nil before reaching the flagged use. | tx.go:295:21, tx.go:295:26 |  |
| C131 | nil-deref | I.I.I(I.I.I()) | 2 | FP/invariant | Every reachable *Tx had tx.meta set unconditionally by Tx.init before being stored in db.txs or returned by beginTx/beginRWTx, so t.meta/tx.meta is never nil at the flagged sites. | db.go:801:42, db.go:871:46 |  |
| C132 | nil-deref | I.I.I().I() | 2 | FP/encoding | The call is guarded by an immediately preceding `if b.rootNode != nil` in the same tiny non-reentrant function with no intervening reassignment, and (*node).root() preserves non-nil-ness by structural induction over the parent chain. | bucket.go:917:18, bucket.go:917:32 |  |
| C133 | nil-deref | I.I.I.I(&I.I) | 2 | FP/requires-lifting | removeTx has a single call site (tx.go:368) guarded by `if tx.db == nil { return }` in the caller, but removeTx itself carries no precondition the analyzer can see interprocedurally, so db/tx non-nilness isn't lifted into the callee. | db.go:880:22, db.go:880:22 |  |
| C134 | nil-deref | I.I.I.I = (I + I) * I.I.I | 2 | FP/encoding | Close begins with `if tx.db == nil { return }`, and the flagged tx.db uses sit inside the fallthrough writable branch with no reassignment of tx.db between the guard and the site. | tx.go:363:6, tx.go:363:67 |  |
| C135 | nil-deref | I.I.I = I | 2 | FP/encoding | Each flagged site sits behind an earlier leading guard in the same function (db.opened in DB.close, tx.db==nil in Tx.close) that the recorded path already passed, with prior same-path dereferences of the same pointer and no intervening reassignment. | db.go:693:5, tx.go:356:6 |  |
| C136 | nil-deref | I.I = N | 2 | FP/invariant | hashMap receivers are only ever produced non-nil by NewHashMapFreelist, and node receivers are only ever produced non-nil by (*Bucket).node, so Init/free never run on a nil receiver. | internal/freelist/hashmap.go:25:4, node.go:497:5 |  |
| C137 | nil-deref | I.I = I(I[I.I]I) | 2 | FP/invariant | The only production constructor NewHashMapFreelist always builds and returns a non-nil `&hashMap{...}` composite literal, so Init's f.forwardMap/f.backwardMap assignments never dereference a nil receiver. | internal/freelist/hashmap.go:27:4, internal/freelist/hashmap.go:28:4 |  |
| C138 | nil-deref | I.I = I(I.I, I) | 2 | FP/encoding | append(nil-slice, elem) is defined Go semantics that never dereferences a nil pointer; the analyzer's model incorrectly treats append's incoming nil slice as a nil-deref requirement. | cursor.go:289:21, cursor.go:289:4 |  |
| C139 | nil-deref | I.I = I.I[:I+N] | 2 | FP/encoding | The reslice c.stack[:i+1] is only reached after the preceding loop successfully indexed c.stack[i], which proves len(c.stack) > i, so the reslice cannot be a nil/out-of-range dereference. | cursor.go:236:15, cursor.go:236:5 |  |
| C140 | nil-deref | I.I = I.I(N).I() | 2 | FP/encoding | Every platform mmap() implementation sets db.data to a non-nil pointer unconditionally before returning a nil error, but the analyzer fails to propagate that err==nil postcondition to the db.page call site. | db.go:515:20, db.go:516:20 |  |
| C141 | nil-deref | I.I = I.I() - N | 2 | FP/invariant | pageNode's every branch yields a non-nil page or node: cached nodes are checked non-nil before return, tx.page/db.page never return nil (unsafe address into mmap'd slice), and inline-bucket b.page is set at construction before rootNode is ever nulled. | cursor.go:208:32, cursor.go:71:23 |  |
| C142 | nil-deref | I.I = I.I.I(&I.I) | 2 | FP/encoding | The recorded path trail already took the `other != nil` branch, and the receiver s is dereferenced repeatedly (s.FreePageN etc.) immediately before the flagged call on the same straight-line path. | db.go:1385:30, db.go:1385:30 |  |
| C143a | nil-deref | I.I = I.I.I() | 1 | FP/invariant | b.rootNode.root() follows an explicit `if b.rootNode == nil { return nil }` guard, and the only intervening call, b.rootNode.spill(), never reassigns the Bucket's rootNode field. | bucket.go:789:30 |  |
| C143b | nil-deref | I.I = I.I.I() | 1 | FP/requires-lifting | db.file.Name() relies on the externally-substitutable Options.OpenFile hook honoring the documented-but-unenforced convention that err==nil implies a non-nil *os.File, which nothing in Open checks. | db.go:239:24 |  |
| C144 | nil-deref | I: I(I.I()), | 2 | FP/invariant | p is the unsafe-pointer address of a mmap'd slice element (db.page), which can never be nil, and Count()/Overflow() are plain field reads on that same never-reassigned p. | tx.go:638:29, tx.go:639:32 |  |
| C145 | nil-deref | I, I := I(I.I()[N:]) | 2 | FP/invariant | fs from flag.NewFlagSet is never nil and is already dereferenced via fs.Bool/fs.Parse/fs.Arg earlier in the same function before the flagged fs.Args() call. | cmd/bbolt/main.go:270:39, cmd/bbolt/page_command.go:52:40 |  |
| C146 | nil-deref | I, I := I.I[I.I()] | 2 | FP/encoding | p.Id() is already called unconditionally three lines earlier (shared.go:57) on the same pointer, so the later map-index dereference at shared.go:67 can't be the first fault point. | internal/freelist/shared.go:67:21, internal/freelist/shared.go:67:32 |  |
| C147 | nil-deref | I, I := I.I(N), I.I(N) | 2 | FP/invariant | fs from flag.NewFlagSet is never nil and fs.Bool/fs.Parse already dereference it earlier in the function before the flagged fs.Arg(0)/fs.Arg(1) calls. | cmd/bbolt/main.go:660:24, cmd/bbolt/main.go:660:35 |  |
| C148 | nil-deref | I, I := I.I(I.I(N), N, N) | 2 | FP/invariant | fs from flag.NewFlagSet is never nil, and fs.BoolVar/fs.Parse/fs.Arg(0) already dereference it earlier on the same path before the flagged fs.Arg calls. | cmd/bbolt/main.go:408:41, cmd/bbolt/main.go:414:41 |  |
| C149 | nil-deref | I, I := I.I(I, I, I.I()-I(I.I.I*N)) | 2 | FP/encoding | tx.db is already dereferenced unconditionally at tx.go:391 (openFile/path) earlier in WriteTo, before the flagged tx.db use at tx.go:432 could ever be reached with a nil tx.db. | tx.go:432:35, tx.go:432:47 |  |
| C150a | nil-deref | I, I := I.I((I.I() + I.I.I - N) / I.I.I) | 1 | FP/requires-lifting | tx.allocate's safety depends on Commit's early tx.db==nil guard, a fact that must be lifted through tx.root.spill()->Bucket.spill->node.spill down to node.go:324. | node.go:324:24 |  |
| C150b | nil-deref | I, I := I.I((I.I() + I.I.I - N) / I.I.I) | 1 | FP/invariant | the range variable node comes from n.split's returned []*node, whose only elements are n itself or freshly-allocated &node{} from splitTwo, never nil by construction. | node.go:324:35 |  |
| C151 | nil-deref | I, I := I.I.I(I) | 2 | FP/invariant | c.bucket can never be nil because the sole Cursor constructor, (*Bucket).Cursor(), already dereferences its receiver b before building the Cursor struct that stores it as bucket. | cursor.go:184:13, cursor.go:284:12 |  |
| C152 | nil-deref | I, I := I.I.I(I.I.I(), I) | 2 | FP/requires-lifting | tx.db/tx.meta non-nilness at tx.allocate is guaranteed only by Commit's early tx.db==nil guard, which must be carried across the commitFreelist/spill call chain rather than re-derived locally. | tx.go:461:26, tx.go:461:39 |  |
| C153 | nil-deref | I I(I).I(I[N:]...) | 2 | FP/encoding | newBenchCommand/newPageItemCommand have a single unconditional return of a freshly-allocated composite literal, so their result (and thus the Run receiver) is never nil. | cmd/bbolt/main.go:124:32, cmd/bbolt/main.go:132:35 |  |
| C154 | nil-deref | I I(I(I.I())) { | 2 | FP/encoding | e from p.LeafPageElement/BranchPageElement is pure pointer arithmetic (UnsafeIndex) off a non-nil base p, and adding a positive offset to a non-zero address can never yield nil. | cmd/bbolt/page_command.go:159:30, cmd/bbolt/page_command.go:198:30 |  |
| C155 | nil-deref | I I(I.I()), I.I(I.I()), I | 2 | FP/encoding | m from common.LoadPageMeta is an address-of-slice-element on an already-filled non-empty buffer, which in Go can never evaluate to a nil pointer. | internal/guts_cli/guts_cli.go:112:26, internal/guts_cli/guts_cli.go:112:49 |  |
| C156 | nil-deref | I I(I.I()), I, I | 2 | FP/encoding | m.Validate() at db.go:369/405 already dereferences m (magic/version/checksum/Sum64) one statement before the flagged m.PageSize() call in the same if-body, precluding a nil m at that point. | db.go:370:25, db.go:406:26 |  |
| C157a | nil-deref | I I(I.I.I()) | 1 | FP/invariant | r.page is non-nil whenever r.node==nil because Bucket.pageNode always returns exactly one non-nil of (page,node) across every construction site of elemRef. | cursor.go:431:25 |  |
| C157b | nil-deref | I I(I.I.I()) | 1 | FP/encoding | tx.go:72's dereference is reached only after the adjacent guard `if tx==nil \|\| tx.meta==nil { return -1 }` (tx.go:69-70) fails, whose negation the analyzer fails to propagate to the fallthrough tx.meta.Txid() call. | tx.go:72:25 |  |
| C158 | nil-deref | I I(I, I, I, I, I.I(), I) | 2 | FP/invariant | bkt from b.Bucket(k) re-seeks the exact key ForEach just yielded with BucketLeafFlag set inside the same immutable read-only-tx snapshot, so it must find the same non-nil bucket again. | compact.go:115:56, compact.go:117:49 |  |
| C159 | nil-deref | I I.I(I[I].I[N].I(), I[I].I[N].I()) == -N | 2 | FP/encoding | s[i]/s[j] are non-nil by construction (nodes slice populated only with real *node pointers), and .Key() operates on an addressable inodes[0] slice element, never a nil-deref site. | node.go:537:41, node.go:537:63 |  |
| C160a | nil-deref | I I.I(I(I, I []I) I { | 1 | FP/requires-lifting | lastBucket's non-nilness at main.go:878 relies on findLastBucket's err==nil-implies-result-non-nil postcondition, which the per-function analysis doesn't carry past the err!=nil check at the call site. | cmd/bbolt/main.go:878:28 |  |
| C160b | nil-deref | I I.I(I(I, I []I) I { | 1 | FP/invariant | bkt from b.Bucket(k) re-seeks the same BucketLeafFlag-set key ForEach just reported, within the same immutable db.View snapshot, guaranteeing a non-nil result — a cross-call cursor-consistency invariant. | compact.go:112:18 |  |
| C161 | nil-deref | I I.I(I.I() - N).I() | 2 | FP/invariant | p from tx.page(pgId) is either a cached page or an address-of-slice-element from db.data, which Go never yields as nil, making p.Count()/LeafPageElement(...).Key() safe field reads. | tx_check.go:215:36, tx_check.go:215:47 |  |
| C162 | nil-deref | I I.I(I, I, I, I() I { I I.I() }) | 2 | FP/requires-lifting | r's non-nilness at r.Uint32() is only provable by tracing the parameter across two call boundaries back to its sole constructor rand.New(rand.NewSource(time.Now().UnixNano())) in benchCommand.Run. | cmd/bbolt/main.go:1170:86, cmd/bbolt/main.go:1179:92 |  |
| C163 | nil-deref | I I.I().I(), I, I | 2 | FP/encoding | m from GetActiveMetaPage's LoadPageMeta is an address-of-slice-element, never nil, and RootBucket() returns &m.root (address-of-field), which never evaluates to nil regardless of m. | internal/guts_cli/guts_cli.go:121:21, internal/guts_cli/guts_cli.go:121:32 |  |
| C164 | nil-deref | I I.I(), I.I(), I | 2 | FP/encoding | e from p.LeafPageElement is pointer arithmetic off common.LoadPage's address-of-slice-element base, which can never be the Go nil value. | cmd/bbolt/main.go:457:14, cmd/bbolt/main.go:457:25 |  |
| C165 | nil-deref | I I.I() >= I.I.I() { | 2 | FP/invariant | on the err==nil branch actually reached, p from tx.allocate/db.allocate is always constructed via address-of-nonempty-buffer, and tx.meta is set non-nil in init and only cleared during teardown after Commit's spill() has already run. | node.go:330:10, node.go:330:28 |  |
| C166 | nil-deref | I I.I() == N \|\| I.I >= I.I() { | 2 | FP/invariant | elemRef can never have both page and node nil because Bucket.pageNode always returns exactly one non-nil of the pair at every construction site. | cursor.go:374:14, cursor.go:374:47 |  |
| C167 | nil-deref | I I.I() < I.I() { | 2 | FP/invariant | m0/m1 from LoadPageMeta are address-of-slice-element pointers on buffers ReadPage only returns fully populated (errors bail out earlier), so they can never be nil. | internal/guts_cli/guts_cli.go:136:12, internal/guts_cli/guts_cli.go:136:24 |  |
| C168 | nil-deref | I I.I() + I.I() | 2 | FP/invariant | receiver t *shared is never nil since shared's only constructor newShared() is always invoked via &array{shared: newShared()}/&hashMap{shared: newShared()}, never left as an uninitialized nil pointer. | internal/freelist/shared.go:48:39, internal/freelist/shared.go:48:9 |  |
| C169 | nil-deref | I I.I() != N { | 2 | FP/invariant | p is (*Page)(unsafe.Pointer(&slice[i])) from tx.page()/db.allocate(); slice index is either valid non-nil or panics, never nil. | bucket.go:628:14, db.go:1160:9 |  |
| C170 | nil-deref | I I.I() != I.I(I) { | 2 | FP/invariant | p := LoadPage(buf) = &buf[0] on a non-empty buf (post successful ReadAt); address-of-element construction is never nil. | internal/guts_cli/guts_cli.go:45:9, internal/guts_cli/guts_cli.go:65:9 |  |
| C171 | nil-deref | I I.I.I() > I.I.I() { | 2 | FP/invariant | meta0/meta1 are nil only inside mmap/close's exclusive mmaplock window; callers reach meta() only under RLock with opened/data checks. | db.go:1129:18, db.go:1129:36 |  |
| C172 | nil-deref | I I.I.I.I() != I.I.I.I() \|\| I.I != I.I { | 2 | FP/encoding | The `b.tx.db==nil \|\| dstBucket.tx.db==nil` guard one statement above proves both pointers non-nil before line 355 uses them. | bucket.go:355:17, bucket.go:355:43 |  |
| C173a | nil-deref | I I.I.I == I { | 1 | FP/encoding | DeleteBucket's init-clause `b.tx.db.Logger()` unconditionally dereferences b.tx one statement before the flagged nil-check. | bucket.go:285:7 |  |
| C173b | nil-deref | I I.I.I == I { | 1 | TP | ForEachBucket's first statement dereferences b with no prior guard; Bucket() can return nil, reachable via external API chaining. | bucket.go:599:7 |  |
| C174 | nil-deref | I I.I < N \|\| I.I >= I(I.I.I()) { | 2 | FP/encoding | tx.db/tx.meta already dereferenced earlier in the same check() body (lines 38, 54) with no reassignment before line 77. | tx_check.go:77:48, tx_check.go:77:57 |  |
| C175 | nil-deref | I I, I, I.I(S, I, I.I(), I) | 2 | FP/encoding | p.Id() already called one line earlier in the if-condition on the same p before the flagged Errorf call reuses it. | internal/guts_cli/guts_cli.go:46:96, internal/guts_cli/guts_cli.go:66:96 |  |
| C176a | nil-deref | I I, I := I.I[I]; I { | 1 | FP/invariant | shared.cache map is unconditionally set in newShared(), the sole constructor path for array/hashmap freelists; never nil. | internal/freelist/shared.go:79:17 |  |
| C176b | nil-deref | I I, I := I.I[I]; I { | 1 | FP/encoding | tx.pages nil-map read is textually guarded by `if tx.pages != nil` one line above; a guard-propagation failure. | tx.go:587:18 |  |
| C177 | nil-deref | I I, I := I.I.I[I.I()]; I { | 2 | FP/encoding | inode is a range loop variable over a value slice ([]Inode); &inode is the address of a stack variable, never nil. | node.go:392:46, node.go:432:44 |  |
| C178 | nil-deref | I I, I := I I.I[:I(I.I)-N] { | 2 | FP/encoding | c already dereferenced via c.stack at lines 391/394 before line 403; solver widening across the loop back-edge drops the fact. | cursor.go:403:24, cursor.go:403:37 |  |
| C179 | nil-deref | I I = I.I.I(I(I.I.I()+N) * I.I.I); I != I { | 2 | FP/requires-lifting | Commit's `tx.db==nil` guard at line 185 isn't carried to the tx.db.grow()/tx.meta.Pgid() calls at line 235 in the same function. | tx.go:235:22, tx.go:235:39 |  |
| C180 | nil-deref | I I = I.I | 2 | FP/invariant | b.page/b.tx trace to &tx.root or openBucket()'s always-non-nil &child; no path stores or uses a nil *Bucket here. | bucket.go:876:12, bucket.go:903:13 |  |
| C181 | nil-deref | I I = &I.I[I(I.I)-N] | 2 | FP/requires-lifting | Callers first()/next() each establish len(c.stack)>=1 immediately before calling goToFirstElementOnTheStack; callee can't see it. | cursor.go:172:16, cursor.go:172:28 |  |
| C182 | nil-deref | I I != I.I() \|\| I.I() { | 2 | FP/invariant | p from ReadPage/LoadPage's &buf[0]; also already dereferenced (IsLeafPage/IsBranchPage) earlier in the same function. | internal/surgeon/surgeon.go:105:30, internal/surgeon/surgeon.go:105:50 |  |
| C183a | nil-deref | I I := N; I < I(I.I()); I++ { | 1 | FP/requires-lifting | ReadInodeFromPage's param p is proven non-nil independently at each of its two callers, invisible inside the shared callee. | internal/common/inode.go:52:29 |  |
| C183b | nil-deref | I I := N; I < I(I.I()); I++ { | 1 | FP/invariant | p := tx.page(...) is a local single-site call whose non-nilness follows purely from tx.page()'s own internal invariant. | tx.go:614:30 |  |
| C184 | nil-deref | I I := I(I); I != I { | 2 | FP/invariant | db receiver traces back to the single db := &DB{opened:true} allocation in Open(); never reassigned to nil anywhere. | db.go:546:18, db.go:706:21 |  |
| C185 | nil-deref | I I := I(I.I); I != I { | 2 | FP/requires-lifting | write()/writeMeta()'s sole caller Commit() already checked tx.db==nil at line 184, not carried into fdatasync(tx.db) calls. | tx.go:526:22, tx.go:570:22 |  |
| C186 | nil-deref | I I := I.I[I]; I != I { | 2 | FP/encoding | b.nodes[pgId] is a plain map index read, never panics even on a nil map; there is no dereference here to be nil. | bucket.go:863:12, bucket.go:942:13 |  |
| C187 | nil-deref | I I := I.I(N); I < I.I().I(); I++ { | 2 | FP/invariant | freepages' two call sites (Open after mmap; rollback on a live writable tx) only run while db.meta0/meta1 are mmap-populated. | db.go:1257:38, db.go:1257:45 |  |
| C188 | nil-deref | I I := I.I(N); I < I.I.I(); I++ { | 2 | FP/invariant | check()'s only caller Check() runs it on a live *Tx before close(); tx.meta is set non-nil in init, nil'd only in close(). | tx_check.go:69:35, tx_check.go:69:44 |  |
| C189 | nil-deref | I I := I.I(I[:], N).I(); I.I() == I { | 2 | FP/encoding | buf is a non-empty stack array; pageInBuffer+Meta() do fixed-offset unsafe arithmetic off a non-nil base, never nil. | db.go:369:56, db.go:405:57 |  |
| C190 | nil-deref | I I := I.I(I, I); I != I { | 2 | FP/encoding | b already dereferenced via an unconditional b.FillPercent=... field write before the flagged b.Put call in the loop. | cmd/bbolt/main.go:1202:20, cmd/bbolt/main.go:1254:20 |  |
| C191 | nil-deref | I = I.I(I.I(), I) | 2 | FP/invariant | RootPage()'s InBucket is set at every construction site (tx.init/openBucket); SetSequence/NextSequence guard db==nil/!Writable first. | bucket.go:553:24, bucket.go:572:24 |  |
| C192 | nil-deref | I = I.I(I.I(), I.I(), I, I, I, I) | 2 | FP/requires-lifting | tx.page()'s never-nil postcondition and BranchPageElement's nonzero-offset contract aren't carried to elem.Pgid()/Key() uses. | tx_check.go:203:71, tx_check.go:203:83 |  |
| C193 | nil-deref | I = I.I(I, I) | 2 | FP/requires-lifting | ReadPage's postcondition (err==nil implies *Page non-nil via LoadPage's unsafe cast) carried across the err guard; p never reassigned before UsedSpaceInPage/WriteInodeToPage calls. | internal/surgeon/surgeon.go:81:39, internal/surgeon/surgeon.go:87:40 |  |
| C194 | nil-deref | I = I.I.I[I.I].I() | 2 | FP/encoding | Pointer-receiver method invoked on addressable element of a value-typed slice (ref.node.inodes[i]); Go auto address-of a slice element can never be nil. | cursor.go:180:42, cursor.go:201:42 |  |
| C195 | nil-deref | I = I.I.I(I(I.I)).I() | 2 | FP/invariant | (*Bucket).pageNode never returns (nil,nil): in the else-arm where ref.node==nil, ref.page is guaranteed non-nil across all its return paths. | cursor.go:182:61, cursor.go:203:61 |  |
| C196 | nil-deref | I <- I.I(S, I.I(), I) | 2 | FP/invariant | tx.page() returns a mmap/dirty-page pointer that is validated by FastCheck's unconditional Assert before the default arm is ever reached, so p is a non-nil valid page there. | tx_check.go:119:75, tx_check.go:218:75 |  |
| C197 | nil-deref | I <- I.I(S, I.I, N, I.I.I()) | 2 | FP/invariant | tx.db and tx.meta are always set/cleared together (init/close), a co-nil invariant, so surviving the earlier tx.db.loadFreelist() call proves tx.meta non-nil too. | tx_check.go:78:77, tx_check.go:78:86 |  |
| C198 | nil-deref | I += I(I.I() + I.I()) | 2 | FP/invariant | BranchPageElement is pure pointer arithmetic (UnsafeIndex) over an always-non-nil base page from tx.page/db.page, so the derived element pointer can never be nil. | bucket.go:673:35, bucket.go:673:57 |  |
| C199 | nil-deref | I += I.I + I(I(I.I())) + I(I(I.I())) | 2 | FP/encoding | inode is a range-loop variable over a value-typed slice ([]Inode); the compiler-inserted &inode for the pointer-receiver call is an address of an addressable local, never nil. | bucket.go:814:61, bucket.go:814:91 |  |
| C200 | nil-deref | I !I.I() && !I.I() { | 2 | FP/requires-lifting | ReadPage's per-return-path postcondition (every non-nil-error return pairs with a nil *Page; only the LoadPage-derived success return is error-free) guarantees p!=nil after the err guard. | internal/surgeon/surgeon.go:43:18, internal/surgeon/surgeon.go:43:39 |  |
| C201 | nil-deref | I !I.I() { | 2 | FP/requires-lifting | db.mmap()'s postcondition that db.data/meta0/meta1 become non-nil together is carried through Open()/tx_check.go's call into loadFreelist() and onward into db.page()/freelist.Read(p). | db.go:420:27, internal/freelist/shared.go:258:22 |  |
| C202a | nil-deref | I !I.I.I() { | 1 | FP/encoding | The tx.db==nil guard fact established at tx.go:324 is lost across the intervening tx.db.freelist.Rollback(...) call at line 328 even though tx.db is never reassigned before line 332. | tx.go:332:11 |  |
| C202b | nil-deref | I !I.I.I() { | 1 | FP/invariant | Cross-field invariant: db.data, db.meta0, db.meta1 are always set together by mmap() and cleared together by invalidate(), so the tx.db.data!=nil guard at tx.go:331 implies meta0/meta1 non-nil inside hasSyncedFreelist(). | tx.go:332:31 |  |
| C203 | nil-deref | I !I.I { | 2 | FP/invariant | *DB is only ever constructed non-nil in Open() (db = &DB{...}) and never reassigned to nil before internal close()/Close() call sites reuse the same receiver. | db.go:684:9, db.go:704:10 |  |
| C204 | nil-deref | I := I([]I, I(I.I())) | 2 | FP/encoding | inode := &n.inodes[i] taken inside a range over a value-typed slice is a valid in-bounds address, never nil, contradicting the analyzer's generic nilable-receiver precondition for Key()/Value(). | node.go:474:36, node.go:479:40 |  |
| C205 | nil-deref | I := I.I[I] | 2 | FP/invariant | shared receiver is always constructed non-nil by NewArrayFreelist/NewHashMapFreelist, and db.rwlock acquisition ordering prevents DB.Close() from nilling db.freelist concurrently with a live writer's Free/Rollback call. | internal/freelist/shared.go:62:11, internal/freelist/shared.go:91:11 |  |
| C206 | nil-deref | I := I.I(S).I(I, N) | 2 | FP/invariant | runtime/pprof registers the built-in "heap" and "block" profiles unconditionally at package init, so pprof.Lookup on these literal names can never return nil. | cmd/bbolt/main.go:1541:38, cmd/bbolt/main.go:1550:39 |  |
| C207 | nil-deref | I := I.I(S, S, S+I+S) | 2 | FP/encoding | fs is the direct, unconditional result of flag.NewFlagSet(...) executed immediately before in the same function, and that stdlib constructor never returns nil. | cmd/bbolt/main.go:839:28, cmd/bbolt/page_command.go:33:26 |  |
| C208 | nil-deref | I := I.I(I(I.I), I(I I) I { I I.I(I.I[I].I(), I) != -N }) | 2 | FP/encoding | sort.Search only invokes its closure for i within [0,len(n.inodes)), so &n.inodes[i] is always a valid in-bounds, non-nil address. | node.go:127:93, node.go:147:93 |  |
| C209 | nil-deref | I := I.I(I(I.I()), I(I I) I { | 2 | FP/encoding | p was already successfully dereferenced one line earlier (p.count read via BranchPageElements/LeafPageElements) with no reassignment, so the later p.Count() dereference of the same pointer is already proven non-nil. | cursor.go:329:34, cursor.go:363:34 |  |
| C210 | nil-deref | I := I.I(I.I() - N) | 2 | FP/requires-lifting | The Stats$1 closure parameter p is treated as an unconstrained external entry point, but both its concrete call sites (guarded b.page, and tx.page's never-Go-nil result) guarantee p!=nil. | bucket.go:637:45, bucket.go:664:46 |  |
| C211 | nil-deref | I := I.I().I() | 2 | FP/encoding | RootBucket()/RootPage() return address-of-struct-field expressions (&m.root) off an already non-nil m, which can never themselves be nil. | internal/surgeon/xray.go:37:21, internal/surgeon/xray.go:37:32 |  |
| C212 | nil-deref | I := I.I() + I(I(I.I())) + I(I(I.I())) | 2 | FP/encoding | inode := n.inodes[i] copies a value into a stack-local; the compiler's implicit &inode for the pointer-receiver call is an address of a local variable, never nil. | node.go:278:56, node.go:278:86 |  |
| C213 | nil-deref | I := I.I.I(I) | 2 | TP | tx.page() dereferences tx.db without a nil check, and tx.db is genuinely nilled by close(); unguarded read-path methods (Bucket.Get, Cursor.First/Next/Last) reuse a post-close Tx and reach this real nil-deref. | tx.go:594:10, tx.go:594:17 |  |
| C214 | nil-deref | I := I * (I(I.I()) + N) | 2 | FP/encoding | page/p comes from common.LoadPage's unsafe pointer cast off a slice address, which is structurally non-nullable (panics on empty buf rather than returning nil). | internal/guts_cli/guts_cli.go:78:49, internal/surgeon/surgeon.go:100:41 |  |
| C215 | bounds | I I[N] < I[N] { | 2 | FP/encoding | Two preceding len(a)==0/len(b)==0 early returns are not propagated forward to the later a[0]/b[0] reads, though surviving both guarantees both slices non-empty at that line. | internal/common/page.go:372:13, internal/common/page.go:372:6 |  |
| C216 | bounds | I I(I[N]) | 2 | FP/requires-lifting | Cobra's configured Args:ExactArgs(1) validator runs and can reject before RunE executes, guaranteeing len(args)==1 whenever args[0] is read inside the closure, but the analyzer has no visibility into cobra's execute() calling that validator first. | cmd/bbolt/command_inspect.go:19:27, cmd/bbolt/command_surgery_meta.go:41:39 |  |
| C217 | bounds | I I(I.I(I), N, I, I) | 2 | FP/encoding | leafPageElement.Key()/Value() already dereference n.pos/n.ksize/n.vsize on the same receiver before the flagged UnsafeByteSlice call, so a nil n would fault earlier. | internal/common/page.go:301:24, internal/common/page.go:308:24 |  |
| C218a | bounds | I I, I := I I[N:] { | 1 | FP/requires-lifting | bucketNames[1:] needs the caller-side `len(buckets)==0 -> ErrBucketRequired` guards in keysCommand.Run/get-value command lifted across the findLastBucket call boundary. | cmd/bbolt/main.go:1788:36 |  |
| C218b | bounds | I I, I := I I[N:] { | 1 | FP/encoding | keys[1:] is directly guarded two lines above by `if nk > 1` where nk is an unreassigned alias of len(keys) set earlier in the same function. | compact.go:55:26 |  |
| C219a | bounds | I (*I)(I.I(&I[N])) | 1 | FP/invariant | bucket-leaf values are always written as a full InBucket-sized blob at every Bucket.write()/DeleteBucket path before BucketLeafFlag is set, so LoadBucket's &buf[0] is never empty when IsBucketEntry() gated it. | internal/common/utils.go:11:40 |  |
| C219b | bounds | I (*I)(I.I(&I[N])) | 1 | TP | guts_cli.ReadPageAndHWMSize trusts on-disk pageSize after only a magic-number check (no Meta.Validate()), so a corrupted file with pageSize==0 makes LoadPage's &buf[0] panic, reachable via cmd/bbolt page/page-item/surgery commands. | internal/common/utils.go:15:36 |  |
| C220 | bounds | I (*I)(I.I(&I[I])) | 2 | TP | InlinePage's v[BucketHeaderSize] and LoadPageMeta's buf[PageHeaderSize] have no length checks and are reachable from surgeon/guts_cli's explicitly-corrupted-file-handling paths that never validate on-disk vsize/pageSize. | internal/common/bucket.go:49:34, internal/common/utils.go:19:36 |  |
| C221 | overflow | I.I(I(I)) | 1 | FP/encoding | start is guarded to [0, elementCnt) by an earlier bounds check in the same function, and elementCnt=int(p.Count()) is provably <=65535 since Count() returns uint16, so uint16(start) never truncates. | internal/surgeon/surgeon.go:78:20 |  |
| C222 | overflow | I.I(I.I() \| I.I \| I.I) | 1 | TP | DefaultLogger has no constructor guarding its embedded *log.Logger, so a zero-value &DefaultLogger{} leaves it nil and EnableTimestamps/Debug/Panic nil-deref on the embedded logger. | logger.go:50:12 |  |
| C223 | overflow | I, I := I.I(I) | 1 | FP/requires-lifting | splitIndex's uintptr sz accumulation is bounded by Bucket.Put's MaxKeySize/MaxValueSize gate on every inode's key/value, a fact never lifted from the bucket.go call boundary into node.go's split logic. | node.go:246:31 |  |
| C224 | overflow | I, I := I.I((I.I() + I.I.I - N) / I.I.I) | 1 | FP/requires-lifting | post-split node.size() is bounded by pageSize+MaxKeySize+MaxValueSize via Bucket.Put's size gate, a bound never lifted into the tx.allocate page-count division/multiplication. | node.go:324:24 |  |
| C225 | overflow | I, I := I.I((I.I.I.I() / I.I.I) + N) | 1 | FP/requires-lifting | EstimatedWritePageSize's tracked-id count is bounded by the database's real page count (db.datasz), a fact never lifted into tx.allocate's count*pageSize multiplication in db.allocate. | tx.go:288:23 |  |
| C226 | overflow | I I.I(S).I(I.I(), S) | 1 | FP/encoding | the real stdlib time.Duration.String() negates via a uint64 cast (well-defined modular arithmetic), so MinInt64 never signed-overflows; the analyzer's stdlib contract models a naive signed negation instead. | internal/btesting/btesting.go:211:70 |  |
| C227 | overflow | I I.I(I(I.I.I()), I.I) | 1 | FP/invariant | os.File.Fd() returns a kernel-assigned fd bounded by the process's fd table (RLIMIT_NOFILE), never near int overflow magnitude, and db.file is nil-guarded by db.close's `if db.file != nil` before funlock runs. | bolt_unix.go:50:22 |  |
| C228 | overflow | I <- I.I(S, I(I.I()), I(I), I) | 1 | TP | branch-element pgid and hwm are raw uint64 fields read off disk with only an id==id equality check (FastCheck), so a crafted 0x8000... pgid wraps negative in int(p.Id()) inside Check()'s corruption-diagnostic message. | tx_check.go:152:78 |  |
| C229 | overflow | I := I(I.I(I), I.I(*I), I, I) | 1 | FP/requires-lifting | FreelistPageCount's idx return is provably {0,1} from its own two assignment sites, but that return-value range is never propagated across the call into UnsafeIndex's n argument at the flagged call. | internal/common/page.go:151:21 |  |
| C230 | overflow | I := I.I(I(I), I) | 1 | FP/invariant | db.file.Fd() returns a small kernel-assigned fd bounded by the open-file-table size; the solver's witness value is inconsistent with any real Fd() magnitude near overflow. | bolt_unix.go:31:23 |  |
| C231 | nil-deref | I(I[I:], I.I()) | 1 | FP/encoding | item is a range-loop-local addressable variable, so item.Value()'s implicit &item receiver can never be nil regardless of the model's nil witness. | internal/common/inode.go:101:25 |  |
| C232 | nil-deref | I(I.I(S, I.I())) | 1 | FP/encoding | p.Id() at line 58 is a redundant re-dereference of the same p already dereferenced unconditionally by the enclosing `if p.Id() <= 1` condition one line above, same straight-line function. | internal/freelist/shared.go:58:56 |  |
| C233 | nil-deref | I(I.I(S, I.I(), I)) | 1 | FP/requires-lifting | closure Free$1 captures p as a free variable already dereferenced twice in the enclosing Free before the closure's single synchronous call site via common.Verify, so the caller-established non-nil fact isn't carried across the closure-creation boundary. | internal/freelist/shared.go:70:94 |  |
| C234 | nil-deref | I(I.I(S, I.I(), I.I())) | 1 | FP/encoding | p.IsFreelistPage() at line 258 already dereferences p (reads p.flags) unconditionally before the reused p.Id()/p.Typ() calls two lines later at line 259, same straight-line function, no reassignment. | internal/freelist/shared.go:259:71 |  |
| C235 | nil-deref | I(I.I(S, I.I.I, I.I.I.I())) | 1 | FP/invariant | b.tx.meta is nilled only inside Tx.close(), which runs strictly after tx.root.spill() returns during Commit (itself gated by an already-closed check), making tx.meta non-nil throughout spill's lifetime — a lifetime invariant invisible to a local read of spill(). | bucket.go:793:92 |  |
| C236 | nil-deref | I(I.I(S, I, I.I.I.I.I())) | 1 | FP/encoding | the panic message's n.bucket.tx.meta.Pgid() call is textually identical to the already-evaluated if-condition one line above in the same unreassigned block, so it cannot newly nil-deref. | node.go:119:88 |  |
| C237 | nil-deref | I(I.I() \|\| | 1 | FP/encoding | FastCheck's p.IsBranchPage() reuses the same receiver p already dereferenced by the unconditional Assert(p.id==id, ...) two lines earlier in the same straight-line function. | internal/common/page.go:85:23 |  |
| C238 | nil-deref | I(I.I() != I.I(), S) | 1 | FP/encoding | p is already dereferenced twice earlier in the same block (p.PageElementSize()->p.IsLeafPage() and a direct p.IsLeafPage() call) before the loop reaches the flagged use, and elem itself comes from non-nil-base pointer arithmetic that can't yield nil either. | internal/common/inode.go:96:20 |  |
| C239 | nil-deref | I(I.I, I(I)) | 1 | FP/encoding | receiver b is already dereferenced twice earlier in DeleteBucket (b.tx.db.Logger() and b.tx.db==nil check) before the flagged delete(b.buckets,...), with b never reassigned in between. | bucket.go:317:11 |  |
| C240 | nil-deref | I(I.I, I, N) | 1 | FP/encoding | the flagged b.page argument is guarded by the textually identical `if b.page != nil` condition one line above in the same basic block on the same unmutated b. | bucket.go:712:8 |  |
| C241 | nil-deref | I(I, S, I, I.I(), I, I, I, I, I) | 1 | FP/encoding | p already unconditionally dereferenced twice earlier (branch/leaf case checks) so non-nil, and LeafPageElement offset arithmetic off a non-nil base cannot land on nil. | tx_check.go:211:44 |  |
| C242 | nil-deref | I(I, I.I.I(), I, I, I, I) | 1 | FP/requires-lifting | tx.meta already dereferenced successfully in the calling (*Tx).check one frame up with no intervening write before reaching the forEachPage closure. | tx_check.go:144:38 |  |
| C243 | nil-deref | I.I[I(I)] = I | 1 | FP/encoding | write at bucket.go:107 is lexically guarded by an identical, unmodified 'b.buckets != nil' check on the immediately preceding line with no intervening assignment. | bucket.go:107:5 |  |
| C244 | nil-deref | I.I[I.I()] = I | 1 | FP/encoding | allocate's only error path returns p==nil paired with err!=nil, and on the success path p is &buf[0] of a guaranteed non-empty slice, so p can never be nil. | tx.go:468:15 |  |
| C245 | nil-deref | I.I(S, I.I(), I.I.I, I) | 1 | FP/encoding | pageInBuffer computes &buf[id*pageSize] over a freshly allocated non-empty slice (pageSize forced positive), so the pointer can never be the null address. | tx.go:565:70 |  |
| C246 | nil-deref | I.I(S, I.I.I()), | 1 | FP/encoding | GetCursorCount is called via auto-address-of on a value-typed embedded struct field of an addressable local, which can never be nil, not a nilable pointer dereference. | internal/btesting/btesting.go:200:54 |  |
| C247 | nil-deref | I.I(S, I.I.I(), I.I.I, I) | 1 | FP/invariant | tx.meta set non-nil in init and only nilled in close; already dereferenced successfully multiple times earlier on the same Commit path with no intervening write. | tx.go:236:87 |  |
| C248 | nil-deref | I.I(S, I.I.I(), I, I) | 1 | FP/invariant | tx.meta is non-nil for the whole live-transaction lifetime between init and close, and is dereferenced successfully one statement earlier on the same path with no reassignment. | tx.go:463:78 |  |
| C249 | nil-deref | I.I(I/I(I) - N) | 1 | FP/requires-lifting | ReadPage's err==nil ⇒ p!=nil postcondition (via LoadPage's unconditional unsafe cast) is not lifted across the call site into ClearPageElements. | internal/surgeon/surgeon.go:95:16 |  |
| C250 | nil-deref | I.I(I(I)) | 1 | FP/requires-lifting | same ReadPage err==nil ⇒ p!=nil postcondition not lifted across the surgeon.go:38 call site to the SetCount call at line 78. | internal/surgeon/surgeon.go:78:13 |  |
| C251 | nil-deref | I.I(I(I), I(I, I I) { | 1 | FP/encoding | stdlib rand.New unconditionally returns &Rand{...}, a non-nil address-of-composite-literal, with no reassignment before the receiver's use three statements later. | cmd/bbolt/main.go:1041:12 |  |
| C252 | nil-deref | I.I(I(I(I))) | 1 | FP/invariant | ReadPage has a strict error-or-page contract (nil,nil,err on every error path; non-nil LoadPage result otherwise) so p is non-nil by the callee's return-value invariant at the guarded call site. | internal/surgeon/surgeon.go:86:13 |  |
| C253 | nil-deref | I.I(I(I.I)) | 1 | FP/requires-lifting | Open forces db.pageSize positive before calling init, but init reads the field locally with no parameter carrying that fact, so the analyzer treats pageSize as possibly zero. | db.go:634:16 |  |
| C254 | nil-deref | I.I(I(I, I.I()), I) | 1 | FP/invariant | p is confirmed non-nil via an earlier same-function IsBranchPage dereference, and bounded uint16-indexed pointer arithmetic off a real mmap/dirty-page base cannot reach the top of the address space to hit nil. | tx.go:616:54 |  |
| C255 | nil-deref | I.I(I(I - N)) | 1 | FP/requires-lifting | allocate's count parameter is unconstrained locally, but both real call sites (node.go:324, tx.go:288) always pass count>=1 via ceiling-division/+1 arithmetic, so buf is never empty. | db.go:1156:15 |  |
| C256 | nil-deref | I.I(I.I(N, N)) | 1 | FP/invariant | same as C253/C113 pattern: db.pageSize forced positive in Open before init runs, making pageInBuffer/Meta's underlying buffer non-empty and pointer non-nil, invisible to a purely local read of init. | db.go:636:18 |  |
| C257 | nil-deref | I.I(I.I(), S) | 1 | FP/requires-lifting | cobra's (*Command).execute nil-checks its receiver before invoking RunE(c,...), so the framework guarantees cmd is non-nil, a fact the analyzer can't see across the third-party callback boundary. | cmd/bbolt/command_check.go:70:31 |  |
| C258 | nil-deref | I.I(I.I(), I) | 1 | FP/requires-lifting | same cobra RunE(c,...) non-nil receiver guarantee as C257, not lifted through the closure/checkFunc call chain to the OutOrStdout use at line 59. | cmd/bbolt/command_check.go:59:32 |  |
| C259 | nil-deref | I.I(I.I.I) | 1 | FP/invariant | every real construction path (newBucket call sites) sets b.InBucket immediately before the Bucket is ever exposed, so a live spill()-reachable *Bucket always has a non-nil embedded InBucket. | bucket.go:795:15 |  |
| C260 | nil-deref | I.I(I.I.I(I, I)) | 1 | FP/encoding | p is &buf[0] of a buffer guaranteed non-empty (pagePool.New or count*pageSize with count>=1 at every call site), and slice-element address-of can never yield nil. | db.go:1159:9 |  |
| C261 | nil-deref | I.I(I.I.I() > N, S) | 1 | FP/encoding | the assert is reached only via fall-through of an explicit 'if n.parent == nil { return }' guard with no intervening assignment to n.parent, a same-function guard-negation the solver's own path trail fails to correlate. | node.go:416:36 |  |
| C262 | nil-deref | I.I(I.I, S, I, I.I()) | 1 | FP/requires-lifting | ReadMetaPageAt's err==nil ⇒ m!=nil postcondition (LoadPageMeta over a fixed 1024-byte buffer) is not lifted across the call site past the err!=nil guard to line 169. | cmd/bbolt/command_surgery_meta.go:169:142 |  |
| C263 | nil-deref | I.I(I.I != I, S) | 1 | FP/invariant | every real call site of (*Bucket).node passes a receiver already proven non-nil by construction-time field-setting or a dominating prior dereference in the same caller function. | bucket.go:860:18 |  |
| C264 | nil-deref | I.I(I, S, I, I.I()) | 1 | FP/encoding | LoadPage's &buf[0] either panics on empty buf or yields a genuine non-null address, and bounded uint16-indexed offset arithmetic off that base cannot produce the null address. | cmd/bbolt/page_command.go:204:46 |  |
| C265 | nil-deref | I.I(I, I[I].I()) | 1 | FP/encoding | inodes[index] is addr of in-bounds slice element of value-typed branchPageElement, base slice already proven non-nil via prior p.count deref; can't nil-deref. | cursor.go:344:34 |  |
| C266 | nil-deref | I.I(I, I.I[I].I()) | 1 | FP/encoding | &n.inodes[index] is addr of in-bounds element of value-typed Inodes slice (guarded by prior n!=nil check in search), can never be nil. | cursor.go:321:36 |  |
| C267 | nil-deref | I.I(I, I.I(), I) | 1 | FP/encoding | Mergepgids uses checked panic for length invariant plus copy()-based early returns on empty a/b, which are safe no-ops on nil/empty slices, so no nil-pointer deref occurs. | internal/freelist/shared.go:213:25 |  |
| C268 | nil-deref | I.I(I, I, I) | 1 | FP/encoding | formatValue comes from flag.FlagSet.String which does new(string) internally, an allocation that's never nil, so the *string param can't be nil at dereference. | cmd/bbolt/page_command.go:58:17 |  |
| C269 | nil-deref | I.I(I / I(I)) | 1 | FP/invariant | ReadPage/LoadPage invariant (err==nil implies p!=nil) is built via unsafe.Pointer arithmetic on &buf[0], which the analyzer's pointer-nullability model can't see through. | internal/surgeon/surgeon.go:97:16 |  |
| C270 | nil-deref | I.I(&I.I, I, I, I.I, I) | 1 | FP/encoding | tx already dereferenced multiple times earlier in same function (tx.db.loadFreelist, tx.page(0/1), tx.meta.Freelist) dominating the flagged &tx.root use, an intraprocedural flow-tracking gap. | tx_check.go:66:33 |  |
| C271 | nil-deref | I.I(&I, S, I, S) | 1 | FP/encoding | &enableRoot is address of a declared package-level var, which can never be nil; analyzer's flag.BoolVar nil-deref precondition is structurally unsatisfiable there. | tests/utils/helpers.go:12:14 |  |
| C272 | nil-deref | I.I([]I.I{}) | 1 | FP/invariant | embedded Interface field of shared is always wired to a non-nil concrete value (a.Interface=a / hm.Interface=hm) at the sole two constructors used before Read ever runs. | internal/freelist/shared.go:266:3 |  |
| C273 | nil-deref | I.I().I(S, I.I, I.I, I.I, I) | 1 | FP/invariant | db.Logger() is a nil-safe accessor returning the always-non-nil package-level discardLogger, so db.Logger().Errorf() can't nil-deref regardless of db's nilness. | db.go:547:121 |  |
| C274 | nil-deref | I.I().I() | 1 | FP/encoding | page is &buf[0] cast (never nil or panics first), and Meta() adds a small constant offset via unsafe.Pointer arithmetic, which can't produce nil from a non-nil base — pure local intraprocedural fact. | tx.go:418:21 |  |
| C275 | nil-deref | I.I(), | 1 | FP/invariant | Both call sites of FastCheck pass a page derived from address-of-slice-element constructions that are never nil; also receiver would have already panicked at prior \|\| operand IsBranchPage. | internal/common/page.go:88:19 |  |
| C276 | nil-deref | I.I(!I.I, S) | 1 | FP/invariant | every Tx-producing path (beginTx/beginRWTx/Update/View) returns non-nil t paired with nil error before Rollback is ever invoked on it. | tx.go:303:20 |  |
| C277 | nil-deref | I.I( | 1 | FP/requires-lifting | rootCmd and all four newXCommand() constructor results are addresses of local composite literals with no nil-returning branch, never lifted into the AddCommand call site. | cmd/bbolt/command_root.go:19:20 |  |
| C278 | nil-deref | I.I.I(I(I)) | 1 | FP/encoding | tx.stats is a value field so &tx.stats derives from tx pointer itself, which is already dereferenced earlier on the same path (tx.db.Logger(), tx.db.allocate) before reaching the flagged call. | tx.go:471:23 |  |
| C279 | nil-deref | I.I.I(I(I * I.I.I)) | 1 | FP/encoding | same as sibling IncPageCount call: &tx.stats address derives from tx, already proven non-nil by earlier tx.db dereferences on the identical recorded path. | tx.go:472:23 |  |
| C280 | nil-deref | I.I.I(I.I) | 1 | FP/encoding | tx.meta dereferenced three times earlier on the identical Commit path (Pgid, RootBucket, Freelist) with no reassignment before the flagged SetFreelist call, so it's provably non-nil there too. | tx.go:226:22 |  |
| C281 | nil-deref | I.I.I(I, I.I[N].I(), I, I.I, N) | 1 | FP/invariant | node.parent.put is only reached inside if node.parent != nil, and rebalance (run before spill) deletes any zero-inode node with a non-nil parent, guaranteeing len(inodes)>=1. | node.go:344:43 |  |
| C282 | nil-deref | I.I.I.I(I) | 1 | FP/encoding | tx.db.freelist is set non-nil by loadFreelist one statement earlier in the same function, and DB.close (the only nil-setter) is lock-excluded from running concurrently with an open Tx. | tx_check.go:45:5 |  |
| C283 | nil-deref | I.I.I.I(I.I.I(), I) | 1 | FP/invariant | tx (=b.tx) is set only via non-nil-parameterized newBucket/close reset, and reachable free() call sites are guarded (DeleteBucket's db==nil check) or run pre-close during spill, so tx.meta is non-nil. | bucket.go:906:36 |  |
| C284 | nil-deref | I.I.I.I(I.I.I(), I.I(I.I)) | 1 | FP/invariant | tx.meta/tx.db are set and cleared together in Tx.init/close, and node.spill's only call chain runs synchronously inside Commit strictly before tx.close(), so tx.meta is non-nil there. | node.go:319:36 |  |
| C285 | nil-deref | I.I.I.I(I + I.I(I)) | 1 | FP/invariant | db.rwtx is set at begin and cleared only in Tx.close at the very end of Commit, and db.allocate is only reached from spill/commitFreelist which run before that close, so db.rwtx is non-nil. | db.go:1175:22 |  |
| C286 | nil-deref | I.I = S | 1 | FP/encoding | db receiver already dereferenced multiple times earlier on the identical path (the opened guard plus several field reads) before the flagged db.path assignment; db is never reassigned in close. | db.go:718:5 |  |
| C287 | nil-deref | I.I = I{I: I} | 1 | FP/encoding | tx receiver already safely dereferenced by the tx.db==nil guard and subsequent field reads earlier on the same path before the flagged tx.root assignment; tx is never reassigned in close. | tx.go:374:5 |  |
| C288 | nil-deref | I.I = I(I[I]I) | 1 | FP/invariant | hashMap is only ever constructed non-nil via NewHashMapFreelist, and Init is reached only via construct-then-Init call sites with no nil-constructing path in between. | internal/freelist/hashmap.go:26:4 |  |
| C289 | nil-deref | I.I = I(I[I.I]I{}, I(I)) | 1 | FP/invariant | shared always built via newShared(); prior dereference of same receiver at line 244 already assumes non-nil on this path | internal/freelist/shared.go:246:4 |  |
| C290 | nil-deref | I.I = I(I.I()) | 1 | FP/encoding | auto address-of on addressable by-value BenchResults param for pointer-receiver CompletedOps() can never be nil | cmd/bbolt/main.go:1073:38 |  |
| C291 | nil-deref | I.I = I.I(N) | 1 | FP/invariant | fs assigned from flag.NewFlagSet, which always returns non-nil *FlagSet, no reassignment before use | cmd/bbolt/main.go:1702:22 |  |
| C292 | nil-deref | I.I = I.I(I) | 1 | FP/invariant | p passed into node.read is non-nil by bucket-page construction invariant, and read() already dereferences p twice earlier in the same function before the flagged line | node.go:165:4 |  |
| C293 | nil-deref | I: &I{I: I.I()}, | 1 | FP/invariant | tx.page falls back to db.page's unsafe pointer-cast of a slice element, which Go never yields as nil | tx_check.go:109:45 |  |
| C294 | nil-deref | I, I, I := I.I().I(I) | 1 | TP | Bucket(name) documented to return nil for a missing bucket, and Cursor()/Get() dereference that receiver with no nil guard | bucket.go:434:25 |  |
| C295 | nil-deref | I, I, I := I.I() | 1 | FP/invariant | pageNode never returns (nil,nil): inline and non-inline bucket construction sites always set page or rootNode | cursor.go:56:27 |  |
| C296 | nil-deref | I, I = I(I.I(), I) | 1 | FP/invariant | LeafPageElement is UnsafeIndex pointer arithmetic on an already non-nil page base, so its result can't be nil | cmd/bbolt/page_command.go:172:32 |  |
| C297 | nil-deref | I, I = I.I(I) | 1 | FP/requires-lifting | Open's non-nil-dst-or-err postcondition holds at Compact's sole call site in cmd/bbolt/main.go, guarded before use | compact.go:31:23 |  |
| C298 | nil-deref | I, I = I.I(I, I(I.I())*I(I)) | 1 | FP/invariant | LoadPage returns an unsafe cast of &buf[0], an address-of-slice-element that is never nil | internal/guts_cli/guts_cli.go:87:43 |  |
| C299 | nil-deref | I, I = I.I() | 1 | FP/invariant | db.file lifecycle invariant enforced by lock ordering: mmaplock held during mmap/fileSize prevents Close from nulling db.file concurrently | db.go:457:29 |  |
| C300 | nil-deref | I, I := I.I() | 1 | FP/requires-lifting | beginTx is guaranteed to return non-nil tx at both call sites because db.opened/db.data invariants (Open's construction, tx.go's data!=nil guard plus rwlock ordering) already hold there, though the defer-before-err-check ordering in freepages is a latent local defect | db.go:1232:23 |  |
| C301 | nil-deref | I I(I) | 1 | FP/invariant | Open is the sole *DB constructor and every failure path returns nil,err, so any *DB a caller legitimately holds is non-nil | db.go:1093:18 |  |
| C302 | nil-deref | I I(I.I) > N && I.I[I(I.I)-N].I() == N { | 1 | FP/invariant | pageNode's postcondition (at least one of page/node non-nil) is established at all bucket construction sites across bucket.go/tx.go/db.go | cursor.go:77:55 |  |
| C303 | nil-deref | I I(I.I) > N { | 1 | FP/requires-lifting | all three callers of unexported node.read construct the receiver via a fresh &node{...} literal immediately before the call | node.go:168:11 |  |
| C304 | nil-deref | I I(I.I.I()) * I(I.I.I) | 1 | TP | tx.close() nils tx.meta/tx.db but Size() omits the closed-tx guard present in its sibling ID() and in every Bucket/Cursor method | tx.go:82:27 |  |
| C305 | nil-deref | I I.I(I[I].I(), I) != -N | 1 | FP/encoding | LeafPageElements returns a value-struct slice; the pointer-receiver Key() call auto-addresses the slice element, which is never nil | cursor.go:364:37 |  |
| C306 | nil-deref | I I.I(I.I[I].I(), I) != -N | 1 | FP/encoding | n.inodes is a value slice; pointer-receiver Key() auto-addresses the element under an already-checked n!=nil guard, never nil | cursor.go:355:40 |  |
| C307 | nil-deref | I I.I(I.I(I).I(), I) { | 1 | FP/invariant | traverse callback's page parameter is always sourced from ReadPage/InlinePage unsafe-cast constructors that never yield nil | internal/surgeon/xray.go:88:48 |  |
| C308 | nil-deref | I I.I()&I.I != N { | 1 | FP/encoding | range-loop value copy inode is auto-addressed for pointer-receiver Flags(); address of a local variable is never nil | bucket.go:816:17 |  |
| C309 | nil-deref | I I.I().I() != I.I | 1 | FP/invariant | meta0/meta1 are co-assigned with db.data on mmap and co-cleared on invalidate; all three call sites hold that invariant | db.go:432:27 |  |
| C310 | nil-deref | I I.I(), I | 1 | FP/invariant | ReadPage only returns non-nil p paired with nil err, and the err is checked before p is used at the flagged line | cmd/bbolt/page_command.go:135:19 |  |
| C311 | nil-deref | I I.I() > N { | 1 | FP/invariant | tx.page falls back to db.page's unsafe pointer-cast of a slice element, never nil, during Check() on an open Tx | tx_check.go:214:13 |  |
| C312 | nil-deref | I I.I() > I { | 1 | FP/invariant | p reaches verifyPageReachable via forEachPage->tx.page->db.page's unsafe slice-element cast, never nil during Check() | tx_check.go:151:9 |  |
| C313 | nil-deref | I I.I() <= N { | 1 | FP/invariant | Free's *Page receiver is always an unsafe-cast &slice[pos] address (via tx.page/db.page) or nil-guarded before the call, never a nilable heap pointer. | internal/freelist/shared.go:57:9 |  |
| C314 | nil-deref | I I.I() != I.I { | 1 | FP/invariant | m comes from LoadPageMeta(buf)=&buf[PageHeaderSize], an address-of a fully-populated slice element that can only be valid or panic-on-index, never nil. | internal/guts_cli/guts_cli.go:109:12 |  |
| C315 | nil-deref | I I.I.I(I), I | 1 | FP/invariant | Bucket.tx is set at every real construction site (newBucket/tx.root/tmpBucket); the rare Bucket{} literals omitting tx never call pageNode. | bucket.go:948:11 |  |
| C316 | nil-deref | I I.I.I(I(I, I []I) I { | 1 | FP/encoding | tx.root is a value field so ForEach's pointer-receiver call is compiler-inserted &tx.root, which can only be nil if tx itself is (already ruled out to reach the method). | tx.go:157:24 |  |
| C317 | nil-deref | I I.I.I() > N { | 1 | FP/encoding | tx.stats is a value field so GetRebalance's call is auto-&tx.stats; tx is already proven non-nil by prior unconditional dereferences earlier in the same Commit call. | tx.go:196:26 |  |
| C318 | nil-deref | I I.I.I() > I { | 1 | FP/invariant | tx.meta and tx.db are always nil/non-nil in lockstep (paired assignments in init/close), so Commit's earlier tx.db==nil guard also rules out tx.meta==nil at the flagged site. | tx.go:230:17 |  |
| C319 | nil-deref | I I.I.I >= I.I.I.I() { | 1 | FP/invariant | tx.meta is only nulled in close(), which Commit calls strictly after spill()/write()/writeMeta() complete, so it can't be nil during any Bucket.spill() call including recursive ones. | bucket.go:792:38 |  |
| C320 | nil-deref | I I.I.I != I { | 1 | FP/encoding | rollback's own tx.db==nil early-return guard already passed, and tx.db is dereferenced again immediately before the flagged line with no intervening reassignment. | tx.go:331:9 |  |
| C321 | nil-deref | I I.I, I = I.I(); I != I { | 1 | FP/encoding | The analyzer's own recorded path trail already dereferences db.file (db.file.Name()) earlier in the same Open() invocation before reaching the flagged getPageSize call. | db.go:277:39 |  |
| C322 | nil-deref | I I.I, I | 1 | FP/encoding | b.page access is reachable only after b.RootPage() (an embedded *InBucket promoted-method call) already dereferenced b earlier in the same function. | bucket.go:937:12 |  |
| C323 | nil-deref | I I.I == N && I != I.I() { | 1 | FP/invariant | m is LoadPageMeta(buf) over a fixed make([]byte,1024) buffer (never nil), and m is already dereferenced via updateMetaField earlier on the same path before the flagged call. | cmd/bbolt/command_surgery_meta.go:168:50 |  |
| C324 | nil-deref | I I.I < I.I()-N { | 1 | FP/invariant | pageNode never returns (nil,nil): rootNode is always materialized before b.page is cleared, an invariant maintained across openBucket/CreateBucket construction paths. | cursor.go:222:30 |  |
| C325 | nil-deref | I I.I != N { | 1 | FP/requires-lifting | free()'s *node receiver is typed nilable in isolation, but all four call sites already dereferenced/proved that same node value non-nil earlier in the caller before invoking free(). | node.go:495:7 |  |
| C326 | nil-deref | I I, I.I | 1 | FP/encoding | b.rootNode access is reachable only after b.RootPage() (embedded *InBucket promoted method) already dereferenced b earlier on the same modeled path. | bucket.go:935:18 |  |
| C327 | nil-deref | I I, I, I.I(S, I.I(), I) | 1 | FP/encoding | The flagged p.Count() in the error message is reached only via the guarding if-condition that already called p.Count() on the same receiver one line prior. | cmd/bbolt/main.go:450:103 |  |
| C328 | nil-deref | I I, I = N, I(I.I) | 1 | FP/encoding | p.count access is preceded on the prior line by unconditional eager-evaluated p.IsFreelistPage()/p.flags dereferences as Assert's arguments, so nil p would already have panicked. | internal/common/page.go:129:28 |  |
| C329 | nil-deref | I I, I := I.I(I(I.I.I*N), I.I); I != I { | 1 | FP/invariant | f is tx.db.openFile's result already guarded by err!=nil early-return; the default os.OpenFile (and its documented pluggable replacement) guarantees err==nil implies non-nil file. | tx.go:427:31 |  |
| C330 | nil-deref | I I, I := I.I.I.I(I, I(I.I())*I(I.I.I)); I != I { | 1 | FP/encoding | pageInBuffer computes &buf[id*pageSize], never nil since buf is make([]byte,pageSize) with pageSize guaranteed non-zero (falls back to DefaultPageSize). | tx.go:564:48 |  |
| C331 | nil-deref | I I, I := I I.I() { | 1 | FP/invariant | Every *shared is produced by newShared(), which unconditionally returns a populated non-nil struct; no code path ever constructs or stores a nil *shared. | internal/freelist/shared.go:209:38 |  |
| C332 | nil-deref | I I >= I(I.I) \|\| !I.I(I.I[I].I(), I) { | 1 | FP/encoding | Go's short-circuiting \|\| guarantees n.inodes[index].Key() is only evaluated when index is already in-bounds, so the pointer-receiver call never hits an out-of-range/nil element. | node.go:150:63 |  |
| C333 | nil-deref | I I >= I.I() { | 1 | FP/encoding | LoadPage's &buf[0] is never nil, and pageBytes is always sized from ReadPage's on-disk pageSize (never 0/empty) so the address-of can't fail into nil. | cmd/bbolt/main.go:449:21 |  |
| C334 | nil-deref | I I >= I.I.I()-N { | 1 | FP/encoding | n.parent is already guarded non-nil two lines earlier in the same function, with only a read-only sort.Search call intervening that cannot mutate n.parent. | node.go:98:34 |  |
| C335 | nil-deref | I I >= I.I.I.I.I() { | 1 | FP/invariant | tx.meta is only nulled inside close(), which runs strictly after every put()-reachable rebalance/spill operation on that transaction has completed. | node.go:118:34 |  |
| C336 | nil-deref | I I == I(I.I()) \|\| I == -N { | 1 | FP/encoding | LoadPage's &buf[0] address-of a slice element is never nil, and every nil-error return of ReadPage is paired with a non-nil LoadPage-derived p. | internal/surgeon/surgeon.go:74:23 |  |
| C337 | nil-deref | I I = I(I.I) - N; I >= N; I-- { | 1 | FP/invariant | sole constructor Bucket.Cursor() always returns &Cursor{...} freshly addressed, never nil, for every call site of next() | cursor.go:220:17 |  |
| C338 | nil-deref | I I = I(I.I) | 1 | FP/invariant | every live Bucket's tx field is non-nil by construction: root from newBucket(tx) with non-nil *Tx receiver, children inherit b.tx via openBucket | bucket.go:116:23 |  |
| C339 | nil-deref | I I = I(I, I); I != I { | 1 | FP/requires-lifting | db.file assigned and error-checked (db.go:234-238) before both mmap call sites (Open and allocate), never nulled while a write tx is active | db.go:492:15 |  |
| C340 | nil-deref | I I = I(I, !I.I, I.I); I != I { | 1 | FP/requires-lifting | db.file already proven non-nil by a successful dereference earlier in the same function (db.path = db.file.Name() at db.go:239) before flock is called | db.go:248:16 |  |
| C341 | nil-deref | I I = I([]I, I.I+I.I()) | 1 | FP/requires-lifting | all three call sites of write() establish rootNode non-nil first, either via the same composite literal or via inlineable()'s own n==nil guard | bucket.go:835:57 |  |
| C342 | nil-deref | I I = I((I.I()+I.I(I))+N) * I.I | 1 | FP/requires-lifting | the identical p is already dereferenced successfully via p.Id() a few lines earlier in the same function, on the same never-reassigned pointer | db.go:1166:23 |  |
| C343 | nil-deref | I I = I.I[N].I | 1 | FP/invariant | node() is unexported and only reachable via the sole Cursor constructor Bucket.Cursor(), which never yields nil | cursor.go:399:12 |  |
| C344 | nil-deref | I I = I.I(I) | 1 | FP/invariant | write-side invariant across bucket.go: every value tagged BucketLeafFlag is written by Bucket.write() with length >= BucketHeaderSize, so openBucket's unsafe indexing is always in-bounds | bucket.go:105:26 |  |
| C345 | nil-deref | I I = I.I(); I != I { | 1 | FP/requires-lifting | db.file already proven non-nil by an earlier successful dereference in the same function (db.path = db.file.Name()) before db.init() is called | db.go:269:19 |  |
| C346 | nil-deref | I I = I.I() + I | 1 | FP/encoding | p is already dereferenced via p.Id() three lines earlier in the same function without a flagged finding there, exposing an internal inconsistency in the tool's own nil model | tx_check.go:157:16 |  |
| C347 | nil-deref | I I = I.I.I(); I != I { | 1 | FP/requires-lifting | the caller's tx.db==nil guard transitively guarantees tx.meta non-nil since both fields are set together in Tx.init and cleared together in Tx.close | tx.go:204:24 |  |
| C348 | nil-deref | I I < I(I.I())-N { | 1 | FP/invariant | tx.page() cannot return nil (sole map write site never stores nil, mmap fallback builds a pointer via address-of-element), and p is already dereferenced earlier in the same function without a guard | tx_check.go:200:35 |  |
| C349 | nil-deref | I I := I.I[I].I(); I != I { | 1 | FP/invariant | every append site into a node's children slice appends only freshly-constructed or cache-retrieved non-nil *node values, so indexing children can never hit nil | node.go:306:32 |  |
| C350 | nil-deref | I I := I.I[I(I)]; I != I { | 1 | FP/encoding | the flagged line is a map index followed by a nil comparison, not a pointer dereference; Go map lookups on a missing key never panic | bucket.go:90:17 |  |
| C351 | nil-deref | I I := I.I(N); I <= I.I(I.I()); I++ { | 1 | FP/encoding | the pointer is built via unsafe.Pointer(&db.data[pos]), an address-of-in-bounds-slice-element construction that in real Go semantics panics on out-of-range pos rather than ever yielding nil | tx_check.go:156:54 |  |
| C352 | nil-deref | I I := I.I(I); I == I.I { | 1 | FP/invariant | fs is constructed one line above by flag.NewFlagSet, which the stdlib always implements to return a non-nil *FlagSet literal | cmd/bbolt/main.go:1692:20 |  |
| C353 | nil-deref | I I := I.I(I(I, I), I(I, I)); I != I { | 1 | FP/requires-lifting | CreateBucketIfNotExists returns nil only when the bucket name argument is empty, and every call site in the tree passes a non-empty literal bucket name | internal/btesting/btesting.go:168:20 |  |
| C354 | nil-deref | I I := I.I(I(I, I.I()), I); I != I { | 1 | FP/encoding | bpe is computed via nonzero-offset unsafe pointer arithmetic off a non-nil base; only integer wraparound to exactly 0 could make it nil, which never occurs for real on-disk pages | internal/surgeon/xray.go:44:48 |  |
| C355 | nil-deref | I I := I.I(I(I, I []I) I { | 1 | TP | tx.Bucket() returns nil when the bench bucket was never created, reachable via bbolt bench -count=0 with a nested write mode so the write-phase loop never runs before ForEach dereferences the nil receiver | cmd/bbolt/main.go:1402:25 |  |
| C356 | nil-deref | I I := I.I(I.I[N:]...); I == I { | 1 | FP/invariant | m is constructed immediately above by NewMain(), which always returns a non-nil &Main{} literal | cmd/bbolt/main.go:66:17 |  |
| C357 | nil-deref | I I := I.I(I.I()); I != I { | 1 | FP/encoding | the same elem receiver was already dereferenced two lines earlier via IsBucketEntry() (a plain flags field read) to even enter this branch, so it cannot be nil at the flagged Key() call | tx_check.go:113:42 |  |
| C358 | nil-deref | I I := I.I(I, I, I.I); I != I { | 1 | FP/requires-lifting | bolt.Open's cross-function postcondition guarantees non-nil *DB whenever err==nil, and both src/dst are guarded by err!=nil checks immediately after Open, even though no local dereference proves it inside compactCommand.Run | cmd/bbolt/main.go:1731:24 |  |
| C359 | nil-deref | I I := I.I(); I == I { | 1 | FP/encoding | metaA aliases whichever of db.meta0/db.meta1 was already dereferenced via Txid() in the preceding comparison, so it cannot be nil by the time Validate() is called | db.go:1135:26 |  |
| C360 | nil-deref | I I := I.I.I(I(I)); I != I { | 1 | FP/invariant | a lock-protected lifecycle invariant (rwlock held for the full write-tx duration) guarantees db.file stays non-nil during any commit that reaches grow(), since Close() must acquire the same lock to null it | db.go:1207:30 |  |
| C361 | nil-deref | I I := I.I.I.I(); I != I { | 1 | FP/encoding | Logger() nil-safe by construction — checks db==nil before touching fields, so pre-guard call site can't nil-deref. | bucket.go:274:13 |  |
| C362 | nil-deref | I I := I I.I { | 1 | FP/encoding | range over nil map is a documented no-op in Go, not a dereference; forwardMap also always initialized non-nil in Init(). | internal/freelist/hashmap.go:126:19 |  |
| C363 | nil-deref | I I := &I.I[I(I.I)-N]; I.I >= I.I() { | 1 | FP/encoding | &c.stack[len-1] is address-of-slice-element, never nil, and seek/search always append an elemRef before returning. | cursor.go:126:60 |  |
| C364 | nil-deref | I = I.I[N].I() | 1 | FP/encoding | pointer-receiver call on inodes[0] is Go's implicit &slice[i], which is never nil since Inode is a value type. | node.go:341:29 |  |
| C365 | nil-deref | I = I.I(I(I + N)).I() | 1 | FP/requires-lifting | tx.page/db.page never return nil (panic first on OOB), so BranchPageElement's in-bounds pointer arithmetic can't be nil. | tx_check.go:201:52 |  |
| C366 | nil-deref | I = I.I.I(I) | 1 | FP/invariant | every Bucket is constructed via newBucket(tx) threading a non-nil tx, so b.tx==nil is unreachable at (*Bucket).node. | bucket.go:878:9 |  |
| C367 | nil-deref | I = I.I.I().([]I) | 1 | FP/invariant | db.pagePool.New is set unconditionally in Open() and never reset, so sync.Pool.Get()'s type assertion can't hit an untyped nil; tx.db already dereferenced one line earlier. | db.go:1151:24 |  |
| C368 | nil-deref | I <- I.I(S, I(I.I()), I(I), I) | 1 | FP/invariant | p comes from tx.page via forEachPage, backed by mmap pointer arithmetic that never yields nil, and FastCheck asserts validity on every return path. | tx_check.go:152:70 |  |
| C369 | nil-deref | I += I.I * I(I.I()-N) | 1 | FP/invariant | p supplied by tx.forEachPage is either a tx.pages entry populated only on allocate success or db.page's address-of-slice-element, never nil. | bucket.go:630:57 |  |
| C370 | nil-deref | I (I.I() & I.I) != N { | 1 | FP/invariant | same forEachPage-supplied p is never nil, and LeafPageElement is pure pointer arithmetic (UnsafeIndex) on that non-nil base. | bucket.go:655:17 |  |
| C371 | nil-deref | I !I.I && !I.I() { | 1 | FP/requires-lifting | db.mmap() only returns nil error after assigning both meta0/meta1 non-nil; every earlier/later failure path returns non-nil err and short-circuits Open() before line 308. | db.go:308:48 |  |
| C372 | nil-deref | I := I(I.I) > N && I < I(I.I) && I.I(I.I[I].I(), I) | 1 | FP/encoding | index is guard-bounded (index < len(n.inodes)) before use, and n.inodes[index] is an addressable value-slice element, never a nilable pointer. | node.go:130:88 |  |
| C373 | nil-deref | I := I(I.I()) | 1 | FP/requires-lifting | guts_cli.ReadPage's error branches all return nil p with non-nil err, checked before use; p already dereferenced one statement earlier without issue. | internal/surgeon/surgeon.go:47:27 |  |
| C374 | nil-deref | I := I(I.I, N, I.I()) | 1 | FP/requires-lifting | Copyall's receiver t is always non-nil since loadFreelist unconditionally assigns a concrete freelist.Interface before any caller reaches Copyall/Write. | internal/freelist/shared.go:208:43 |  |
| C375 | nil-deref | I := I(I, I(I.I)) | 1 | FP/requires-lifting | ReadMetaPageAt's only success return pairs a non-nil LoadPageMeta cast with nil err, checked by the caller before m is used. | cmd/bbolt/command_surgery_meta.go:161:29 |  |
| C376 | nil-deref | I := I(I, I(I.I())) | 1 | FP/requires-lifting | every real caller of ReadInodeFromPage supplies p from tx.page/db.page (never nil) or guts_cli.ReadPage guarded by err check, so the callee's precondition is always discharged by callers. | internal/common/inode.go:50:36 |  |
| C377 | nil-deref | I := I(I, I.I()) | 1 | FP/encoding | item is a for-range value variable, not a caller-supplied pointer; &item address-of-local is never nil regardless of slice contents. | internal/common/inode.go:100:24 |  |
| C378 | nil-deref | I := I([]I.I, N, I(I.I)) | 1 | FP/invariant | hashMap is only ever constructed via NewHashMapFreelist which always builds a non-nil struct; f already dereferenced earlier in the same function. | internal/freelist/hashmap.go:125:47 |  |
| C379 | nil-deref | I := I([]I.I, I.I.I.I()) | 1 | FP/requires-lifting | loadFreelist unconditionally assigns a non-nil db.freelist just before the same-function Count() call, with no intervening call that could nil it. | tx_check.go:44:32 |  |
| C380 | nil-deref | I := I([]I, I.I.I) | 1 | TP | WriteTo dereferences tx.db with no tx.db==nil guard, unlike every sibling Tx/Bucket/Cursor method, so calling it after Commit/Rollback (which nils tx.db) panics instead of returning ErrTxClosed. | tx.go:402:25 |  |
| C381 | nil-deref | I := I.I(I[I].I(), I) | 1 | FP/encoding | inodes[i] address-of-slice-element receiver is never nil, and sort.Search's closure is only invoked with i in-bounds of the slice it was built from. | cursor.go:332:37 |  |
| C382 | nil-deref | I := I.I(I).I() | 1 | TP | bench -count=0 skips write loop entirely so the bucket is never created, and (*Bucket).Cursor() dereferences the nil Bucket returned by tx.Bucket() with no nil-check. | cmd/bbolt/main.go:1327:43 |  |
| C383 | nil-deref | I := I.I(I) | 1 | FP/requires-lifting | childBucket's fresh internal seek deterministically re-finds the same bucket-flagged entry the outer cursor just observed on the unmodified receiver b, so Bucket(k) can't return nil here. | bucket.go:418:45 |  |
| C384 | nil-deref | I := I.I(I(I.I), I(I I) I { I I.I(I.I[I].I(), I.I) != -N }) | 1 | FP/encoding | n.inodes[i] address-of-slice-element receiver is never nil, and sort.Search only invokes its closure with i within the slice's valid index range. | node.go:83:93 |  |
| C385 | nil-deref | I := I.I(I.I[I].I(), I) | 1 | FP/encoding | pointer-receiver method on addressable slice element (n.inodes[i]) can never have a nil receiver; invalid index panics first instead | cursor.go:309:39 |  |
| C386 | nil-deref | I := I.I.I(I(I *I) I { | 1 | FP/invariant | batch.db is only ever set to the enclosing (*DB).Batch receiver, which is already proven non-nil by an earlier Mutex.Lock() on that same receiver | db.go:1023:21 |  |
| C387 | nil-deref | I := I.I.I(I.I[N].I(), I) | 1 | FP/encoding | receiver &n.inodes[0] guarded by len(n.inodes)==1 check immediately prior, so address-of-slice-element is never nil | node.go:385:43 |  |
| C388 | nil-deref | I := I.I.I(I.I(I)) | 1 | FP/encoding | tx.db==nil early-return guard four lines above in the same function proves tx.db non-nil at the flagged call with no intervening reassignment | tx.go:635:17 |  |
| C389 | nil-deref | I := I.I.I(I, N) | 1 | FP/requires-lifting | sole caller Commit checks tx.db==nil before falling through to the only call site of writeMeta, a fact not lifted across the call boundary | tx.go:560:25 |  |
| C390 | nil-deref | I := I.I.I.I() | 1 | FP/encoding | identical db.rwtx.meta.Pgid() expression already dereferenced a few lines earlier in the same function with no intervening reassignment of db.rwtx/meta | db.go:1174:30 |  |
| C391 | nil-deref | I := I.I + (I.I * I(I.I()-N)) | 1 | FP/encoding | same closure parameter p already dereferenced via p.IsLeafPage()/IsBranchPage()/Count() earlier in the same invocation, so nil p would already have faulted | bucket.go:668:83 |  |
| C392 | nil-deref | I := &I.I[I] | 1 | FP/invariant | every elemRef pushed onto c.stack comes from Bucket.pageNode(), which invariantly returns a non-nil page or node for any non-corrupted db file | cursor.go:221:15 |  |
| C393 | nil-deref | *I.I() = *I.I | 1 | FP/invariant | tx.db and tx.meta are always set/cleared together (paired-nil invariant) and an earlier tx.db field read at line 391 already proves tx.db non-nil | tx.go:405:21 |  |
| C394 | nil-deref | *I.I.I = *(I.I.I()) | 1 | FP/encoding | receiver tx.meta is the address of a freshly allocated composite literal (&common.Meta{}) two statements earlier, provably non-nil, not a nullable pointer | tx.go:58:42 |  |
| C395 | nil-deref | } I I I.I(I) >= I.I.I() { | 1 | FP/invariant | paired tx.db/tx.meta set-and-cleared-together invariant plus the immediately preceding tx.db==nil early return guarantees tx.meta non-nil | tx.go:626:43 |  |
| C396 | nil-deref | } I I I := I.I(); I == I { | 1 | FP/invariant | metaB is only nil before Open()/after Close(), and every meta() call runs under a lock (mmaplock) that mutually excludes the concurrent invalidate() that nils it | db.go:1137:33 |  |
| C397 | nil-deref | } I I !I.I() { | 1 | FP/encoding | receiver b already dereferenced via b.tx.db.Logger() in the same function's preceding if-statement init clause, so nil b would already have panicked | bucket.go:287:23 |  |
| C398 | nil-deref | } I I !I.I.I() { | 1 | FP/encoding | c.bucket already dereferenced in the preceding if-branch of the same function (c.bucket.tx.db==nil), and Cursor.bucket is set only at one construction site that itself dereferences b.tx first | cursor.go:143:30 |  |
| C399 | bounds | I I[N] { | 1 | FP/encoding | same short-circuit-\|\|-encoding defect as C400 propagated one guard downstream: reaching the switch already requires len(args)!=0 | cmd/bbolt/main.go:119:13 |  |
| C400 | bounds | I I(I) == N \|\| I.I(I[N], S) { | 1 | FP/encoding | Go's \|\| short-circuits so args[0] is only evaluated when len(args)==0 is false, i.e. len(args)>=1; analyzer's encoding of short-circuit evaluation is unsound here | cmd/bbolt/main.go:113:45 |  |
| C401 | bounds | I I(I.I(I), N, I(I.I), I(I.I)+I(I.I)) | 1 | FP/encoding | Go evaluates all call arguments left-to-right before the call proceeds, so a nil n would already panic reading n.pos/n.ksize in sibling arguments before UnsafeByteSlice's body runs with base=nil | internal/common/page.go:245:24 |  |
| C402 | bounds | I I(I, I[N], I) | 1 | FP/requires-lifting | cobra's Args validator (ExactArgs(1)) runs before RunE is invoked, so args[0] in the closure is unreachable unless len(args)==1, a fact invisible across the external cobra dependency boundary | cmd/bbolt/command_check.go:28:30 |  |
| C403 | bounds | I I *I.I = I.I([]I(I[N])) | 1 | FP/requires-lifting | both callers of findLastBucket guard len(buckets)==0 with an early return immediately before calling it, a fact not lifted into the callee's bucketNames[0] use | cmd/bbolt/main.go:1784:60 |  |
| C404 | bounds | I := I[:N] | 1 | FP/encoding | dst[:0] is a constant-zero-upper-bound slice re-expression that is unconditionally in-bounds per the Go spec for any slice including nil, regardless of dst's length/capacity | internal/common/page.go:368:15 |  |
| C405 | bounds | I := I(I.I(I), N, N, I) | 1 | FP/requires-lifting | unexported hexdump has exactly one caller (a test) passing the literal constant 16, matching PageHeaderSize exactly, so the unconstrained-parameter model overapproximates a path no caller produces | internal/common/page.go:159:24 |  |
| C406 | bounds | I := I(I.I(I), I, N, I) | 1 | FP/requires-lifting | the sole production writer path (node.spill->tx.allocate sized by node.size()) guarantees the page buffer is at least as large as the same key/value sum WriteInodeToPage's off accumulates, invisible to local analysis of UnsafeByteSlice | internal/common/inode.go:81:23 |  |
| C407 | bounds | I := I(I.I(&I[N]))&I != N | 1 | FP/invariant | every write-path constructor of a BucketLeafFlag-tagged value builds it from an InBucket struct (16 bytes), so value is always >= BucketHeaderSize at read time absent on-disk tampering outside bbolt's own write path | bucket.go:123:44 |  |
| C408 | bounds | I := I.I(I[N]) | 1 | FP/encoding | the reported violating path skips the dominating if nk==0 {return nil} guard block entirely, dropping the same-function branch condition that proves len(keys)>=1 before keys[0] | compact.go:53:22 |  |

## Totals

Computed from `docs/shakeout-phase4-bbolt-findings.tsv` (`tail -n +2 ... | cut -f10 | sort | uniq -c`; see Step-3 verification below):

- findings: 1006
- TP: 33
- FP/requires-lifting: 124
- FP/invariant: 410
- FP/encoding: 434
- mixed (class-heterogeneous, C015b only): 5
- unclear: 0

- headline FP rate: confirmed-FP rows (verdict starts `FP/`) / N =
  (124 + 410 + 434) / 1006 = 968 / 1006 ≈ **96.2%**. The 5 `mixed`
  rows (all `C015b`) count as neither FP nor TP in this figure.
- estimated FP rate: `C015b`'s sampled ratio is 0 TP : 5 FP (2
  FP/requires-lifting : 3 FP/invariant — see `verdicts/C015.md`
  refinement round), so all 5 fold in as FP: (968 + 5) / 1006 = 973 /
  1006 ≈ **96.7% (estimate)**.
- TP rate: 33 / 1006 ≈ 3.3%.
- wall clock: cold 372.47 s / warm 29.74 s (Run parameters above).
- capture determinism: cold and warm capture files are byte-identical
  (`cmp capture-cold.txt capture-warm.txt`); the only difference
  between the cold/warm logs is the one-time release-binary compile
  step captured in the cold run's build log, not the analysis output.

### True positives (33 findings, 20 classes)

Real bbolt bugs at API boundaries, dominated by nil-receiver/nil-param
on exported functions reachable without any prior guard. Recorded here
only — upstreaming to etcd-io/bbolt is a separate user decision (design
§3.4), not part of this task.

- **C031c** (1 finding: compact.go:11:22): exported Compact's dst *DB parameter is never nil-checked, and beginRWTx dereferences db.readOnly with no guard — a genuinely reachable nil-pointer panic.
- **C036a** (2 findings: bucket.go:63:19, bucket.go:539:21): Sequence()/Root() getters omit the closed-tx guard their sibling setters (SetSequence) have, and Tx.Cursor() hands out a &tx.root alias that tx.close() nils in place — reproduced live as a real panic.
- **C037a** (1 finding: logger.go:64:7): Options.Logger's nil check only rejects a nil interface, not a non-nil interface wrapping a nil *DefaultLogger, so a caller-supplied typed-nil logger reaches a real nil-receiver panic in DefaultLogger.Debugf.
- **C049b** (2 findings: cursor.go:46:30, cursor.go:46:45): recursivelyInspect calls unexported first() with no tx-closed guard (unlike First()/ForEachBucket); tx.Inspect() after Commit/Rollback reaches nil InBucket, confirmed by repro panic.
- **C063** (3 findings: cmd/bbolt/main.go:1236:30, cmd/bbolt/main.go:1251:31, cmd/bbolt/main.go:1199:31): options.KeySize is an unbounded CLI flag (default 8, no minimum check); binary.BigEndian.PutUint32 requires len>=4, so --key-size<4 reaches make([]byte,KeySize) then panics with index out of range.
- **C083** (3 findings: cmd/bbolt/main.go:1035:28 x3): benchCommand.Run discards the error from CreateBucketIfNotExists (`b, _ := ...`) then unconditionally writes b.FillPercent, panicking when a prior -work run left an incompatible non-bucket value at the key.
- **C084** (3 findings: tx.go:391:15, tx.go:391:30, tx.go:391:54): WriteTo/Copy/CopyFile dereference tx.db/tx unguarded, unlike every other Tx/Bucket accessor that checks for ErrTxClosed, so calling WriteTo on an already-closed or nil Tx panics.
- **C097** (3 findings: logger.go:65:7, logger.go:82:6, logger.go:74:6): DefaultLogger embeds *log.Logger with no exported constructor; zero-value struct passed via Options.Logger reaches Output() on the nil embedded pointer in Debugf/Infof/Errorf.
- **C117** (2 findings: logger.go:50:12, logger.go:50:20): EnableTimestamps calls SetFlags/Flags on the same unguarded embedded *log.Logger as C097, reachable via a zero-value DefaultLogger since no exported constructor enforces initialization.
- **C173b** (1 finding: bucket.go:599:7): ForEachBucket's first statement dereferences b with no prior guard; Bucket() can return nil, reachable via external API chaining.
- **C213** (2 findings: tx.go:594:10, tx.go:594:17): tx.page() dereferences tx.db without a nil check, and tx.db is genuinely nilled by close(); unguarded read-path methods (Bucket.Get, Cursor.First/Next/Last) reuse a post-close Tx and reach this real nil-deref.
- **C219b** (1 finding: internal/common/utils.go:15:36): guts_cli.ReadPageAndHWMSize trusts on-disk pageSize after only a magic-number check (no Meta.Validate()), so a corrupted file with pageSize==0 makes LoadPage's &buf[0] panic, reachable via cmd/bbolt page/page-item/surgery commands.
- **C220** (2 findings: internal/common/bucket.go:49:34, internal/common/utils.go:19:36): InlinePage's v[BucketHeaderSize] and LoadPageMeta's buf[PageHeaderSize] have no length checks and are reachable from surgeon/guts_cli's explicitly-corrupted-file-handling paths that never validate on-disk vsize/pageSize.
- **C222** (1 finding: logger.go:50:12): DefaultLogger has no constructor guarding its embedded *log.Logger, so a zero-value &DefaultLogger{} leaves it nil and EnableTimestamps/Debug/Panic nil-deref on the embedded logger.
- **C228** (1 finding: tx_check.go:152:78): branch-element pgid and hwm are raw uint64 fields read off disk with only an id==id equality check (FastCheck), so a crafted 0x8000... pgid wraps negative in int(p.Id()) inside Check()'s corruption-diagnostic message.
- **C294** (1 finding: bucket.go:434:25): Bucket(name) documented to return nil for a missing bucket, and Cursor()/Get() dereference that receiver with no nil guard.
- **C304** (1 finding: tx.go:82:27): tx.close() nils tx.meta/tx.db but Size() omits the closed-tx guard present in its sibling ID() and in every Bucket/Cursor method.
- **C355** (1 finding: cmd/bbolt/main.go:1402:25): tx.Bucket() returns nil when the bench bucket was never created, reachable via bbolt bench -count=0 with a nested write mode so the write-phase loop never runs before ForEach dereferences the nil receiver.
- **C380** (1 finding: tx.go:402:25): WriteTo dereferences tx.db with no tx.db==nil guard, unlike every sibling Tx/Bucket/Cursor method, so calling it after Commit/Rollback (which nils tx.db) panics instead of returning ErrTxClosed.
- **C382** (1 finding: cmd/bbolt/main.go:1327:43): bench -count=0 skips write loop entirely so the bucket is never created, and (*Bucket).Cursor() dereferences the nil Bucket returned by tx.Bucket() with no nil-check.

## Dispatch-precision + phase-5 observations

- **Dispatch precision** (carried Task-10 watch item, spec §16): across
  all 459 distilled class verdict entries, **none observed** — zero
  classes evidenced a trace routing through a shared-signature
  over-approximated invoke edge. Basis: all 459 class verdict entries
  (across the 409 `verdicts/C*.md` files; subclasses share their parent
  class's file) were reviewed and every cited `path:` trail grounds
  through static call edges; the per-class dispatch-precision flag
  distilled into `work/reasons.tsv` column 5 is 0 for all 459 rows.

- **requires-lifting (phase-5 input)**: 124 findings across 78 classes
  (all confirmed FP/requires-lifting, no unresolved candidates).

  Canonical example — **C009c**, `compact.go:26:23`
  (`nil-deref: call to (*go.etcd.io/bbolt.Tx).Commit violates its
  nil-deref requirement [go.etcd.io/bbolt.Compact$2]`):

  ```
  compact.go:26:23: nil-deref: call to (*go.etcd.io/bbolt.Tx).Commit violates its nil-deref requirement [go.etcd.io/bbolt.Compact$2]
     26 |    if err := tx.Commit(); err != nil {
        |                       ^
      path: compact.go:23 -> compact.go:24 -> compact.go:26 -> compact.go:27
      with: p0 = (seq-val #x0000000000000002 #x0000000000000002), p1 = (seq-val #x8001000000800800 #xa000000000000000), p2 = (seq-val #x8820002000000001 #xc000000000000000)
  ```

  `tx` is bound by `tx, err := dst.Begin(true)` (compact.go:10),
  immediately guarded by `if err != nil { return err }`
  (compact.go:11-13) before the `walk` closure containing the flagged
  `tx.Commit()` is entered; `tx` is reassigned only via
  `tx, err = dst.Begin(true)` (compact.go:29-32), again immediately
  guarded. `DB.Begin` unconditionally dispatches to
  `beginRWTx`/`beginTx`, and both satisfy `err == nil ⇒ result != nil`
  (every error path returns `(nil, err)`; the sole success path
  allocates `t := &Tx{...}` and returns `(t, nil)`). The analyzer never
  lifts this postcondition of `Begin`/`beginRWTx`/`beginTx` through the
  caller's `err != nil` guards, so `tx` is modeled as possibly nil at
  the `Commit()` call even though every reachable path to it dominates
  on `err == nil`. Full trace and evidence citations: `verdicts/C009.md`.

  Distilled PHASE5-NOTE payload for the canonical example: a
  substitution-based lifting pass must summarize `DB.Begin` (and the
  `beginRWTx`/`beginTx` helpers it dispatches to) with the postcondition
  `err == nil ⇒ result != nil`, then substitute that summary at both
  call sites in `Compact` — the initial `tx, err := dst.Begin(true)`
  (compact.go:10) and the in-loop reassignment
  (compact.go:29-32) — narrowing `tx` to non-nil on the branch that
  dominates every reachable use of `tx.Commit()` (compact.go:26) and the
  deferred `tx.Rollback()`.

  All 78 classes' distilled PHASE5-NOTE payloads (exactly what a
  substitution-based lifting pass must carry through call sites to kill
  each class), in descending-count order:

- **C004b** (3 findings): carry 'e.page != nil' proven by search()'s e.isLeaf() call (cursor.go:292) into searchPage/nsearch at their single call sites (cursor.go:301/293); carry 'p != nil' from p.Count() one statement earlier into p.IsLeafPage() within ReadInodeFromPage (inode.go:50->51)
- **C008c** (2 findings): carry 'err==nil => meta!=nil' for readMetaPage through both guard sites before meta.IsFreelistPersisted(): command_surgery_freelist.go:87-91 and command_surgery.go:141-145
- **C009c** (2 findings): carry 'err==nil => tx!=nil' for DB.Begin through both call sites in Compact (compact.go:10 and :29-32) to tx.Commit() at compact.go:26; carry 'err==nil => m!=nil' for ReadMetaPageAt through command_surgery_meta.go:54 guard to m.Validate()/m.PageSize()
- **C017c** (2 findings): carry results=&writeResults (declared main.go:1030) non-nil through runWrites -> runWritesSequential/RandomNested -> runWritesWithSource/NestedWithSource forwarding chains to discharge AddCompletedOps at main.go:1208 and :1260
- **C024c** (4 findings): carry Tx.page/DB.page's never-nil postcondition through Bucket.node's p==nil branch structure (bucket.go:876-888) to the single n.read(p) call site, substituting into all four dereferences in node.go:163-164
- **C027** (7 findings): lift cobra ExactArgs(1)'s len(args)==1 postcondition through (*Command).execute's dispatch into each RunE closure's args[0] index at its five call sites.
- **C029a** (3 findings): carry p!=nil from tx.allocate's success path (via node.write's p param) and from guts_cli.ReadPage's success path (via ClearPageElements's p param) into WriteInodeToPage's p parameter and onward through LeafPageElement/BranchPageElement.
- **C031a** (4 findings): summarize CreateBucketIfNotExists/Begin's err==nil⇒result!=nil postcondition and substitute it at the guarding call sites (main.go:1228, compact.go:11/31, db.go 'Open' construction), and carry same-function prior-dereference non-nil facts across straight-line code (Open db.go:178-309, spill node.go:296-315).
- **C036c** (1 finding): model common.Assert as a path-killing precondition check in Commit/Rollback and thread tx.managed==true through the fn(t) call in DB.Update to prove tx.db is unchanged at db.go:915's t.Commit() call.
- **C039a** (1 finding): derive inlineable()'s true⇒rootNode!=nil postcondition, recognize free() doesn't mutate rootNode, and substitute that fact for the same child receiver at the write() call (bucket.go:749-751).
- **C042** (4 findings): substitute n!=nil at searchNode's sole call site (cursor.go:298), and combine pageNode's never-both-nil postcondition with the n==nil else-branch fact to derive p!=nil at searchPage's sole call site (cursor.go:301).
- **C050** (4 findings): carry &readResults non-nil fact through runReads's results param across call sites 1290/1292/1297/1299 into each runReadsXxx before its AddCompletedOps deref at 1325/1363/1403/1439
- **C051b** (2 findings): at page_command.go:168 substitute enclosing IsBucketEntry() guard into leafPageElement.Bucket()'s internal branch; at bucket.go:377 carry forward that RootPage() already dereferenced InBucket without panicking earlier on the same path
- **C053b** (3 findings): propagate caller-established tx-non-nil (Rollback's prior tx.managed/tx.db reads for nonPhysicalRollback; Commit/commitFreelist's prior tx.* accesses or db.Begin's non-nil result for rollback; each guard's false-branch proof for close) across the respective call edges
- **C054b** (2 findings): lift the 'if db.DB != nil' guard fact from btesting.go:82 across the sole call at line 85 into PrintStats's entry state for receiver db and embedded db.DB
- **C072a** (1 finding): substitute newPageCommand's &pageCommand{} literal non-nilness through main.go:140's sole call site into Run's cmd receiver, discharging the nil-deref precondition on cmd.printAllPages at line 60
- **C073a** (1 finding): Lift 'cobra.Command.Flags() never nil' postcondition from call sites (command_surgery.go:65,124,188,255; meta.go:133; freelist.go:39,74) through to AddFlags's fs parameter.
- **C100** (3 findings): Carry 'no Close/MustClose precedes this call on the same *btesting.DB in the test's control flow' from each grepped call site into MustCheck/Fill/CopyTempFile's summaries, mirroring PostTestCleanup's explicit db.DB!=nil guard.
- **C101** (2 findings): Carry the uint16 domain of index (int(index) in [0,65535]) through the int() conversion and combine with the compile-time-constant elemsz to discharge UnsafeIndex's uintptr overflow precondition at LeafPageElement/BranchPageElement call sites.
- **C104a** (1 finding): Carry 'err==nil => p!=nil' from DB.allocate through Tx.allocate's pass-through success path and bind it to p at node.go:324's call site so node.spill's use at node.go:331 inherits non-nilness.
- **C107** (2 findings): Follow both FastCheck call sites through Tx.page back to Tx.allocate/DB.allocate and DB.page, recognize p as address-of-slice-element (never nil), and substitute that fact for FastCheck's receiver.
- **C120** (2 findings): Summarize ReadPage as 'err==nil => ret0!=nil' and substitute it at the page_command.go:102 call site so both later uses of p (lines 116, 119) inherit non-nilness through the err!=nil guard.
- **C133** (2 findings): Lift caller guard tx.db!=nil (tx.go:346-348) and the non-nil tx receiver from call site tx.go:368 into removeTx's entry state so &tx.stats at db.go:880 is known non-nil.
- **C143b** (1 finding): Attach postcondition err==nil => result!=nil to db.openFile/Options.OpenFile (defaults to os.OpenFile) and carry it from the call site db.go:234 through to the dereference at db.go:239.
- **C150a** (1 finding): Carry Commit's tx.db!=nil fact (tx.go:185-186) through tx.root.spill()->Bucket.spill(bucket.go:786)->(*node).spill(node.go:295) down to the tx.allocate call at node.go:324.
- **C152** (2 findings): Carry Commit's tx.db==nil early-return fact (tx.go:185-186) forward through tx.root.spill()->Bucket.spill->(*node).spill->tx.allocate(tx.go:461), and separately through tx.commitFreelist->tx.allocate(tx.go:288).
- **C160a** (1 finding): Carry findLastBucket's postcondition (err==nil ⟹ result!=nil, main.go:1783-1795) past the `if err != nil { return err }` check at main.go:865-868 so lastBucket!=nil is established before the ForEach call at main.go:878.
- **C162** (2 findings): Carry r!=nil through runWrites's call into runWritesRandom/runWritesRandomNested (main.go:1144/1148), then through cmd.Run's call to runWrites with r=rand.New(rand.NewSource(...)) (main.go:1029/1035), resolving that neither constructor returns nil.
- **C179** (2 findings): Carry tx.db!=nil from Commit's line-185 guard through straight-line code to tx.db.grow() and tx.meta.Pgid() at line 235.
- **C181** (2 findings): Carry len(c.stack)>=1 from first()'s post-append and next()'s i==-1 guard + reslice into the callee's precondition.
- **C183a** (1 finding): Carry p!=nil into ReadInodeFromPage from node.go:165 (tx.page invariant) and surgeon.go:75/83 (guts_cli.ReadPage postcondition).
- **C185** (2 findings): Carry Commit's line-184 tx.db!=nil guard through its two single call sites into write()/writeMeta() before fdatasync(tx.db).
- **C192** (2 findings): Carry tx.page/db.page's never-nil postcondition through BranchPageElement's non-nil-base+offset contract to elem uses at line 203.
- **C193** (2 findings): Carry ReadPage's postcondition (err==nil => *Page != nil, from LoadPage) across the err1 guard in ClearPageElements to discharge the UsedSpaceInPage(surgeon.go:81) and WriteInodeToPage(surgeon.go:87) call sites.
- **C200** (2 findings): Carry ReadPage()'s multi-return postcondition (err==nil => p != nil, terminating in common.LoadPage) through the surgeon.go:38 guard to discharge p.IsLeafPage()/IsBranchPage() at surgeon.go:43.
- **C201** (2 findings): Propagate db.mmap()'s postcondition (db.data, meta0, meta1 all non-nil while opened, cleared only together by invalidate()) through Open()/tx_check.go:40 -> loadFreelist() -> db.page()/db.meta() -> freelist.Read(p).
- **C210** (2 findings): Prove p!=nil at Stats$1's two invocation sites (bucket.go:699's guarded b.page, and forEachPageInternal's tx.page-derived p) and substitute that fact into the closure's own parameter binding.
- **C216** (2 findings): Model cobra's Args:ExactArgs(N) as establishing len(args)==N on Execute()'s success path, substituting that fact into the RunE closures reading args[0] in command_inspect.go and command_surgery_meta.go.
- **C218a** (1 finding): Lift the len(buckets)==0 guards at main.go:851-853/945-947 into findLastBucket's bucketNames param at call sites main.go:872/960 to cover the bucketNames[1:] use at main.go:1788.
- **C223** (1 finding): Carry len(key)<=32768 and len(value)<=2^31-2 from Bucket.Put (bucket.go:469-472) forward through node construction into node.inodes so splitIndex's elsize sum at node.go:278 (reached via node.go:246) is provably bounded.
- **C224** (1 finding): Carry MaxKeySize/MaxValueSize enforced at Bucket.Put through node.size()/sizeLessThan() and splitTwo's threshold stopping rule to bound the division at node.go:324 and db.allocate's count*pageSize product (db.go:1153).
- **C225** (1 finding): Carry that freelist-tracked ids derive from real allocated pages bounded by tx.meta.Pgid()/db.datasz through EstimatedWritePageSize's 8*n term and the tx.go:288 division into db.allocate's count*pageSize product.
- **C229** (1 finding): Carry FreelistPageCount's idx-in-{0,1} return-range through the local binding at page.go:145 into UnsafeIndex's n parameter at the page.go:151 call site before checking uintptr(n)*elemsz overflow.
- **C233** (1 finding): Carry the p!=nil fact established by the dominating dereferences at shared.go:57/67 across the closure-creation call at shared.go:68 into Free$1's captured free variable p, eliminating the nil-deref hypothesis at shared.go:70.
- **C242** (1 finding): Carry 'tx.meta != nil, no assignment between' from (*Tx).check through recursivelyCheckBucket/Page -> checkInvariantProperties -> the forEachPage closure.
- **C249** (1 finding): Lift ReadPage's 'err==nil ⇒ p!=nil' summary through the surgeon.go:38 call site so p is known non-nil through the SetOverflow call at line 95.
- **C250** (1 finding): Attach summary 'ReadPage: err==nil ⇒ p!=nil' and substitute at call site surgeon.go:38 so p is non-nil through SetCount at line 78.
- **C253** (1 finding): Lift 'db.pageSize > 0' established in Open across the sole db.init() call edge into init's body so buf/&b[idx]/p.Meta() are known non-nil.
- **C255** (1 finding): Carry db.pageSize>0 post-Open plus prove count>=1 at both (*Tx).allocate call sites, substituting count>=1 through to (*DB).allocate.
- **C257** (1 finding): Model cobra's c==nil guard dominating c.RunE(c,...) and substitute cmd!=nil through the RunE closure and checkFunc call down to cmd.OutOrStdout() at line 70.
- **C258** (1 finding): Model cobra's c==nil guard dominating c.RunE(c,...) and substitute cmd!=nil through the RunE closure/checkFunc call chain down to line 59.
- **C262** (1 finding): Summarize ReadMetaPageAt as 'err==nil ⇒ ret0!=nil' and substitute at the command_surgery_meta.go:161 call site so it holds at line 169.
- **C277** (1 finding): Requires per-function summary that each newXCommand always returns non-nil *cobra.Command (single return, never reassigned nil), plus treating &T{...} composite literals as unconditionally non-nil, substituted at the AddCommand call site.
- **C297** (1 finding): lift Open's 'non-nil *DB or non-nil err, never both' postcondition through the cmd/bbolt/main.go dst,err:=bolt.Open(...); if err!=nil guard into the bolt.Compact(dst,...) call argument
- **C300** (1 finding): lift 'db.opened==true && db.data!=nil' from Open's construction (db.go:422 call site) and from the adjacent tx.db.data!=nil guard plus rwlock-ordering argument (tx.go:335 call site) into freepages so beginTx's nil-tx branch is provably unreachable there
- **C303** (1 finding): lift 'receiver != nil' from the &node{...} composite-literal construction at each of read's three call sites into read's entry state before the n.inodes access
- **C325** (1 finding): Lift into free()'s summary, from each of its 4 call sites, the fact that the receiver was already proven non-nil by a prior dereference in the calling function (or pageNode's p==nil⟹n!=nil postcondition), instead of treating *node as generically nilable at free()'s boundary.
- **C339** (1 finding): lift db.file!=nil from the db.go:234-238 assign-and-check through the intraprocedural mmap call at db.go:292, and separately through allocate()'s call at db.go:1168 by carrying the open/close lifecycle invariant
- **C340** (1 finding): carry db.file!=nil established at db.go:234-239 forward through straight-line statements 240-247 into the flock call at db.go:248
- **C341** (1 finding): carry rootNode!=nil from each call site (composite-literal init at bucket.go:190/259, or inlineable()'s n==nil check at bucket.go:706/802-808) into write()'s local read at bucket.go:834-835
- **C342** (1 finding): carry p!=nil proven by the earlier p.Id() call at db.go:1160 forward through db.go:1160-1165 into the second p.Id() call at db.go:1166
- **C345** (1 finding): carry db.file!=nil established at db.go:234-239 forward through db.go:239-268 into the db.init() call at db.go:269, discharging init()'s transitive db.file/db.ops.writeAt requirement
- **C347** (1 finding): substitute b with tx.root, recognize tx.root.tx==tx from construction, carry tx.db!=nil from the tx.go:185 guard, and apply the tx.meta!=nil iff tx.db!=nil cross-field invariant to discharge spill()'s b.tx.meta use
- **C353** (1 finding): lift the fact that Fill's bucket argument is always a non-empty literal (all call sites substitute e.g. []byte("data")) through to the CreateBucketIfNotExists call, plus the writable-tx guarantee from db.Update, to prove b!=nil at btesting.go:168
- **C358** (1 finding): synthesize Open's return-value summary (err==nil iff returned *DB is the non-nil db.go:179 allocation) and substitute it at each bolt.Open call site, propagating non-nil through the immediately following err!=nil guards for src and dst
- **C365** (1 finding): Carry tx.page/db.page's non-nil-or-panic postcondition to local p at tx_check.go:189, then thread BranchPageElement's non-nil-base+in-range contract to the p.BranchPageElement(i+1).Key() call at tx_check.go:201.
- **C371** (1 finding): Encode db.mmap()'s postcondition (err==nil implies meta0/meta1 non-nil) and carry it across the db.go:292 err check into the db.meta() call reached via hasSyncedFreelist() at db.go:308.
- **C373** (1 finding): Model ReadPage's return[0]-non-nil-iff-return[2]-nil correlation and carry it through the err!=nil guard at surgeon.go:37-39 into the p.Count() call at line 47.
- **C374** (1 finding): Track loadFreelist's db.freelist!=nil postcondition and carry it into tx_check.go:45's Copyall call (same-function, intraprocedural) and into shared.Write's recursive calls via Open()'s PreLoadFreelist=true guarantee.
- **C375** (1 finding): Model ReadMetaPageAt's (value,error) correlation and carry it through the err!=nil guard into the updateMetaField(m,...) call at line 161.
- **C376** (1 finding): Prove Tx.page/DB.page never return nil (address-of-slice-element), propagate through bucket.go's p==nil branch into node.read, and correlate guts_cli.ReadPage's (p,err) pairing across its three surgeon.go call sites.
- **C379** (1 finding): Model loadFreelist's db.freelist!=nil postcondition and carry it forward across the intervening statement to the tx.db.freelist.Count() call at tx_check.go:44.
- **C383** (1 finding): Carry the outer loop's BucketLeafFlag-observed fact through to b.Bucket(k)'s internal seek to match its non-nil postcondition at the recursivelyInspect call site.
- **C389** (1 finding): Lift tx.db!=nil from Commit's tx.go:185 guard across its sole call to writeMeta at tx.go:267 into writeMeta's entry state for tx.go:560.
- **C402** (1 finding): Model cobra's Command.execute as applying the Args:cobra.ExactArgs(1) field before invoking RunE, substituting len(args)==1 into the RunE closure's entry state at command_check.go:28.
- **C403** (1 finding): Substitute len(buckets)>=1 from each caller's else-if len(buckets)==0 return guard (main.go:851-853, 945-947) into findLastBucket's bucketNames parameter at both call sites (main.go:872, 960) to discharge bucketNames[0] at main.go:1784.
- **C405** (1 finding): Specialize hexdump's parameter n to the literal 16 at its sole caller (page_test.go:31) and show off(0)+n(16)<=sizeof(Page)(16) at that one instantiation, discharging the finding.
- **C406** (1 finding): Carry from node.spill()'s tx.allocate((node.size()+pageSize-1)/pageSize) call, through node.write and common.WriteInodeToPage, the fact that node.size()'s PageHeaderSize+Σ(elementSize+len(Key)+len(Value)) sum bounds the loop's running off, discharging UnsafeByteSlice's bound at inode.go:81.

- **FP/encoding findings** (434 findings, 185 classes — the dominant
  verdict, contrary to the design's expectation that
  make-from-param requires-lifting would dominate): each is a goverify
  bug in the analyzer's own encoding, not a fact about bbolt. Distilled
  into its handful of recurring mechanisms (a class can touch more than
  one; bucketed here by primary cause) — listed as fix-wave candidates
  for the plan owner, **not fixed in this task**:

  1. **Same-function dominating check not carried forward** (62
     classes, 121 findings): an earlier line in the same function
     already dereferences the identical never-reassigned
     receiver/field (a prior nil-check, a promoted-method call, or a
     plain field read), which already proves non-nilness for the
     remainder of the function body; the analyzer re-treats the later
     access as an independent nil-deref opportunity instead of carrying
     the in-function fact forward. Flow-insensitivity across re-reads.
     Example: `C015a` (bucket.go:106 vs. the identical `b.buckets`
     check at bucket.go:89 in the same function).
  2. **Address-of stack-local / composite-literal / slice-element /
     value-typed field** (48 classes, 111 findings): the flagged
     pointer is `&local`, `&buf[i]`, `&s.field` on an addressable Go
     value (a `var o T` local, a range-loop element, an embedded
     value-typed struct field) — Go guarantees such an address is never
     nil. The analyzer mismodels these address-of expressions as
     independently nilable pointers. Example: `C002b`
     (`&n.inodes[i]`), `C009b` (`(&o).Validate()` on a stack-local
     `var o surgeryBaseOptions`).
  3. **Unsafe-pointer / pointer-arithmetic derived value** (35 classes,
     111 findings): the flagged receiver is computed by
     `unsafe.Pointer(uintptr(base) + offset)` (`UnsafeIndex`,
     `LeafPageElement`, `db.page`/`tx.page` over an mmap'd slice) off an
     already-non-nil base; the arithmetic cannot realistically produce
     the nil address (would require 64-bit uintptr wraparound). The
     analyzer treats the arithmetic-derived pointer as a free/opaque
     nilable value decoupled from its non-null base. Example: `C001`
     (`internal/common/inode.go` `LeafPageElement`).
  4. **Stdlib constructor documented never-nil** (8 classes, 42
     findings): the value comes directly from a stdlib constructor
     (`flag.NewFlagSet(...)`) that always returns a freshly-allocated
     non-nil value; the analyzer models the constructor's return as
     nilable anyway. Example: `C003`.
  5. **Nil-map range is legal** (3 classes, 8 findings): ranging over a
     nil Go map is valid and performs zero iterations — no dereference
     occurs at all; the analyzer flags the loop body as if the range
     variable could be a nil-deref.
  6. **Other / miscellaneous encoding gaps** (29 classes, 41 findings):
     smaller distinct mechanisms not covered above — e.g. `append` on a
     nil slice never dereferencing it (`C138`), a reslice only reached
     after an in-bounds index proves its length (`C139`), a decoded
     `rune` from a UTF-8-validated string modeled as an unconstrained
     64-bit value (`C064`), a CLI-arg slice reslice already guarded by a
     preceding `len(args)>=1` check (`C062`).

  Mechanism counts sum to 185 classes / 434 findings, matching the
  FP/encoding row in Totals.

## Exit-criteria disposition (spec §7)

- **all findings triaged**: every row of the committed TSV
  (`docs/shakeout-phase4-bbolt-findings.tsv`) carries a `class_id` and
  `verdict` (1006/1006); mixed classes (`C015b`, 5 rows) are documented
  above with their sampled ratio.
- **FP rate recorded**: see Totals — 96.2% headline, 96.7% estimated.
- **"every fixed FP lands a corpus case"**: satisfied vacuously — no
  fixes are in scope for this task (design 2026-07-19 §1); the 78
  KNOWN-FP(phase-5) requires-lifting classes and the 185 FP/encoding
  classes above stand in as forward-looking red/green corpus targets
  for whichever future task fixes them.
- **dispatch-precision observations**: recorded above — none observed.

### Corpus pins

Task 5 (`testdata/corpus/knownfp/knownfp.go`,
`crates/goverify-checkers/tests/knownfp_corpus.rs`) pins CURRENT
(wrong) analyzer behavior for confirmed FP mechanisms, superseding the
original per-class pin plan: with 968 confirmed-FP findings across
438 classes, one pin per class is not viable. Instead, one minimal
pin per FP *mechanism group* (the encoding-mechanism buckets above, the
dominant invariant mechanisms, and the requires-lifting canonical
shapes), each citing 1-3 exemplar class ids. 9 pins reproduced; 3
mechanisms did not minimally reproduce outside bbolt's own context and
are recorded below instead.

**Pinned (9):**

| pin function(s) | mechanism group | exemplar classes |
|---|---|---|
| `BuildSurgeryOptions` / `baseOptions.Validate` | FP/encoding — address-of stack-local / composite-literal / slice-element / value-typed field (group 2) | C009b, C002b |
| `ReadElem` / `elemAt` | FP/encoding — unsafe-pointer / pointer-arithmetic derived value (group 3; manifests as a `bounds` finding in this checker snapshot rather than `nil-deref`) | C001, C057, C033 |
| `Tail` | FP/encoding — other/misc: reslice already guarded by a same-function length check (group 6) | C062 |
| `UseBucket` / `newBucket` / `bucket.Depth` | FP/invariant — field set at every construction site before exposure | C002a, C017b |
| `UseHandle` / `newHandle` / `handleID` | FP/invariant — err==nil ⇒ result!=nil paired-return contract of a callee, checked locally | C025, C004a |
| `Compact` / `beginTx` / `commitFn` / `commitTx` | FP/requires-lifting — err==nil ⇒ result!=nil postcondition not lifted across a call boundary, re-derived across a second guarded reassignment (report's own canonical example) | C009c |
| `Use` / `maybeSession` / `closeSession` | FP/requires-lifting — caller's prior (inline) dereference not carried into a callee's own nil-receiver check | C031a, C053b |
| `First` / `tail` | FP/requires-lifting — caller's length guard not lifted across the call boundary into the callee's index | C403, C218a |
| `BranchElemOffset` / `elemOffset` | FP/requires-lifting — bound-derived obligation (a `uint16` domain) kept local instead of lifted to the caller | C101 |

**Not minimally reproducible (3), recorded instead of pinned:**

| mechanism | exemplar classes | why dropped |
|---|---|---|
| FP/encoding group 1 — same-function dominating check not carried forward | C015a, C007a, C012 | Every minimal repro tried (a receiver from a branching constructor, a bare field/pointer-chain `!= nil` comparison checked twice around an intervening call, both flat map-field and two-hop pointer-field forms) produced no finding at either check. This checker snapshot does not appear to attach a nil-deref obligation to a bare `!= nil` comparison read in isolation — only to reads that flow into a further call, index, or arithmetic operation — so the mechanism doesn't reproduce standalone. |
| FP/encoding group 4 — stdlib constructor documented never-nil | C003 | Reproducing it needs a genuine external-package constructor (the point is that the analyzer treats an opaque dependency's return as generically nilable). Pulling in a real stdlib package ("flag") to test this dragged its entire transitive closure into the corpus's whole-DAG analysis and empirically blew the single-test run past 30 minutes — unacceptable for a blocking-tier corpus test — so this was reverted before it could even be assessed for correctness. |
| FP/encoding group 5 — nil-map range is legal | C038 | Same underlying reason as group 1 above: ranging over a map field used only for a nil-safe operation (`range`, which never dereferences a nil map) didn't register as an obligation site in either a bare-map-field or two-hop pointer-field form. |

## Fix-wave re-run (2026-07-20)

Status: gate check 2 initially triggered a hard-stop; verified by a
follow-up per-commit bisection investigation, whose conclusions are
reproduced inline below (they resolved all three open questions). This
section records the re-run, the four gate outcomes as adjudicated after
that investigation, and the wave's honest net effect.

### Run parameters

- goverify commit: `5e891cf0478217d9896b572f19f53d7d09bb8c8a` (branch
  `fixwave/fp-encoding`, fixes 1-5: commits `9c9d99f..788f25a`)
- bbolt ref: v1.4.0
- timeouts: infer 100 ms / obligation 250 ms (defaults, unchanged)
- findings: **509** (baseline: 1006 — a 497-finding, 49.4% reduction)
- wall clock: cache-cleared 108 s; warm (cache reused) 23-25 s across two
  repeated runs
- **cold-run caveat**: the phase-4 baseline's cold figure (372.47 s)
  included a first-ever bbolt clone plus a from-scratch release build.
  Neither was needed here (already present from prior sessions), so 108 s
  reflects only clearing the SMT query cache, not a true first-run figure.
- **determinism note**: three independent runs (cache-cleared + two warm)
  produced byte-identical finding *headers* (509/509, signature-level diff
  empty). The only non-determinism observed was in solver counterexample
  witnesses (`with:`/occasional `path:` detail) — cosmetic, outside the
  `(file:line:col, tag)` signature every gate check and the bucketer's own
  class-key are keyed on.

### Before/after totals per verdict bucket

| verdict (baseline) | baseline rows | rows still present (same signature) | vanished |
|---|---|---|---|
| TP | 33 | 24 | 9 |
| FP/encoding | 434 | 167 | 267 |
| FP/invariant | 410 | 217 | 193 |
| FP/requires-lifting | 124 | 102 | 22 |
| mixed (C015b) | 5 | not separately gated | — |
| **total** | **1006** | — | — |

New-run total: 509 findings.

### Before/after per fixed mechanism

Fixes 1-5 collectively target FP/encoding mechanisms 1-5 (fix1→mechanism 2,
fix2a/2b→mechanism 1, fix3→mechanism 3, fix4→mechanism 4, fix5→mechanism
5); mechanism 6 (other/misc) and the bounds/overflow checkers were not
touched by this wave.

- **FP/encoding overall (mechanisms 1-5 combined): 434 → 167 findings
  (61.5% reduction)**, 185 → 101 classes fully at zero.
- **Mechanism 5 (nil-map range, fix 5) — the one mechanism with a
  pre-documented partial result**: mechanism 5's 8 baseline findings
  (3 classes: C038 5, C362 1, C186 2) → 4 remain, all in C038 (5 → 4;
  C362 and C186 fully clean). The 4 residual C038 positions
  (hashmap.go:237:29, 255:29, 271:27, shared.go:224:24) are the
  SCC-widening residual, Task 7, binding, not re-litigated here.
- A fully exhaustive per-position reclassification into mechanisms 1/2/3/4
  vs. 6 for all 434 baseline findings was not redone from scratch for this
  task (that is itself the "83-class residual coverage decision" scope
  question below, not a bookkeeping gap); the class-level "reason" text in
  this doc's per-class table records each class's dominant, triage-time
  mechanism attribution, and gate 1 below is evaluated against that
  attribution plus targeted case-study verification.
- FP/requires-lifting: 124 → 102 (22 vanished, ~18%) — Use/closeSession
  (C031a/C053b family) plus the writeMeta relocation (see gate 3).
- FP/invariant: 410 → 217 (193 vanished, ~47%) — see gate 4; larger than
  any mechanism this wave nominally targeted, a side effect of fix 2b's
  dominance reasoning applying wave-wide across verdict buckets, not just
  FP/encoding.

### Gate 1 — vanished-class check: FAILS the plan's numeric bar (≥156 classes at zero); documented as a plan-expectation miss, not fix bugs

- 185 baseline FP/encoding classes; **101 fully vanished** (zero surviving
  positions); **84 have ≥1 surviving position** (167/434 findings survive,
  61.5% reduction at the finding level).
- Of the 84 survivors, **1 is the pre-documented C038 exception** (4
  positions, SCC-widening family, Task 7, binding — not a fix bug, not
  re-litigated).
- The remaining **83 classes** are the call-boundary / closure-capture /
  cross-function-postcondition **generalization of C038's own theme**: the
  fixes provably implement their specified *same-function* mechanisms (the
  corpus regression suite for fixes 1-5 passes in full — see Step 4 below),
  but real bbolt code routes the same textual patterns through boundaries
  those same-function mechanisms don't reach. Three verified case studies
  (via `goverify debug ir` + source inspection, not assumption):
  - **C001** (43 baseline findings, 27 survive): `internal/common/inode.go`'s
    `elem := p.LeafPageElement(...)` computes its pointer via a **call** to
    a shared `UnsafeIndex` helper
    (`(*leafPageElement)(UnsafeIndex(unsafe.Pointer(p), ...))`), not an
    inline `uintptr(...)+offset` conversion in the same function. Fix 3's
    Convert-arm non-nil assertion fires only when the uintptr arithmetic is
    syntactically inline in the *same* Convert; it does not propagate as a
    callee postcondition across the extra `UnsafeIndex` call boundary that
    bbolt's real code factors this into (the corpus's `elemAt` inlines
    everything in one function; `LeafPageElement`/`BranchPageElement` do
    not).
  - **C009b** (7 baseline findings, 7 survive — 100%, despite being the
    class explicitly pinned as fixed via the `BuildSurgeryOptions` corpus
    case): every real-bbolt instance
    (`newSurgeryClearPageCommand`/`newSurgeryClearPageElementsCommand`/etc.)
    has a `RunE: func(cmd, args) error { ... o.Validate() ... }`
    **closure** sitting between `var o T` and the later
    `o.AddFlags(...)`/`o.Validate()` call; the corpus repro has no such
    closure. Fix 1's never-nil-Alloc-dst fact, established for `o` in the
    enclosing function, does not visibly reach the closure's own analysis
    of the captured variable.
  - **C030** (6 baseline findings, 1 survives — `tx.go:338:59`,
    `tx.db.meta().Freelist()`): the 5 that vanished were "is tx.db nil"
    dominance cases (soundly fixed by fix 2b); the 1 survivor's subject is
    the **return value of `DB.meta()`** — a cross-function postcondition
    ("does `meta()` ever return nil?") that same-function dominance was
    never going to reach.
- **Full survivor-class enumeration** (84 classes, by tag; bounds/overflow
  survivors are trivially expected since neither checker was touched by
  this wave):
  - nil-deref (72, excl. C038): C001, C002b, C007a, C008a, C009b, C012,
    C014a, C017a, C022a, C030, C033, C039b, C055, C057, C060a, C061, C071,
    C088, C090b, C095, C105, C106b, C109, C110, C115, C116a, C118b, C126,
    C132, C140, C146, C153, C154, C155, C156, C163, C164, C172, C174,
    C175, C189, C209, C211, C214, C232, C234, C236, C238, C241, C244,
    C245, C251, C260, C264, C267, C268, C271, C274, C280, C327, C330,
    C333, C336, C346, C351, C354, C357, C359, C361, C390, C391, C394
  - bounds (9): C062, C215, C217, C218b, C399, C400, C401, C404, C408
  - overflow (2): C064, C226
  - nil-deref, mechanism 5 exception (1, pre-documented): C038
- **Plan-owner decision required**: closing the 83-class residual requires
  cross-function capabilities not in this fix-wave's plan (interprocedural
  propagation of non-nil facts across a call to a small shared helper,
  across closure-capture boundaries, and across certain postcondition-style
  cross-function facts). Accept as wave 1 + a follow-up wave, or extend
  this wave's scope — not a call this task makes unilaterally.

### Gate 2 — TP preservation (hard gate): PASSES at the letter after re-adjudication; one substantive cost to headline

All 9 originally-missing baseline TP-row signatures were bisected
per-commit (worktree builds of `6c1b879`/`9c9d99f`/`1feef18`/`3de8824`/
`d9ace1f`/`4ab6f54` against a shared `.gvir` extraction, since
`extractor`/`proto` are byte-identical across the whole wave). All 9 are
**sound attribution shifts** — the underlying vulnerability subject
remains separately caught (5 classes: C213, C097/C037a-family, C084, C380,
C049b — same-function redundant outer-receiver recheck discharged by a
dominating earlier deref, deeper subject unaffected), and the 6th
(**C083**) is a **mis-triage correction**, not a lost detector:

- **C083 itself is a sound discharge of a genuinely false positive.**
  `main.go:1035:28` was a lifted `options *BenchOptions != nil`
  precondition on `runWrites`, raised at its call site in
  `benchCommand.Run`. `Run` dereferences `options` unconditionally and
  repeatedly *before* line 1035 (`options.Work` at 1015, `options.Path` at
  1016/1022, `options.NoSync` at 1026) — every one of these strictly
  dominates 1035, so fix 2b's dominance reasoning correctly proves
  `options` non-nil there: if `options` were nil, `Run` would already have
  panicked at 1015. The baseline finding was a checker gap (dominance
  wasn't tracked pre-fix), not a live bug — the same textbook discharge
  already blessed for C213/C097/C084/C380/C049b.

- **Headline cost — the real FillPercent bug it was misattributed to has
  lost all detection.** The genuine, reachable panic is in
  `runWritesWithSource` at **`cmd/bbolt/main.go:1191`**:
  ```go
  b, _ := tx.CreateBucketIfNotExists(benchBucketName) // error IGNORED
  b.FillPercent = options.FillPercent                 // deref possibly-nil b
  ```
  `CreateBucketIfNotExists` returns `(nil, err)` on failure; discarding the
  error and writing `b.FillPercent` panics when a prior `-work` run left an
  incompatible non-bucket value at the key. This exact statement (line
  1191) was **never** flagged in *any* run, baseline included — `b` is a
  havoc'd call-result value, and `nil.rs`'s `obligations()` only raises a
  manifest finding when the subject is `is_const_nil`, has no free
  variables, or is params-only (nil.rs:149-157); a call-extract result is
  none of these, so the true first-failure site has always been
  inexpressible as a local obligation (a pre-existing phase-4 gap, not
  introduced by this wave). Baseline detection instead came entirely from
  *downstream* re-derefs of the same `b` — `main.go:1202:20` and the
  nested `1254:20` (both `b.Put`, triaged **C190 / FP/encoding**) and the
  nested `1239:41` (`top.CreateBucketIfNotExists`, triaged **C031a /
  FP/requires-lifting**). Post-fix-2b, the
  store at 1191 strictly dominates the loop body containing 1202, so
  fix 2b's `¬is_nil(b)` dominance fact discharges `b.Put`'s receiver
  requirement too — locally sound (reaching 1202 means 1191's store
  already executed, which faults on nil `b`) — and the nested call sites
  are discharged the same way. **Net effect: every detector of a real,
  reachable panic is now silent, each individual discharge is sound, and
  the aggregate loses the bug.** In-wave repair is out of scope: fixing
  1191 directly would require lifting a callee postcondition
  (`CreateBucketIfNotExists`'s `err == nil ⇒ result != nil`) across an
  ignored-error boundary — exactly the requires-lifting capability this
  wave doesn't implement — and any narrower patch risks reintroducing the
  FP/encoding classes fix 2b correctly retired. Phase-5 callee-postcondition
  / ignored-error work must restore this detector; recorded here as the
  wave's principal known cost.

- **Triage-quality implication**: baseline verdicts contain at least one
  TP/FP inversion. The row labelled TP (`C083`/1035) was the soundly-
  dischargeable false positive; the rows that actually covered the real
  bug (`C190`@1202, `C031a`@1239/1254) were triaged FP. The gate's raw
  "9 missing TP rows" count therefore *understates* the problem for this
  specific case: no TP *row* was truly lost, but a real, reachable bug
  that baseline caught is now caught nowhere.

**Verdict: gate 2 passes** (no genuine true positive lost at the row
level; all 9 signatures are sound), **with the FillPercent detection loss
recorded as the wave's most important open cost**, not folded into a
clean pass.

### Gate 3 — no new signatures: PASSES with explanations, both resolved

- **`tx.go:558:11`** (nil-deref, `writeMeta`): this is **C185**'s
  `writeMeta` concern (`tx.go:570:22`, `fdatasync` call-site requires,
  FP/requires-lifting) relocated by fix 2b to the function's *earliest*
  dereference of `tx.db` (`lg := tx.db.Logger()`, the function's first
  statement) — the later `fdatasync(tx.db)` call is then soundly
  discharged by dominance from this earlier point. Coverage is preserved;
  this is gate-4 (requires-lifting) territory, an allowed shrinkage, not a
  new bug. Its sibling, C185's `write()` half (`tx.go:526:22`), relocated
  identically to `tx.go:480:11` at fix 2b, but was then **removed
  entirely by fix 3** (`d9ace1f`) — `write()` has zero findings in the
  current build, with no surviving replacement. `write()` contains uintptr
  arithmetic (`written += uintptr(sz)`, `common.UnsafeByteSlice(...)`)
  that `writeMeta` does not, which is why fix 3 (uintptr-provenance work)
  affects one and not the other; the removal is a genuine solver-result
  change from fix 3's added asserts, not a skipped/bailed-out function.
  Recorded as an asymmetry worth flagging: two mitigating caveats keep it
  from being a lost *true* bug — C185 itself is triaged FP/requires-lifting
  (a checker gap, not a real vulnerability), and the relocated
  480/558 receiver-nil manifest finding is itself most likely an FP
  (`write`/`writeMeta` are only ever called on a live, open `tx` during
  commit) — so `write()` ending clean is arguably the better outcome, and
  `writeMeta` retaining 558 is arguably a residual FP rather than a
  meaningful detector. The asymmetry (relocated-then-removed vs.
  relocated-and-kept) is nonetheless real and worth a plan-owner's
  attention.
- **`cmd/bbolt/command_surgery.go:268:55`** (overflow,
  `surgeryClearPageElementFunc`): this is **C221**'s manifest overflow
  inside `ClearPageElements` (`internal/surgeon/surgeon.go:78:20`,
  `p.SetCount(uint16(start))`) converted by **fix 3** (`d9ace1f`) into an
  interprocedural overflow `requires` on `ClearPageElements` itself. All
  three call sites were checked for consistency: `ClearPageElements`'s
  only call with an unbounded caller value
  (`command_surgery.go:268`, `cfg.startElementIdx`, a raw CLI int) is
  flagged; `internal/surgeon/surgeon.go:20`'s `ClearPage` (which always
  passes the literal `start=0`) and its sole caller
  (`command_surgery.go:201`) are silent because `0` is provably in
  `uint16` range. Detection of the uint16 truncation is **preserved**, and
  arguably **more precise** than baseline: the old manifest fired
  unconditionally inside the callee (including on the always-safe
  `start=0` path); the new requires fires exactly at the one call site
  that can actually overflow.

### Gate 4 — requires-lifting / invariant delta (report-only, not gated)

- FP/requires-lifting: 124 → 102 (22 vanished, ~18%) — the documented
  Use/closeSession (C031a/C053b family, fix 2b) resolution plus the
  writeMeta relocation (gate 3).
- FP/invariant: 410 → 217 (193 vanished, ~47%) — substantially larger than
  any verdict bucket this wave explicitly targeted. This is the same
  dominance mechanism (fix 2b) discharging redundant same-function
  re-checks wave-wide, not confined to FP/encoding or TP rows; it should
  be read as further evidence that fix 2b's effect is systemic across
  verdict buckets rather than scoped narrowly to "mechanism 1."

### Known costs and open items

1. **FillPercent detection loss** (gate 2): the reachable panic at
   `cmd/bbolt/main.go:1191` (`b, _ := tx.CreateBucketIfNotExists(...);
   b.FillPercent = ...`, ignored error) is detected at no site in the
   current build. Every prior detector (`main.go:1035:28`/1202:20/1239:41/
   1254:20) is now soundly discharged by fix 2b's dominance reasoning, but
   the true first-failure site was never locally expressible (a
   pre-existing gap: a havoc'd call-result subject doesn't qualify for a
   manifest obligation under `nil.rs`'s `obligations()` — needs
   `is_const_nil`, no free vars, or params-only). Restoring detection
   needs phase-5 callee-postcondition / ignored-error-result lifting work
   (lifting `CreateBucketIfNotExists`'s `err == nil ⇒ result != nil`
   across the `b, _ := ...` discard) — out of scope for an in-wave patch,
   which risks reintroducing the FP/encoding classes fix 2b correctly
   retired.
2. **83-class gate-1 residual** (gate 1): a plan-owner decision — bless
   the C038-style closure/call-boundary/cross-function-postcondition gap
   wave-wide (generalizing the existing Task 7 adjustment, at roughly 20x
   its original scope) and accept as wave 1 + a follow-up wave, or treat
   the residual as unfinished scope for this wave.
3. **Triage TP/FP inversion** (gate 2): baseline verdicts contain at least
   one confirmed TP/FP inversion — `C083` (labelled TP) is the sound
   discharge; `C190`/`C031a` (labelled FP) covered the real bug. Worth a
   note for anyone re-deriving FP-rate statistics from the phase-4 totals
   above: the headline FP-rate arithmetic is unaffected (both sides of the
   inversion are single-digit finding counts), but it's a data point that
   phase-4's per-class triage, while extensively cross-checked, was not
   immune to this failure mode.
