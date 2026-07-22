//! Checked-in fuzz seeds, single-sourced from these builders.
//! `UPDATE_FUZZ_SEEDS=1` regenerates the files; otherwise the test
//! asserts they are byte-current (goldens convention, testutil.rs).

use prost::Message;

use goverify_extract::gvir;

/// A crafted package with a self-referential Named type reaching
/// `encode_func` through a parameter: the ir_encode fuzz target only
/// encodes functions, so the cycle must be reachable from one
/// (wave-2 spec §3).
fn named_cycle_package() -> gvir::Package {
    gvir::Package {
        import_path: "t".into(),
        types: vec![
            gvir::Type {
                id: 1,
                repr: "t.Self".into(),
                kind: gvir::TypeKind::Named as i32,
                name: "t.Self".into(),
                elem: 1,
                ..Default::default()
            },
            gvir::Type {
                id: 2,
                repr: "t.A".into(),
                kind: gvir::TypeKind::Named as i32,
                name: "t.A".into(),
                elem: 3,
                ..Default::default()
            },
            gvir::Type {
                id: 3,
                repr: "t.B".into(),
                kind: gvir::TypeKind::Named as i32,
                name: "t.B".into(),
                elem: 2,
                ..Default::default()
            },
        ],
        functions: vec![gvir::Function {
            id: "t.F".into(),
            params: vec![
                gvir::Param {
                    id: 1,
                    name: "p".into(),
                    r#type: 1,
                },
                gvir::Param {
                    id: 2,
                    name: "q".into(),
                    r#type: 2,
                },
            ],
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs: vec![gvir::Instruction {
                    kind: "Return".into(),
                    ..Default::default()
                }],
                succs: vec![],
                preds: vec![],
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn named_cycle_seed_is_current() {
    let bytes = named_cycle_package().encode_to_vec();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds/ir_encode/named-cycle.bin");
    if std::env::var_os("UPDATE_FUZZ_SEEDS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        return;
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("missing seed {path:?} ({e}); run with UPDATE_FUZZ_SEEDS=1"));
    assert_eq!(
        want, bytes,
        "named-cycle.bin drifted from its builder; regenerate with UPDATE_FUZZ_SEEDS=1"
    );
}
