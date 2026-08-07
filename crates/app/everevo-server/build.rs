//! Build script — ensures frontend dist directory exists for rust-embed.
//!
//! rust-embed requires the #[folder] path to exist at compile time.
//! When `frontend/dist/` is missing (e.g., in rust-analyzer or CI without
//! a prior frontend build), we create a placeholder so compilation succeeds.
//! The real frontend assets are embedded when a proper build is done via
//! `./build.ps1 release` (which runs `vite build` before `cargo build`).

use std::path::Path;

fn main() {
    let dist = Path::new("../../../frontend/dist");
    if !dist.join("index.html").exists() {
        // Create placeholder for rust-analyzer and ad-hoc cargo builds
        std::fs::create_dir_all(dist).ok();
        std::fs::write(
            dist.join("index.html"),
            "<html><body>Frontend not built. Run: cd frontend && npm run build</body></html>",
        )
        .ok();
        println!("cargo:warning=frontend/dist/ not found — created placeholder. Run `cd frontend && npm run build` for real assets.");
    }
    // Re-run if the dist directory changes
    println!("cargo:rerun-if-changed=../../../frontend/dist/index.html");
}
