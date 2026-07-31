// EverEvo Desktop — Tauri v2 shell
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::OnceLock;
use tauri::Manager;

static SERVER_PORT: OnceLock<u16> = OnceLock::new();

#[tauri::command]
fn server_url() -> String {
    let port = SERVER_PORT.get().copied().unwrap_or(3000);
    format!("http://127.0.0.1:{}", port)
}

fn main() {
    // ── CRITICAL: Windows DLL name-collision defense ──────────────────
    // ── Tauri's WebView2 may load C:\Windows\System32\onnxruntime.dll
    // ── (v1.17.1, Windows ML). Once a module with the same basename is
    // ── loaded, LoadLibrary("onnxruntime.dll") returns the stale handle.
    // ── We preload our v1.21.0 DLL BEFORE anything else, AND set
    // ── ORT_DYLIB_PATH so the ort crate finds it.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest_dir.parent().expect("CARGO_MANIFEST_DIR has no parent");
    let ort_dll = project_root.join("data").join("runtime").join("onnxruntime").join("lib").join("onnxruntime.dll");
    if ort_dll.exists() {
        std::env::set_var("ORT_DYLIB_PATH", &ort_dll);
        // Force-load our DLL into the process NOW, before Tauri/WebView2 can
        // pull in the stale System32 version.
        let wide: Vec<u16> = ort_dll
            .to_str()
            .unwrap_or("")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let h = unsafe {
            windows_load_library(wide.as_ptr())
        };
        if h.is_null() {
            eprintln!("[everevo] WARNING: PreloadLibrary failed for {}", ort_dll.display());
        } else {
            eprintln!("[everevo] Preloaded ORT DLL: {}", ort_dll.display());
        }
    } else {
        eprintln!("[everevo] WARNING: onnxruntime.dll not found at {}", ort_dll.display());
    }
    // ── end ORT preload ──────────────────────────────────────────────

    // Initialize tracing so sandbox and server logs are visible
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("everevo=debug,info")),
        )
        .init();

    // Fix CWD: Tauri's DevCommand runs from src-tauri/, but the project root
    // (where config, data/, and frontend/ live) is one level up. We set
    // EVEREVO_DATA_DIR so AppConfig::load() resolves data/ relative to project root.
    std::env::set_var("EVEREVO_DATA_DIR", project_root.join("data"));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![server_url])
        .setup(|app| {
            // Resolve bundled resource directory for offline asset extraction.
            // Falls back to empty if no resources were bundled (dev mode).
            if let Ok(resource_dir) = app.path().resource_dir() {
                let bundled = resource_dir.join("bundled");
                if bundled.exists() {
                    std::env::set_var("EVEREVO_RESOURCE_DIR", &bundled);
                    eprintln!("[everevo] Bundled resources at {}", bundled.display());
                } else {
                    std::env::set_var("EVEREVO_RESOURCE_DIR", "");
                }
            } else {
                std::env::set_var("EVEREVO_RESOURCE_DIR", "");
            }

            // Start Axum backend in background
            std::thread::spawn(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let config = everevo_core::AppConfig::load().unwrap_or_else(|e| {
                        tracing::error!("Config: {e}");
                        std::process::exit(1);
                    });
                    let port = config.server_port;
                    SERVER_PORT.set(port).unwrap();

                    let db_path = config.database_path();
                    let _ = std::fs::create_dir_all(db_path.parent().unwrap());
                    let db = everevo_db::Database::connect(&db_path).await.unwrap();

                    let data_dir = config.data_dir.clone();
                    let (app, state) = everevo_server::build_app(config, db).await.unwrap();

                    // ── Init orchestrator: provision → check LLM → startup checks ──
                    let pipeline = state.init_pipeline.clone();
                    let init_state = std::sync::Arc::clone(&state);
                    let data_dir_c = data_dir.clone();

                    if !pipeline.is_initialized() {
                        tracing::info!("First boot — provisioning in background…");
                        let mut events = pipeline.events();
                        let _handle = tokio::spawn(async move { pipeline.run().await });

                        tokio::spawn(async move {
                            while let Ok(event) = events.recv().await {
                                match event {
                                    everevo_bootstrap::pipeline::InitEvent::AssetDone { key, .. } => {
                                        tracing::info!(%key, "Asset provisioned");
                                    }
                                    everevo_bootstrap::pipeline::InitEvent::AssetFailed { key, error, .. } => {
                                        tracing::warn!(%key, %error, "Asset failed");
                                    }
                                    everevo_bootstrap::pipeline::InitEvent::AllDone => {
                                        tracing::info!("All assets provisioned");
                                        break;
                                    }
                                    everevo_bootstrap::pipeline::InitEvent::FatalError { error: e } => {
                                        tracing::error!(%e, "Init pipeline fatal error");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            everevo_server::main_impl::run_init_llm_phase(&init_state, &data_dir_c).await;
                        });
                    } else {
                        tracing::info!("Init marker found — already provisioned");
                        tokio::spawn(async move {
                            everevo_server::main_impl::run_init_llm_phase(&init_state, &data_dir_c).await;
                        });
                    }

                    // Start dreaming scheduler in background
                    let persona_profile = state.config.data_dir.join("memory").join("persona").join("profile.json");
                    let scheduler_handle = state.scheduler.start_background(
                        std::sync::Arc::clone(&state.dreaming_engine),
                        std::sync::Arc::clone(&state.fact_manager),
                        std::sync::Arc::clone(&state.wiki_generator),
                        Some(persona_profile),
                    );
                    tracing::info!("Dreaming scheduler started");

                    let addr = format!("127.0.0.1:{port}");
                    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
                    tracing::info!("EverEvo backend → http://{addr}");
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            let _ = tokio::signal::ctrl_c().await;
                            tracing::info!("Shutdown signal received, draining connections...");
                        })
                        .await
                        .unwrap();

                    state.destroy_all_sandboxes().await;
                    state.scheduler.stop();
                    tracing::info!("Dreaming scheduler stopped, waiting for background task...");
                    let _ = scheduler_handle.await;
                });
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("EverEvo failed");
}

/// Minimal preload helper: loads a DLL into the process via LoadLibraryExW.
/// Returns the module handle (null on failure).
/// We use LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS
/// so dependencies in the same directory are found first.
#[cfg(windows)]
unsafe fn windows_load_library(path: *const u16) -> *mut std::ffi::c_void {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(
            lpLibFileName: *const u16,
            hFile: *mut std::ffi::c_void,
            dwFlags: u32,
        ) -> *mut std::ffi::c_void;
    }
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
    unsafe { LoadLibraryExW(path, std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) }
}

#[cfg(not(windows))]
unsafe fn windows_load_library(_path: *const u16) -> *mut std::ffi::c_void {
    std::ptr::null_mut()
}
