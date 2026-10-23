fn main() {
    println!("cargo:rerun-if-changed=src/sprite.wgsl");
    let source = std::fs::read_to_string("src/sprite.wgsl").expect("Read the fixed drawing shader");
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|error| panic!("Drawing shader: {}", error.emit_to_string(&source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("Compile the fixed drawing shader");
}
