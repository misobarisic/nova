fn main() {
    // Trusted bundled scripts share one context without a dynamic module loader.
    let files = [
        "core.js",
        "matching.js",
        "evidence.js",
        "enrichment.js",
        "metadata.js",
        "presentation.js",
        "sources/nyaa.js",
        "sources/animetosho.js",
        "sources/apibay.js",
        "sources/knaben.js",
        "sources/stremthru.js",
        "debrid/torbox.js",
        "index.js",
    ];
    let mut source = String::new();
    for file in files {
        let path = format!("plugins/supernova/{file}");
        println!("cargo:rerun-if-changed={path}");
        source.push_str(&std::fs::read_to_string(&path).expect("bundled Supernova source"));
        source.push_str("\n;\n");
    }
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("supernova.js"), source).expect("write bundled Supernova script");
}
