#[test]
fn manifest_entry_is_the_built_wasm() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/haunt-plugin.toml")).unwrap();
    let entry = format!("entry = \"target/wasm32-wasip2/release/{}.wasm\"", env!("CARGO_PKG_NAME").replace('-', "_"));
    assert!(manifest.contains(&entry), "haunt-plugin.toml should have {entry}");
}
