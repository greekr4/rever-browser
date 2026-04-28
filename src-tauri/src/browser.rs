use tauri::{
    http, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Runtime, UriSchemeContext,
    UriSchemeResponder, Url, WebviewBuilder, WebviewUrl,
};

pub const BROWSER_WEBVIEW_LABEL: &str = "browser";

const INJECT_SCRIPT: &str = include_str!("./inject.js");

/// Custom URI scheme handler: receives JSON-encoded network events from the
/// injected script in the browser webview and re-emits them to the rest of the app.
pub fn handle_capture<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    let body = request.body().clone();

    tauri::async_runtime::spawn(async move {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
            let _ = app.emit("network-event", value);
        }

        responder.respond(
            http::Response::builder()
                .status(204)
                .header("Access-Control-Allow-Origin", "*")
                .body(Vec::<u8>::new())
                .unwrap(),
        );
    });
}

#[tauri::command]
pub async fn browser_navigate<R: Runtime>(app: AppHandle<R>, url: String) -> Result<(), String> {
    let webview = app
        .get_webview(BROWSER_WEBVIEW_LABEL)
        .ok_or_else(|| "browser webview not found".to_string())?;
    let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
    webview.navigate(parsed).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn browser_set_position<R: Runtime>(
    app: AppHandle<R>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let webview = app
        .get_webview(BROWSER_WEBVIEW_LABEL)
        .ok_or_else(|| "browser webview not found".to_string())?;
    webview
        .set_position(LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    webview
        .set_size(LogicalSize::new(w, h))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn browser_back<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let webview = app
        .get_webview(BROWSER_WEBVIEW_LABEL)
        .ok_or_else(|| "browser webview not found".to_string())?;
    webview.eval("history.back()").map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn browser_forward<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let webview = app
        .get_webview(BROWSER_WEBVIEW_LABEL)
        .ok_or_else(|| "browser webview not found".to_string())?;
    webview
        .eval("history.forward()")
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn browser_reload<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let webview = app
        .get_webview(BROWSER_WEBVIEW_LABEL)
        .ok_or_else(|| "browser webview not found".to_string())?;
    webview
        .eval("location.reload()")
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn create_browser_webview<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let webview_window = app
        .get_webview_window("main")
        .ok_or("main window not found")?;
    let window = webview_window.as_ref().window();

    let initial_url = Url::parse("https://www.google.com")?;
    let builder = WebviewBuilder::new(BROWSER_WEBVIEW_LABEL, WebviewUrl::External(initial_url))
        .initialization_script(INJECT_SCRIPT);

    window.add_child(
        builder,
        LogicalPosition::new(0.0, 80.0),
        LogicalSize::new(900.0, 700.0),
    )?;

    Ok(())
}
