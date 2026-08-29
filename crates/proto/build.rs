fn main() {
    prost_build::Config::new()
        .compile_protos(&["src/jjlab.proto"], &["src/"])
        .expect("failed to compile jjlab.proto");
}