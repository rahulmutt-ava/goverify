fn main() {
    println!("cargo:rerun-if-changed=../../proto/gvir/v1/gvir.proto");
    prost_build::compile_protos(&["../../proto/gvir/v1/gvir.proto"], &["../../proto"])
        .expect("compile gvir.proto");
}
