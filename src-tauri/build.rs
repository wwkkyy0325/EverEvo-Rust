fn main() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest_dir.parent().expect("CARGO_MANIFEST_DIR has no parent");
    let ort_base = project_root.join("data").join("runtime").join("onnxruntime");

    // Search for lib/ — may be flat or in a versioned subdirectory
    let ort_lib = if ort_base.join("lib").join("onnxruntime.lib").exists() {
        Some(ort_base.join("lib"))
    } else {
        std::fs::read_dir(&ort_base).ok().and_then(|entries| {
            entries.filter_map(|e| e.ok()).find_map(|e| {
                let lib = e.path().join("lib").join("onnxruntime.lib");
                if lib.exists() { Some(e.path().join("lib")) } else { None }
            })
        })
    };

    if let Some(ref lib) = ort_lib {
        println!("cargo:rustc-link-search=native={}", lib.display());
        println!("cargo:warning=ONNX Runtime lib at {}", lib.display());
    } else if let Ok(env_path) = std::env::var("ORT_LIB_PATH") {
        println!("cargo:rustc-link-search=native={}", env_path);
    }

    tauri_build::build()
}
