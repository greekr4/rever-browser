mod browser;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .register_asynchronous_uri_scheme_protocol("reverevt", browser::handle_capture)
        .invoke_handler(tauri::generate_handler![
            browser::browser_navigate,
            browser::browser_set_position,
            browser::browser_back,
            browser::browser_forward,
            browser::browser_reload,
        ])
        .setup(|app| {
            browser::create_browser_webview(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
