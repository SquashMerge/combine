//! App entry: builds and runs the Tauri application.

/// Runs the Combine window until it is closed.
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("Combine failed to start")
}
