//! The control socket: the CLI's way into the running app.
//!
//! One JSON request per line in, one JSON reply per line out, over a Unix
//! domain socket that only the user can open. The socket carries a token
//! because the file's mode is the only other lock on the door.
//!
//! ```text
//! → {"token":"…","tool":"list_requests","args":{}}
//! ← {"ok":true,"result":{…}}
//! ← {"ok":false,"error":"…"}
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

/// One tool call, as the CLI wrote it.
pub struct Call {
    pub tool: String,
    pub args: Value,
}

impl Call {
    /// `{"tool":"list_requests","args":{…}}`; `args` may be left out.
    fn parse(request: &Value) -> Result<Call, String> {
        let tool = request["tool"]
            .as_str()
            .ok_or_else(|| "request has no `tool`".to_owned())?;
        let args = request.get("args").cloned().unwrap_or(Value::Null);
        Ok(Call {
            tool: tool.to_owned(),
            args,
        })
    }
}

/// What the transport hands back: a tool result, or the reason there is none.
pub struct Reply {
    pub ok: bool,
    pub payload: Value,
}

impl Reply {
    pub fn result(value: Value) -> Self {
        Self {
            ok: true,
            payload: value,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            ok: false,
            payload: Value::String(message),
        }
    }

    fn to_line(&self) -> String {
        let envelope = if self.ok {
            json!({ "ok": true, "result": self.payload })
        } else {
            json!({ "ok": false, "error": self.payload })
        };
        format!("{envelope}\n")
    }
}

/// Runs one call. The adapter crate owns dispatch; this module owns the wire.
pub type Handler = Arc<dyn Fn(Call) -> Reply + Send + Sync>;

/// A listening socket. Dropping it stops the accept loop and removes the file.
pub struct ControlServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unlinks the socket and waits for the accept loop to notice.
    pub fn close(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.thread.is_none() {
            return;
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Binds `path` and answers calls in the background.
///
/// The file is created with mode 0600, so only the user can connect at all;
/// the token catches a client that somehow got past the file.
pub fn serve(path: &Path, token: &str, handler: Handler) -> Result<ControlServer, String> {
    // A socket left behind by a crashed run would make `bind` fail with
    // "address already in use"; nobody else can be listening or we could not
    // have gotten this far.
    // Not `create_dir_all`: a data folder that cannot be written is the app's
    // to report, and creating it here would hide that from its own test.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| format!("failed to bind {path:?}: {e}"))?;
    // Polled rather than blocked: a blocking accept cannot be woken from
    // another thread without connecting, which races with a rebind.
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("failed to poll {path:?}: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("failed to lock down {path:?}: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let token = token.to_owned();
    let owned_path = path.to_path_buf();
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("rever-control".to_owned())
        .spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // BSD and macOS hand the accepted socket the listener's
                        // non-blocking flag, which would fail a write halfway
                        // through a large reply.
                        if stream.set_nonblocking(false).is_err() {
                            continue;
                        }
                        let token = token.clone();
                        let handler = handler.clone();
                        // One thread per client: a scan's workers may all be
                        // talking at once, and each call blocks on the page.
                        let _ = std::thread::Builder::new()
                            .name("rever-control-client".to_owned())
                            .spawn(move || serve_client(stream, &token, &handler));
                    }
                    // Nobody is waiting: sleep a tick and look at the flag.
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // A bad connection is one client's problem, not the server's.
                    Err(_) => continue,
                }
            }
        })
        .map_err(|e| format!("failed to start the control socket thread: {e}"))?;

    Ok(ControlServer {
        path: owned_path,
        stop,
        thread: Some(thread),
    })
}

fn serve_client(stream: UnixStream, token: &str, handler: &Handler) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut writer = stream;
    for line in BufReader::new(read_half).lines() {
        let reply = match line {
            Ok(line) if !line.trim().is_empty() => answer(&line, token, handler),
            Ok(_) => continue,
            Err(_) => break,
        };
        if writer.write_all(reply.to_line().as_bytes()).is_err() {
            break;
        }
        let _ = writer.flush();
    }
}

fn answer(line: &str, token: &str, handler: &Handler) -> Reply {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return Reply::error("request is not JSON");
    };
    if request["token"].as_str() != Some(token) {
        return Reply::error("bad token");
    }
    match Call::parse(&request) {
        Ok(call) => (handler)(call),
        Err(message) => Reply::error(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufWriter;

    fn echo_handler() -> Handler {
        Arc::new(|call: Call| Reply::result(json!({ "tool": call.tool, "args": call.args })))
    }

    fn temp_socket(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rever-control-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("control.sock")
    }

    fn send(path: &Path, line: &str) -> Value {
        let stream = UnixStream::connect(path).unwrap();
        let mut writer = BufWriter::new(stream.try_clone().unwrap());
        writeln!(writer, "{line}").unwrap();
        writer.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    #[test]
    fn 도구_호출을_받아_결과를_돌려준다() {
        let path = temp_socket("call");
        let server = serve(&path, "t0ken", echo_handler()).unwrap();
        let reply = send(
            &path,
            r#"{"token":"t0ken","tool":"list_requests","args":{"limit":2}}"#,
        );
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["result"]["tool"], "list_requests");
        assert_eq!(reply["result"]["args"]["limit"], 2);
        server.close();
    }

    #[test]
    fn 소켓_버퍼보다_큰_응답도_끝까지_온다() {
        // macOS marks an accepted socket non-blocking, which used to cut the
        // reply at one buffer and hang the caller.
        let path = temp_socket("big");
        let big = "x".repeat(256 * 1024);
        let handler: Handler =
            Arc::new(move |_: Call| Reply::result(json!({ "text": big.clone() })));
        let server = serve(&path, "t0ken", handler).unwrap();
        let reply = send(&path, r#"{"token":"t0ken","tool":"big"}"#);
        assert_eq!(reply["result"]["text"].as_str().unwrap().len(), 256 * 1024);
        server.close();
    }

    #[test]
    fn 토큰이_틀리면_거절한다() {
        let path = temp_socket("token");
        let server = serve(&path, "t0ken", echo_handler()).unwrap();
        let reply = send(&path, r#"{"token":"wrong","tool":"list_requests"}"#);
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "bad token");
        server.close();
    }

    #[test]
    fn 소켓_파일은_소유자만_연다() {
        let path = temp_socket("mode");
        let server = serve(&path, "t0ken", echo_handler()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        server.close();
    }

    #[test]
    fn 닫으면_소켓_파일이_사라진다() {
        let path = temp_socket("close");
        let server = serve(&path, "t0ken", echo_handler()).unwrap();
        assert!(path.exists());
        server.close();
        assert!(!path.exists());
    }
}
