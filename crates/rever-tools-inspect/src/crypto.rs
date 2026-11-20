//! Port of `src/main/mcp/tools/crypto.ts` — `crypto_chain`, `hash_iter`,
//! `hmac_compute` and `magic_hash_lookup`.
//!
//! `crypto_chain` works on UTF-16 code units, like the JavaScript string it
//! replaces: `reverse` splits surrogate pairs and the 200-character preview
//! cuts between code units, and a Rust `String` cannot hold the lone halves
//! those leave behind.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use md5::Md5;
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::args::validate;
use crate::js::{base64_bytes, hex, hex_bytes, js_number, json_string, json_stringify};

const URI_MALFORMED: &str = "URI malformed";

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `Buffer.from(s, 'utf8')`: a lone surrogate becomes U+FFFD.
fn utf8_bytes(input: &[u16]) -> Vec<u8> {
    String::from_utf16_lossy(input).into_bytes()
}

/// `buf.toString('utf8')`: bytes that are not UTF-8 become U+FFFD.
fn utf8_text(bytes: &[u8]) -> Vec<u16> {
    units(&String::from_utf8_lossy(bytes))
}

fn rot13(unit: u16) -> u16 {
    let base = match unit {
        0x61..=0x7a => 0x61,
        0x41..=0x5a => 0x41,
        _ => return unit,
    };
    (unit - base + 13) % 26 + base
}

/// `encodeURIComponent`, which throws on a lone surrogate.
fn encode_uri_component(input: &[u16]) -> Result<String, String> {
    let mut out = String::new();
    for c in char::decode_utf16(input.iter().copied()) {
        let c = c.map_err(|_| URI_MALFORMED)?;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) {
            out.push(c);
        } else {
            for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Ok(out)
}

/// `decodeURIComponent`: every `%XX` run must spell one UTF-8 character.
fn decode_uri_component(input: &[u16]) -> Result<Vec<u16>, String> {
    // The byte of the `%XX` escape at `at`, if there is one.
    let escape = |at: usize| -> Option<u8> {
        let digit = |unit: &u16| char::from_u32(u32::from(*unit))?.to_digit(16);
        if input.get(at) != Some(&u16::from(b'%')) {
            return None;
        }
        let (high, low) = (input.get(at + 1)?, input.get(at + 2)?);
        Some((digit(high)? << 4 | digit(low)?) as u8)
    };
    let mut out = Vec::with_capacity(input.len());
    let mut at = 0;
    while let Some(&unit) = input.get(at) {
        if unit != u16::from(b'%') {
            out.push(unit);
            at += 1;
            continue;
        }
        let first = escape(at).ok_or(URI_MALFORMED)?;
        at += 3;
        let length = first.leading_ones() as usize;
        if length == 0 {
            out.push(u16::from(first));
            continue;
        }
        let mut bytes = vec![first];
        for _ in 1..length {
            bytes.push(escape(at).ok_or(URI_MALFORMED)?);
            at += 3;
        }
        // Rejects what JavaScript rejects: a bare continuation byte, overlong
        // forms, encoded surrogates and code points past U+10FFFF.
        let text = std::str::from_utf8(&bytes).map_err(|_| URI_MALFORMED)?;
        out.extend(text.encode_utf16());
    }
    Ok(out)
}

/// `String(JSON.parse(input))`, on UTF-16 code units and with V8's wording
/// for a syntax error. serde_json cannot stand in for JSON.parse here: it
/// rejects what JavaScript accepts (a `\ud800` escape, `1e400`, nesting past
/// 128 levels) and a Rust string cannot hold the lone surrogate halves that
/// `reverse` leaves in the input.
fn json_parse_to_string(input: &[u16]) -> Result<Vec<u16>, String> {
    JsonToString {
        src: input,
        at: 0,
        out: Vec::new(),
        stack: Vec::new(),
        hidden: 0,
        unprintable: false,
    }
    .run()
}

enum Container {
    Array,
    /// `to_string`: the object has an own `toString` key, which makes
    /// `String(object)` throw (a JSON value is never callable).
    Object {
        to_string: bool,
    },
}

/// A JSON parser that writes what `String()` makes of the value as it goes
/// (array elements joined with commas, `null` elements empty, objects as
/// `[object Object]`), without recursion, so nesting depth is not limited by
/// the stack.
struct JsonToString<'a> {
    src: &'a [u16],
    at: usize,
    out: Vec<u16>,
    stack: Vec<Container>,
    /// How many objects are open: what is inside an object is never printed.
    hidden: usize,
    /// A printed object had its own `toString`.
    unprintable: bool,
}

impl JsonToString<'_> {
    fn peek(&self) -> Option<u16> {
        self.src.get(self.at).copied()
    }

    fn is(&self, c: u8) -> bool {
        self.peek() == Some(u16::from(c))
    }

    fn is_digit(&self) -> bool {
        self.peek().is_some_and(|c| (0x30..=0x39).contains(&c))
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(0x20 | 0x09 | 0x0a | 0x0d)) {
            self.at += 1;
        }
    }

    fn skip_digits(&mut self) {
        while self.is_digit() {
            self.at += 1;
        }
    }

    /// `<what> at position N (line L column C)`; `\r\n` is one line break.
    fn located(&self, what: &str) -> String {
        let before = self.src.get(..self.at).unwrap_or(self.src);
        let (mut line, mut line_start, mut i) = (1, 0, 0);
        while let Some(&c) = before.get(i) {
            if c == 0x0d && before.get(i + 1) == Some(&0x0a) {
                i += 1;
            }
            if matches!(before.get(i), Some(0x0d | 0x0a)) {
                line += 1;
                line_start = i + 1;
            }
            i += 1;
        }
        let column = 1 + self.at.saturating_sub(line_start);
        format!(
            "{what} at position {} (line {line} column {column})",
            self.at
        )
    }

    /// The error for a character no rule was waiting for.
    fn unexpected(&self) -> String {
        let Some(c) = self.peek() else {
            return "Unexpected end of JSON input".into();
        };
        if c == u16::from(b'-') || self.is_digit() {
            return self.located("Unexpected number in JSON");
        }
        if c == u16::from(b'"') {
            return self.located("Unexpected string in JSON");
        }
        let source = String::from_utf16_lossy(self.src);
        if ["[object Object]", "NaN", "Infinity", "undefined"].contains(&source.as_str()) {
            return format!("\"{source}\" is not valid JSON");
        }
        // A long source is quoted as the ten code units on each side.
        // shortcut: half of a surrogate pair (the token itself, or one cut by
        // the quote) prints as U+FFFD, since a Rust string cannot hold it.
        let (len, pos) = (self.src.len(), self.at);
        let (before, range, after) = if len < 21 {
            ("", 0..len, "")
        } else if pos < 10 {
            ("", 0..pos + 10, "...")
        } else if pos < len - 10 {
            ("...", pos - 10..pos + 10, "...")
        } else {
            ("...", pos - 10..len, "")
        };
        format!(
            "Unexpected token '{}', {before}\"{}\"{after} is not valid JSON",
            String::from_utf16_lossy(&[c]),
            String::from_utf16_lossy(self.src.get(range).unwrap_or(self.src)),
        )
    }

    /// The string literal starting at the cursor, unescaped.
    fn string(&mut self) -> Result<Vec<u16>, String> {
        let mut text = Vec::new();
        self.at += 1;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.located("Unterminated string in JSON"));
            };
            self.at += 1;
            match c {
                0x22 => return Ok(text),
                0x5c => {
                    // V8 looks the escape up in a Latin-1 table; the end of the
                    // input and anything past U+00FF get the generic error.
                    let Some(kind) = self.peek().and_then(|c| u8::try_from(c).ok()) else {
                        return Err(self.unexpected());
                    };
                    let escaped = match kind {
                        b'"' | b'\\' | b'/' => u16::from(kind),
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => 0x0a,
                        b'r' => 0x0d,
                        b't' => 0x09,
                        b'u' => {
                            let mut value = 0;
                            for _ in 0..4 {
                                self.at += 1;
                                let digit = self
                                    .peek()
                                    .and_then(|c| char::from_u32(u32::from(c))?.to_digit(16))
                                    .ok_or_else(|| self.located("Bad Unicode escape in JSON"))?;
                                value = value * 16 + digit as u16;
                            }
                            value
                        }
                        _ => return Err(self.located("Bad escaped character in JSON")),
                    };
                    text.push(escaped);
                    self.at += 1;
                }
                0..0x20 => {
                    self.at -= 1;
                    return Err(self.located("Bad control character in string literal in JSON"));
                }
                c => text.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.at;
        if self.is(b'-') {
            self.at += 1;
        }
        if self.is(b'0') {
            self.at += 1;
            // A leading zero stands alone.
            if self.is_digit() {
                return Err(self.unexpected());
            }
        } else if self.is_digit() {
            self.skip_digits();
        } else {
            return Err(self.located("No number after minus sign in JSON"));
        }
        if self.is(b'.') {
            self.at += 1;
            if !self.is_digit() {
                return Err(self.located("Unterminated fractional number in JSON"));
            }
            self.skip_digits();
        }
        if self.is(b'e') || self.is(b'E') {
            self.at += 1;
            if self.is(b'+') || self.is(b'-') {
                self.at += 1;
            }
            if !self.is_digit() {
                return Err(self.located("Exponent part is missing a number in JSON"));
            }
            self.skip_digits();
        }
        let token = self.src.get(start..self.at).unwrap_or_default();
        // Rounds to the nearest double, and to an infinity past the range, as JSON.parse does.
        Ok(String::from_utf16_lossy(token).parse().unwrap_or(f64::NAN))
    }

    fn literal(&mut self, word: &str) -> Result<(), String> {
        for expected in word.encode_utf16() {
            if self.peek() != Some(expected) {
                return Err(self.unexpected());
            }
            self.at += 1;
        }
        Ok(())
    }

    /// Expects the cursor on a property key and reads through its colon.
    fn key(&mut self, or_else: &str) -> Result<Vec<u16>, String> {
        self.skip_whitespace();
        if !self.is(b'"') {
            return Err(self.located(or_else));
        }
        let key = self.string()?;
        self.skip_whitespace();
        if !self.is(b':') {
            return Err(self.located("Expected ':' after property name in JSON"));
        }
        self.at += 1;
        Ok(key)
    }

    fn print(&mut self, text: &str) {
        if self.hidden == 0 {
            self.out.extend(text.encode_utf16());
        }
    }

    fn open_object(&mut self, key: &[u16]) {
        self.stack.push(Container::Object {
            to_string: key == units("toString"),
        });
        self.hidden += 1;
    }

    fn run(mut self) -> Result<Vec<u16>, String> {
        'value: loop {
            self.skip_whitespace();
            match self.peek().and_then(|c| u8::try_from(c).ok()) {
                Some(b'"') => {
                    let text = self.string()?;
                    if self.hidden == 0 {
                        self.out.extend(text);
                    }
                }
                Some(b'-' | b'0'..=b'9') => {
                    let text = match self.number()? {
                        f64::INFINITY => "Infinity".to_owned(),
                        f64::NEG_INFINITY => "-Infinity".to_owned(),
                        number => js_number(number),
                    };
                    self.print(&text);
                }
                Some(b'{') => {
                    self.at += 1;
                    self.skip_whitespace();
                    if self.is(b'}') {
                        self.at += 1;
                        self.print("[object Object]");
                    } else {
                        let key = self.key("Expected property name or '}' in JSON")?;
                        self.open_object(&key);
                        continue 'value;
                    }
                }
                Some(b'[') => {
                    self.at += 1;
                    self.skip_whitespace();
                    if self.is(b']') {
                        self.at += 1;
                    } else {
                        self.stack.push(Container::Array);
                        continue 'value;
                    }
                }
                Some(b't') => {
                    self.literal("true")?;
                    self.print("true");
                }
                Some(b'f') => {
                    self.literal("false")?;
                    self.print("false");
                }
                Some(b'n') => {
                    self.literal("null")?;
                    // `[null].join()` is empty; `String(null)` is not.
                    if self.stack.is_empty() {
                        self.print("null");
                    }
                }
                _ => return Err(self.unexpected()),
            }
            // A value just ended: close every container that ends with it.
            loop {
                self.skip_whitespace();
                match self.stack.last_mut() {
                    None => break 'value,
                    Some(Container::Array) => {
                        if self.is(b',') {
                            self.at += 1;
                            self.print(",");
                            continue 'value;
                        }
                        if !self.is(b']') {
                            return Err(
                                self.located("Expected ',' or ']' after array element in JSON")
                            );
                        }
                        self.at += 1;
                        self.stack.pop();
                    }
                    Some(Container::Object { to_string }) => {
                        let had_to_string = *to_string;
                        if self.is(b',') {
                            self.at += 1;
                            let key = self.key("Expected double-quoted property name in JSON")?;
                            if key == units("toString")
                                && let Some(Container::Object { to_string }) = self.stack.last_mut()
                                {
                                    *to_string = true;
                                }
                            continue 'value;
                        }
                        if !self.is(b'}') {
                            return Err(
                                self.located("Expected ',' or '}' after property value in JSON")
                            );
                        }
                        self.at += 1;
                        self.stack.pop();
                        self.hidden -= 1;
                        if self.hidden == 0 && had_to_string {
                            self.unprintable = true;
                        }
                        self.print("[object Object]");
                    }
                }
            }
        }
        if self.at < self.src.len() {
            return Err(self.located("Unexpected non-whitespace character after JSON"));
        }
        if self.unprintable {
            return Err("Cannot convert object to primitive value".into());
        }
        Ok(self.out)
    }
}

fn apply(input: &[u16], transform: &str) -> Result<Vec<u16>, String> {
    Ok(match transform {
        "base64-encode" => units(&STANDARD.encode(utf8_bytes(input))),
        // Node reads both alphabets under either name.
        "base64-decode" | "base64url-decode" => utf8_text(&base64_bytes(input.iter().copied())),
        "base64url-encode" => units(&URL_SAFE_NO_PAD.encode(utf8_bytes(input))),
        "hex-encode" => units(&hex(&utf8_bytes(input))),
        "hex-decode" => utf8_text(&hex_bytes(input.iter().copied())),
        "url-encode" => units(&encode_uri_component(input)?),
        "url-decode" => decode_uri_component(input)?,
        "reverse" => input.iter().rev().copied().collect(),
        "rot13" => input.iter().map(|unit| rot13(*unit)).collect(),
        // Both are a round trip through UTF-8, which only replaces lone surrogates.
        "utf8-encode" | "utf8-decode" => utf8_text(&utf8_bytes(input)),
        "json-stringify" => units(&json_string(input)),
        "json-parse" => json_parse_to_string(input)?,
        other => return Err(format!("unknown transform: {other}")),
    })
}

fn crypto_chain(args: &Value) -> Result<String, String> {
    let input = units(args.get("input").and_then(Value::as_str).unwrap_or(""));
    let transforms = args.get("transforms").and_then(Value::as_array);
    let mut steps = Vec::new();
    let mut cur = input.clone();
    for transform in transforms.into_iter().flatten().filter_map(Value::as_str) {
        cur = apply(&cur, transform)?;
        // cur.length > 200 ? cur.slice(0, 200) + '…' : cur
        let result = match cur.get(..200) {
            Some(head) if cur.len() > 200 => json_string(&[head, &[0x2026]].concat()),
            _ => json_string(&cur),
        };
        steps.push(format!(
            "    {{\n      \"transform\": {},\n      \"result\": {result}\n    }}",
            Value::from(transform)
        ));
    }
    // JSON.stringify({ input, steps, final }, null, 2), written out by hand
    // because the strings may hold lone surrogates.
    let steps = if steps.is_empty() {
        "[]".to_owned()
    } else {
        format!("[\n{}\n  ]", steps.join(",\n"))
    };
    Ok(format!(
        "{{\n  \"input\": {},\n  \"steps\": {steps},\n  \"final\": {}\n}}",
        json_string(&input),
        json_string(&cur)
    ))
}

fn digest(algorithm: &str, data: &[u8]) -> Vec<u8> {
    match algorithm {
        "md5" => Md5::digest(data).to_vec(),
        "sha1" => Sha1::digest(data).to_vec(),
        "sha512" => Sha512::digest(data).to_vec(),
        _ => Sha256::digest(data).to_vec(),
    }
}

fn hmac(algorithm: &str, key: &[u8], message: &[u8]) -> Option<Vec<u8>> {
    macro_rules! mac {
        ($hash:ty) => {{
            let mut mac = Hmac::<$hash>::new_from_slice(key).ok()?;
            mac.update(message);
            mac.finalize().into_bytes().to_vec()
        }};
    }
    Some(match algorithm {
        "md5" => mac!(Md5),
        "sha1" => mac!(Sha1),
        "sha512" => mac!(Sha512),
        _ => mac!(Sha256),
    })
}

/// `digest('hex' | 'base64')`.
fn encode(bytes: &[u8], encoding: &str) -> String {
    match encoding {
        "base64" => STANDARD.encode(bytes),
        _ => hex(bytes),
    }
}

// Well-known magic-hash collisions for PHP `==` / `md5(...,true)` SQLi tricks.
fn magic_hash_samples() -> Value {
    json!({
        "md5": [
            { "value": "240610708", "hash": "0e462097431906509019562988736854" },
            { "value": "QNKCDZO", "hash": "0e830400451993494058024219903391" },
            { "value": "aabg7XSs", "hash": "0e087386482136013740957780965295" },
            { "value": "aabC9RqS", "hash": "0e041022518165728065344349536299" }
        ],
        // For md5(...,true) raw-bytes SQLi (`' or '6É]\x1f\x...` patterns)
        "rawMd5SQLi": [
            { "value": "ffifdyop", "note": "md5(ffifdyop, true) contains \"'or'6\\xc2\\xa2\" — bypasses ' OR ' filter" }
        ],
        "sha1": [
            { "value": "10932435112", "hash": "0e07766915004133176347055865026311692244" }
        ]
    })
}

fn text<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

pub struct CryptoChain;

impl Tool for CryptoChain {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "crypto_chain",
            "Apply a chain of encoding/decoding transforms to a string. Useful for decoding nested base64/hex/url/rot13 cookie values or reverse-engineering custom obfuscation.",
            json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Input string"
                    },
                    "transforms": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["base64-encode", "base64-decode", "base64url-encode", "base64url-decode", "hex-encode", "hex-decode", "url-encode", "url-decode", "reverse", "rot13", "utf8-encode", "utf8-decode", "json-stringify", "json-parse"]
                        },
                        "description": "Pipeline of transforms applied in order"
                    }
                },
                "required": ["input", "transforms"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            match crypto_chain(&args) {
                Ok(text) => ok(text),
                Err(message) => err(message),
            }
        })
    }
}

pub struct HashIter;

impl Tool for HashIter {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "hash_iter",
            "Iterate a hash N times. Used for challenges that require finding a string whose nested hash matches a target (e.g. webhacking.kr SHA1 x 500).",
            json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Initial string"
                    },
                    "algorithm": {
                        "type": "string",
                        "enum": ["md5", "sha1", "sha256", "sha512"],
                        "description": "Hash algorithm"
                    },
                    "iterations": {
                        "type": "integer",
                        "exclusiveMinimum": 0,
                        "maximum": 100000,
                        "description": "Number of times to hash"
                    },
                    "encoding": {
                        "description": "Output encoding (default hex)",
                        "type": "string",
                        "enum": ["hex", "base64"]
                    }
                },
                "required": ["input", "algorithm", "iterations"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let algorithm = text(&args, "algorithm").unwrap_or("");
            let encoding = text(&args, "encoding").unwrap_or("hex");
            // Validated as an integer in 1..=100000.
            let iterations = args
                .get("iterations")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as u64;
            let mut cur = text(&args, "input").unwrap_or("").to_owned();
            for _ in 0..iterations {
                cur = encode(&digest(algorithm, cur.as_bytes()), encoding);
            }
            ok(json_stringify(
                &json!({
                    "algorithm": algorithm,
                    "iterations": iterations,
                    "encoding": encoding,
                    "result": cur,
                }),
                true,
            ))
        })
    }
}

pub struct HmacCompute;

impl Tool for HmacCompute {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "hmac_compute",
            "Compute HMAC of a message with a key (for API signature reversing).",
            json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string"
                    },
                    "key": {
                        "type": "string"
                    },
                    "algorithm": {
                        "type": "string",
                        "enum": ["md5", "sha1", "sha256", "sha512"]
                    },
                    "encoding": {
                        "type": "string",
                        "enum": ["hex", "base64"]
                    }
                },
                "required": ["message", "key", "algorithm"],
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let algorithm = text(&args, "algorithm").unwrap_or("");
            let encoding = text(&args, "encoding").unwrap_or("hex");
            let key = text(&args, "key").unwrap_or("");
            let message = text(&args, "message").unwrap_or("");
            let Some(mac) = hmac(algorithm, key.as_bytes(), message.as_bytes()) else {
                return err("Invalid key length");
            };
            ok(json_stringify(
                &json!({
                    "algorithm": algorithm,
                    "encoding": encoding,
                    "result": encode(&mac, encoding),
                }),
                true,
            ))
        })
    }
}

pub struct MagicHashLookup;

impl Tool for MagicHashLookup {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "magic_hash_lookup",
            "Return known \"magic hash\" values that PHP loose-comparison or md5(...,true) SQLi vulnerabilities accept (e.g. 240610708 hashes to \"0e...all digits\").",
            json!({
                "type": "object",
                "properties": {
                    "algorithm": {
                        "description": "Algorithm filter",
                        "type": "string",
                        "enum": ["md5", "sha1", "rawMd5SQLi"]
                    }
                },
                "$schema": "http://json-schema.org/draft-07/schema#"
            }),
        )
    }

    fn call<'a>(&'a self, _: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(invalid) = validate(&self.def(), &args) {
                return invalid;
            }
            let samples = magic_hash_samples();
            let out = match text(&args, "algorithm").filter(|name| !name.is_empty()) {
                Some(name) => json!({ name: samples.get(name) }),
                None => samples,
            };
            ok(json_stringify(&out, true))
        })
    }
}

pub fn register(registry: &mut ToolRegistry) {
    registry.register(CryptoChain);
    registry.register(HashIter);
    registry.register(HmacCompute);
    registry.register(MagicHashLookup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;

    // Every expected value below is what the TypeScript tool returned for the
    // same arguments under node.

    /// The `final` text of a one-step chain, or the error message.
    fn one(input: &str, transform: &str) -> Result<String, String> {
        apply(&units(input), transform).map(|out| String::from_utf16_lossy(&out))
    }

    async fn chain(input: &str, transforms: &[&str]) -> (bool, String) {
        let args = json!({ "input": input, "transforms": transforms });
        run(&CryptoChain, &ToolContext::for_tests(), args).await
    }

    fn step(transform: &str, result: &str) -> String {
        format!("    {{\n      \"transform\": \"{transform}\",\n      \"result\": {result}\n    }}")
    }

    fn report(input: &str, steps: &[String], last: &str) -> String {
        format!(
            "{{\n  \"input\": {input},\n  \"steps\": [\n{}\n  ],\n  \"final\": {last}\n}}",
            steps.join(",\n")
        )
    }

    #[tokio::test]
    async fn 변환을_순서대로_적용하고_단계마다_결과를_적는다() {
        let transforms = [
            "base64-encode",
            "hex-encode",
            "reverse",
            "rot13",
            "url-encode",
            "json-stringify",
        ];
        let expected = report(
            "\"hello\"",
            &[
                step("base64-encode", "\"aGVsbG8=\""),
                step("hex-encode", "\"614756736247383d\""),
                step("reverse", "\"d383742637657416\""),
                step("rot13", "\"q383742637657416\""),
                step("url-encode", "\"q383742637657416\""),
                step("json-stringify", r#""\"q383742637657416\"""#),
            ],
            r#""\"q383742637657416\"""#,
        );
        assert_eq!(chain("hello", &transforms).await, (false, expected));
    }

    #[tokio::test]
    async fn 변환이_없으면_입력이_그대로_결과다() {
        assert_eq!(
            chain("hello", &[]).await,
            (
                false,
                "{\n  \"input\": \"hello\",\n  \"steps\": [],\n  \"final\": \"hello\"\n}"
                    .to_owned()
            )
        );
    }

    #[test]
    fn base64와_hex로_인코딩한다() {
        assert_eq!(
            one("hello world", "base64-encode").as_deref(),
            Ok("aGVsbG8gd29ybGQ=")
        );
        assert_eq!(
            one("hello world", "base64url-encode").as_deref(),
            Ok("aGVsbG8gd29ybGQ")
        );
        assert_eq!(
            one("Hello, 世界! 😀", "base64-encode").as_deref(),
            Ok("SGVsbG8sIOS4lueVjCEg8J+YgA==")
        );
        assert_eq!(
            one("Hello, 世界! 😀", "base64url-encode").as_deref(),
            Ok("SGVsbG8sIOS4lueVjCEg8J-YgA")
        );
        assert_eq!(
            one("Hello, 世界! 😀", "hex-encode").as_deref(),
            Ok("48656c6c6f2c20e4b896e7958c2120f09f9880")
        );
        assert_eq!(one("", "base64-encode").as_deref(), Ok(""));
    }

    #[test]
    fn base64_디코딩은_노드처럼_너그럽고_깨진_utf8은_대체_문자가_된다() {
        assert_eq!(
            one("aGVsbG8gd29ybGQ=", "base64-decode").as_deref(),
            Ok("hello world")
        );
        assert_eq!(one("aGVs bG8", "base64url-decode").as_deref(), Ok("hello"));
        assert_eq!(one("aGVs=bG8=", "base64-decode").as_deref(), Ok("hel"));
        // Either alphabet under either name.
        assert_eq!(
            one("-_-_", "base64-decode"),
            one("+/+/", "base64url-decode")
        );
        assert_eq!(one("wyg=", "base64-decode").as_deref(), Ok("\u{fffd}("));
        assert_eq!(
            one("7aCA", "base64-decode").as_deref(),
            Ok("\u{fffd}\u{fffd}\u{fffd}")
        );
        assert_eq!(one("/w==", "base64-decode").as_deref(), Ok("\u{fffd}"));
        // Plain text is still "decoded", to whatever bytes its letters spell.
        assert_eq!(
            one("hello world", "base64-decode").as_deref(),
            Ok("\u{fffd}\u{fffd}e\u{fffd}\n+\u{fffd}")
        );
        assert_eq!(
            one("Hello, 世界! 😀", "base64-decode").as_deref(),
            Ok("\u{1d}\u{fffd}e\u{fffd}")
        );
    }

    #[test]
    fn hex_디코딩은_처음으로_잘못된_쌍에서_멈춘다() {
        assert_eq!(one("68656c6c6f", "hex-decode").as_deref(), Ok("hello"));
        assert_eq!(one("68656C6C6Fzz", "hex-decode").as_deref(), Ok("hello"));
        assert_eq!(one("hello world", "hex-decode").as_deref(), Ok(""));
        assert_eq!(one("c328", "hex-decode").as_deref(), Ok("\u{fffd}("));
        assert_eq!(
            one("eda080", "hex-decode").as_deref(),
            Ok("\u{fffd}\u{fffd}\u{fffd}")
        );
    }

    #[test]
    fn url_인코딩은_encode_uri_component와_같다() {
        assert_eq!(
            one("a b&c/d?e=f'()*!~-_.é😀", "url-encode").as_deref(),
            Ok("a%20b%26c%2Fd%3Fe%3Df'()*!~-_.%C3%A9%F0%9F%98%80")
        );
        assert_eq!(
            one("a\"b\\c\n\t\u{1}\u{7f}\u{2028}", "url-encode").as_deref(),
            Ok("a%22b%5Cc%0A%09%01%7F%E2%80%A8")
        );
    }

    #[test]
    fn url_디코딩은_decode_uri_component와_같다() {
        assert_eq!(
            one("%E4%B8%96%E7%95%8C%20x", "url-decode").as_deref(),
            Ok("世界 x")
        );
        assert_eq!(one("%41%20+%c3%a9", "url-decode").as_deref(), Ok("A +é"));
        assert_eq!(one("%F0%9F%98%80x", "url-decode").as_deref(), Ok("😀x"));
        assert_eq!(
            one("Hello, 世界! 😀", "url-decode").as_deref(),
            Ok("Hello, 世界! 😀")
        );
        for malformed in [
            "%",
            "%4",
            "%zz",
            "%C3",
            "%C3%28",
            "%ED%A0%80",
            "%C0%AF",
            "%F4%90%80%80",
            "%80",
            "%FF",
            "%E4%B8",
            "%E4%B8x",
        ] {
            assert_eq!(
                one(malformed, "url-decode"),
                Err("URI malformed".to_owned()),
                "{malformed}"
            );
        }
    }

    #[tokio::test]
    async fn 변환이_실패하면_단계_없이_오류만_돌려준다() {
        assert_eq!(
            chain("%C3%28", &["rot13", "url-decode"]).await,
            (true, "URI malformed".to_owned())
        );
        assert_eq!(
            chain("not json", &["json-parse"]).await,
            (
                true,
                "Unexpected token 'o', \"not json\" is not valid JSON".to_owned()
            )
        );
    }

    #[test]
    fn rot13은_ascii_글자만_돌린다() {
        assert_eq!(one("hello world", "rot13").as_deref(), Ok("uryyb jbeyq"));
        assert_eq!(
            one("Uryyb, Jbeyq! 123 é", "rot13").as_deref(),
            Ok("Hello, World! 123 é")
        );
        assert_eq!(one("azAZ", "rot13").as_deref(), Ok("nmNM"));
    }

    #[test]
    fn json_stringify는_따옴표와_제어_문자를_이스케이프한다() {
        assert_eq!(
            one("hello world", "json-stringify").as_deref(),
            Ok("\"hello world\"")
        );
        assert_eq!(
            one(
                "a\"b\\c\n\t\u{1}\u{7f}\u{2028}\u{8}\u{c}\r",
                "json-stringify"
            )
            .as_deref(),
            Ok("\"a\\\"b\\\\c\\n\\t\\u0001\u{7f}\u{2028}\\b\\f\\r\"")
        );
    }

    #[test]
    fn json_parse는_파싱한_값을_자바스크립트의_문자열로_바꾼다() {
        assert_eq!(
            one(r#"[1,[2,null,[3]],{"a":1},"x",true]"#, "json-parse").as_deref(),
            Ok("1,2,,3,[object Object],x,true")
        );
        assert_eq!(one("1e21", "json-parse").as_deref(), Ok("1e+21"));
        assert_eq!(one("-0", "json-parse").as_deref(), Ok("0"));
        assert_eq!(one("0.1", "json-parse").as_deref(), Ok("0.1"));
        assert_eq!(
            one("123456789012345678901234567890", "json-parse").as_deref(),
            Ok("1.2345678901234568e+29")
        );
        assert_eq!(one("\"str\\n\"", "json-parse").as_deref(), Ok("str\n"));
        assert_eq!(
            one(r#"{"a":1}"#, "json-parse").as_deref(),
            Ok("[object Object]")
        );
        assert_eq!(one("  5 ", "json-parse").as_deref(), Ok("5"));
        assert_eq!(one("null", "json-parse").as_deref(), Ok("null"));
        assert_eq!(one("true", "json-parse").as_deref(), Ok("true"));
        for invalid in ["", "abc", "{", "[1,]", "01", "'a'", "\u{feff}1"] {
            assert!(one(invalid, "json-parse").is_err(), "{invalid}");
        }
    }

    #[test]
    fn json_parse는_자바스크립트가_받는_값을_모두_받는다() {
        // Past the range of a double JSON.parse gives an infinity.
        assert_eq!(
            one("[1e400,-1e400,1e-400,-1e-400]", "json-parse").as_deref(),
            Ok("Infinity,-Infinity,0,0")
        );
        assert_eq!(
            one("1.7976931348623159e308", "json-parse").as_deref(),
            Ok("Infinity")
        );
        // A lone surrogate escape stays a lone surrogate.
        assert_eq!(
            apply(&units(r#"["\ud800",1]"#), "json-parse"),
            Ok(vec![0xd800, 0x2c, 0x31])
        );
        assert_eq!(one(r#""\uD83D\uDE00""#, "json-parse").as_deref(), Ok("😀"));
        // Nesting has no limit.
        let deep = format!("{}1{}", "[".repeat(3000), "]".repeat(3000));
        assert_eq!(one(&deep, "json-parse").as_deref(), Ok("1"));
        let deep = format!("{}1{}", "{\"a\":".repeat(2000), "}".repeat(2000));
        assert_eq!(one(&deep, "json-parse").as_deref(), Ok("[object Object]"));
        assert_eq!(
            one("[null,[null,[null,{\"a\":null}]],{}]", "json-parse").as_deref(),
            Ok(",,,[object Object],[object Object]")
        );
        assert_eq!(one(" \n\t\r[ ] ", "json-parse").as_deref(), Ok(""));
    }

    #[test]
    fn json_parse는_own_tostring이_있는_객체를_문자열로_못_바꾼다() {
        let cannot = Some("Cannot convert object to primitive value");
        assert_eq!(
            one(r#"{"toString":1}"#, "json-parse").err().as_deref(),
            cannot
        );
        assert_eq!(
            one(r#"[{"toString":"x"},1]"#, "json-parse")
                .err()
                .as_deref(),
            cannot
        );
        assert_eq!(
            one(r#"{"\u0074oString":1}"#, "json-parse").err().as_deref(),
            cannot
        );
        assert_eq!(
            one(r#"{"a":1,"toString":null}"#, "json-parse")
                .err()
                .as_deref(),
            cannot
        );
        // An object inside an object is never printed.
        assert_eq!(
            one(r#"{"a":{"toString":1}}"#, "json-parse").as_deref(),
            Ok("[object Object]")
        );
        assert_eq!(
            one(r#"[{"a":{"toString":1}}]"#, "json-parse").as_deref(),
            Ok("[object Object]")
        );
        // A syntax error wins: the value is only printed once it has parsed.
        assert_eq!(
            one(r#"{"toString":1,"toString":2}x"#, "json-parse")
                .err()
                .as_deref(),
            Some(
                "Unexpected non-whitespace character after JSON at position 27 (line 1 column 28)"
            )
        );
    }

    #[test]
    fn json_parse의_문법_오류는_v8의_문구다() {
        // Every message is what node's JSON.parse threw for the same input.
        let cases = [
            ("", "Unexpected end of JSON input"),
            ("[", "Unexpected end of JSON input"),
            ("tru", "Unexpected end of JSON input"),
            ("\"\\", "Unexpected end of JSON input"),
            ("abc", "Unexpected token 'a', \"abc\" is not valid JSON"),
            ("[1,]", "Unexpected token ']', \"[1,]\" is not valid JSON"),
            ("trux", "Unexpected token 'x', \"trux\" is not valid JSON"),
            (
                "\u{feff}1",
                "Unexpected token '\u{feff}', \"\u{feff}1\" is not valid JSON",
            ),
            (
                "\"\\\u{100}\"",
                "Unexpected token '\u{100}', \"\"\\\u{100}\"\" is not valid JSON",
            ),
            ("NaN", "\"NaN\" is not valid JSON"),
            ("[object Object]", "\"[object Object]\" is not valid JSON"),
            (" NaN", "Unexpected token 'N', \" NaN\" is not valid JSON"),
            (
                "01",
                "Unexpected number in JSON at position 1 (line 1 column 2)",
            ),
            (
                "tr1e",
                "Unexpected number in JSON at position 2 (line 1 column 3)",
            ),
            (
                "tr\"e",
                "Unexpected string in JSON at position 2 (line 1 column 3)",
            ),
            (
                "-",
                "No number after minus sign in JSON at position 1 (line 1 column 2)",
            ),
            (
                "1.",
                "Unterminated fractional number in JSON at position 2 (line 1 column 3)",
            ),
            (
                "1e+",
                "Exponent part is missing a number in JSON at position 3 (line 1 column 4)",
            ),
            (
                "\"a",
                "Unterminated string in JSON at position 2 (line 1 column 3)",
            ),
            (
                "\"a\\x\"",
                "Bad escaped character in JSON at position 3 (line 1 column 4)",
            ),
            (
                "\"\\u12\"",
                "Bad Unicode escape in JSON at position 5 (line 1 column 6)",
            ),
            (
                "\"tab\there\"",
                "Bad control character in string literal in JSON at position 4 (line 1 column 5)",
            ),
            (
                "{",
                "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
            ),
            (
                "{\"a\"}",
                "Expected ':' after property name in JSON at position 4 (line 1 column 5)",
            ),
            (
                "{\"a\":1,}",
                "Expected double-quoted property name in JSON at position 7 (line 1 column 8)",
            ),
            (
                "{\"a\":1 \"b\":2}",
                "Expected ',' or '}' after property value in JSON at position 7 (line 1 column 8)",
            ),
            (
                "[1 2]",
                "Expected ',' or ']' after array element in JSON at position 3 (line 1 column 4)",
            ),
            // Lines are counted on \n, \r and \r\n.
            (
                "[1,\n\n\n  2,\n3 4]",
                "Expected ',' or ']' after array element in JSON at position 13 (line 5 column 3)",
            ),
            (
                "true false",
                "Unexpected non-whitespace character after JSON at position 5 (line 1 column 6)",
            ),
        ];
        for (input, message) in cases {
            assert_eq!(
                one(input, "json-parse").err().as_deref(),
                Some(message),
                "{input}"
            );
        }
    }

    #[test]
    fn json_parse의_오류는_긴_입력을_앞뒤_열_글자만_인용한다() {
        let unexpected = |input: &str| one(input, "json-parse").unwrap_or_else(|message| message);
        let pad = |n: usize| " ".repeat(n);
        // Under 21 code units the whole input is quoted.
        assert_eq!(
            unexpected(&format!("{}x{}", pad(10), pad(9))),
            format!(
                "Unexpected token 'x', \"{}x{}\" is not valid JSON",
                pad(10),
                pad(9)
            )
        );
        assert_eq!(
            unexpected(&format!("{}x{}", pad(9), pad(11))),
            format!(
                "Unexpected token 'x', \"{}x{}\"... is not valid JSON",
                pad(9),
                pad(9)
            )
        );
        assert_eq!(
            unexpected(&format!("{}x{}", pad(10), pad(10))),
            format!(
                "Unexpected token 'x', ...\"{}x{}\"... is not valid JSON",
                pad(10),
                pad(9)
            )
        );
        assert_eq!(
            unexpected(&format!("{}x", pad(20))),
            format!(
                "Unexpected token 'x', ...\"{}x\" is not valid JSON",
                pad(10)
            )
        );
        assert_eq!(
            unexpected("[1,2,3,4,5,6,7,8,9,10,x,12,13,14,15,16,17,18]"),
            "Unexpected token 'x', ...\",7,8,9,10,x,12,13,14\"... is not valid JSON"
        );
    }

    #[tokio::test]
    async fn reverse는_utf16_코드_단위를_뒤집어_서로게이트_쌍을_쪼갠다() {
        let reversed = r#""dc\ude00\ud83dba""#;
        let input = "\"ab😀cd\"";
        assert_eq!(
            chain("ab😀cd", &["reverse"]).await,
            (false, report(input, &[step("reverse", reversed)], reversed))
        );
        // Reversing again restores the pair.
        assert_eq!(
            chain("ab😀cd", &["reverse", "reverse"]).await.1,
            report(
                input,
                &[step("reverse", reversed), step("reverse", input)],
                input
            )
        );
        assert_eq!(
            chain("Hello, 世界! 😀", &["reverse"]).await.1,
            report(
                "\"Hello, 世界! 😀\"",
                &[step("reverse", r#""\ude00\ud83d !界世 ,olleH""#)],
                r#""\ude00\ud83d !界世 ,olleH""#
            )
        );
    }

    #[tokio::test]
    async fn 쪼개진_서로게이트는_변환마다_자바스크립트처럼_다룬다() {
        /// The `final` of reversing "ab😀cd" and then applying `transform`.
        async fn after(transform: &str) -> (bool, String) {
            let (failed, text) = chain("ab😀cd", &["reverse", transform]).await;
            let last = text.rsplit("\"final\": ").next().unwrap_or("").to_owned();
            (failed, last.trim_end_matches("\n}").to_owned())
        }
        // encodeURIComponent throws on a lone surrogate.
        assert_eq!(
            chain("ab😀cd", &["reverse", "url-encode"]).await,
            (true, "URI malformed".to_owned())
        );
        // decodeURIComponent and rot13 pass it through.
        assert_eq!(
            after("url-decode").await,
            (false, r#""dc\ude00\ud83dba""#.to_owned())
        );
        assert_eq!(
            after("rot13").await,
            (false, r#""qp\ude00\ud83don""#.to_owned())
        );
        // JSON.stringify escapes it; a Buffer turns each half into U+FFFD.
        assert_eq!(
            after("json-stringify").await,
            (false, r#""\"dc\\ude00\\ud83dba\"""#.to_owned())
        );
        assert_eq!(
            after("base64-encode").await,
            (false, "\"ZGPvv73vv71iYQ==\"".to_owned())
        );
        assert_eq!(
            after("utf8-encode").await,
            (false, "\"dc\u{fffd}\u{fffd}ba\"".to_owned())
        );
        assert_eq!(
            after("utf8-decode").await,
            (false, "\"dc\u{fffd}\u{fffd}ba\"".to_owned())
        );
        // JSON.parse keeps a lone half that is inside a string literal.
        assert_eq!(
            chain("😀", &["json-stringify", "reverse", "json-parse"])
                .await
                .1,
            report(
                "\"😀\"",
                &[
                    step("json-stringify", r#""\"😀\"""#),
                    step("reverse", r#""\"\ude00\ud83d\"""#),
                    step("json-parse", r#""\ude00\ud83d""#),
                ],
                r#""\ude00\ud83d""#
            )
        );
        // Outside a string literal it is a syntax error, as in JavaScript.
        assert!(chain("1😀", &["reverse", "json-parse"]).await.0);
        // Read as base64, the low byte of U+DE00 is 0x00 and is skipped; that
        // of U+D83D is "=", which ends the data: "dc" decodes to one byte.
        assert_eq!(after("base64-decode").await, (false, "\"u\"".to_owned()));
    }

    #[tokio::test]
    async fn 단계_결과는_utf16으로_200자까지만_보여준다() {
        let exact = "x".repeat(200);
        let (_, text) = chain(&exact, &["rot13"]).await;
        assert!(text.contains(&format!("\"result\": \"{}\"\n", "k".repeat(200))));

        let (_, text) = chain(&"x".repeat(201), &["rot13"]).await;
        assert!(text.contains(&format!("\"result\": \"{}…\"\n", "k".repeat(200))));
        // The full text is still in `final`.
        assert!(text.ends_with(&format!("\"final\": \"{}\"\n}}", "k".repeat(201))));

        // 101 emoji are 202 code units: the preview keeps exactly 100 of them.
        let (_, text) = chain(&"😀".repeat(101), &["utf8-encode"]).await;
        assert!(text.contains(&format!("\"result\": \"{}…\"\n", "😀".repeat(100))));

        // A cut inside a pair leaves the lone half, as `slice(0, 200)` does.
        let straddling = format!("{}😀y", "x".repeat(199));
        let (_, text) = chain(&straddling, &["rot13"]).await;
        assert!(text.contains(&format!("\"result\": \"{}\\ud83d…\"\n", "k".repeat(199))));
        assert!(text.ends_with(&format!("\"final\": \"{}😀l\"\n}}", "k".repeat(199))));
    }

    #[tokio::test]
    async fn 입력의_따옴표와_제어_문자는_json으로_이스케이프한다() {
        let (failed, text) = chain("a\"b\\c\n\t\u{1}", &["utf8-encode"]).await;
        assert!(!failed);
        assert!(text.starts_with("{\n  \"input\": \"a\\\"b\\\\c\\n\\t\\u0001\",\n"));
    }

    #[tokio::test]
    async fn 모르는_변환과_틀린_타입은_검증_오류다() {
        let ctx = ToolContext::for_tests();
        let (failed, text) = chain("a", &["nope"]).await;
        assert!(failed);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool crypto_chain: [\n  {\n    \"code\": \"invalid_value\",\n    \"values\": [\n      \"base64-encode\",\n"
        ));
        assert!(text.ends_with(
            "    \"path\": [\n      \"transforms\",\n      0\n    ],\n    \"message\": \"Invalid option: expected one of \\\"base64-encode\\\"|\\\"base64-decode\\\"|\\\"base64url-encode\\\"|\\\"base64url-decode\\\"|\\\"hex-encode\\\"|\\\"hex-decode\\\"|\\\"url-encode\\\"|\\\"url-decode\\\"|\\\"reverse\\\"|\\\"rot13\\\"|\\\"utf8-encode\\\"|\\\"utf8-decode\\\"|\\\"json-stringify\\\"|\\\"json-parse\\\"\"\n  }\n]"
        ));
        let (failed, text) = run(&CryptoChain, &ctx, json!({ "input": "a" })).await;
        assert!(failed);
        assert!(text.contains("Invalid input: expected array, received undefined"));
        let args = json!({ "input": 5, "transforms": "rot13" });
        let (failed, text) = run(&CryptoChain, &ctx, args).await;
        assert!(failed);
        assert!(text.contains("Invalid input: expected string, received number"));
        assert!(text.contains("Invalid input: expected array, received string"));
    }

    async fn hash(args: Value) -> (bool, String) {
        run(&HashIter, &ToolContext::for_tests(), args).await
    }

    fn hash_report(algorithm: &str, iterations: u64, encoding: &str, result: &str) -> String {
        format!(
            "{{\n  \"algorithm\": \"{algorithm}\",\n  \"iterations\": {iterations},\n  \"encoding\": \"{encoding}\",\n  \"result\": \"{result}\"\n}}"
        )
    }

    #[tokio::test]
    async fn 해시를_n번_반복한다() {
        let cases = [
            ("md5", "hex", "5b9641fbf51b369cbd3c8fcb1c344e08"),
            ("md5", "base64", "9JpCL8g1hzpq5GFtEkiBZg=="),
            ("sha1", "hex", "7bf2380a2e11ea1506b821544841257cb051dca0"),
            ("sha1", "base64", "BAeEkrBE4d1d4xgxDDtenJzHGAY="),
            (
                "sha256",
                "hex",
                "dbfa4f39f0bde661d0074bb10527d167103e7ffbc20a05018f22871dab76c3a7",
            ),
            (
                "sha256",
                "base64",
                "MYfSxE9/b1TFwfoPGWWEE41r46QIQhe/s51GjujIxKA=",
            ),
            (
                "sha512",
                "hex",
                "88b6553e1beda7a9923ad19d456d960895cfd816d54d748a1f11d27d3a22f8a1c2835d53e1fec96fd5c39e83528dd500cb2951c38621c7a2f5ba462ac7af0f1f",
            ),
            (
                "sha512",
                "base64",
                "ycr3+9veri+CqgbqcbS6ZGTSIM3GsUxHe8syacFUEuY4gK00Akm1KYl5lfC0dNkCO5LzLZWtXMQHvYPt+rnE9A==",
            ),
        ];
        for (algorithm, encoding, result) in cases {
            let args = json!({
                "input": "hello 世界", "algorithm": algorithm, "iterations": 3, "encoding": encoding
            });
            assert_eq!(
                hash(args).await,
                (false, hash_report(algorithm, 3, encoding, result)),
                "{algorithm} {encoding}"
            );
        }
    }

    #[tokio::test]
    async fn 해시_인코딩의_기본값은_hex다() {
        let args = json!({ "input": "hello 世界", "algorithm": "md5", "iterations": 3 });
        assert_eq!(
            hash(args).await,
            (
                false,
                hash_report("md5", 3, "hex", "5b9641fbf51b369cbd3c8fcb1c344e08")
            )
        );
        let args = json!({ "input": "", "algorithm": "sha1", "iterations": 1 });
        assert_eq!(
            hash(args).await.1,
            hash_report("sha1", 1, "hex", "da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        // A whole number written as a float is accepted and printed as an integer.
        let args = json!({ "input": "a", "algorithm": "md5", "iterations": 2.0 });
        assert_eq!(
            hash(args).await.1,
            hash_report("md5", 2, "hex", "d7afde3e7059cd0a0fe09eec4b0008cd")
        );
    }

    #[tokio::test]
    async fn 해시는_십만_번까지_반복한다() {
        let args = json!({ "input": "a", "algorithm": "sha1", "iterations": 500 });
        assert_eq!(
            hash(args).await.1,
            hash_report(
                "sha1",
                500,
                "hex",
                "94d61f72ed15ea1cd412c6443b414eb0922bcd23"
            )
        );
        let args = json!({ "input": "a", "algorithm": "md5", "iterations": 100000 });
        assert_eq!(
            hash(args).await,
            (
                false,
                hash_report("md5", 100000, "hex", "1865914487677b15d410ad0860794aab")
            )
        );
        let args = json!({ "input": "a", "algorithm": "md5", "iterations": 100001 });
        assert_eq!(
            hash(args).await,
            (
                true,
                "MCP error -32602: Input validation error: Invalid arguments for tool hash_iter: [\n  {\n    \"origin\": \"number\",\n    \"code\": \"too_big\",\n    \"maximum\": 100000,\n    \"inclusive\": true,\n    \"path\": [\n      \"iterations\"\n    ],\n    \"message\": \"Too big: expected number to be <=100000\"\n  }\n]"
                    .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn 반복_횟수는_양의_정수여야_한다() {
        for iterations in [json!(0), json!(-3)] {
            let args = json!({ "input": "a", "algorithm": "md5", "iterations": iterations });
            assert_eq!(
                hash(args).await,
                (
                    true,
                    "MCP error -32602: Input validation error: Invalid arguments for tool hash_iter: [\n  {\n    \"origin\": \"number\",\n    \"code\": \"too_small\",\n    \"minimum\": 0,\n    \"inclusive\": false,\n    \"path\": [\n      \"iterations\"\n    ],\n    \"message\": \"Too small: expected number to be >0\"\n  }\n]"
                        .to_owned()
                )
            );
        }
        for (iterations, message) in [
            (json!(1.5), "Invalid input: expected int, received number"),
            (
                json!("3"),
                "Invalid input: expected number, received string",
            ),
            (
                json!(1e30),
                "Too big: expected int to be <=9007199254740991",
            ),
            (
                json!(-1e30),
                "Too small: expected int to be >=-9007199254740991",
            ),
        ] {
            let args = json!({ "input": "a", "algorithm": "md5", "iterations": iterations });
            let (failed, text) = hash(args).await;
            assert!(failed);
            assert!(text.contains(message), "{text}");
        }
        let args =
            json!({ "input": "a", "algorithm": "sha3", "iterations": 1, "encoding": "utf8" });
        let (failed, text) = hash(args).await;
        assert!(failed);
        assert!(text.contains(r#"expected one of \"md5\"|\"sha1\"|\"sha256\"|\"sha512\""#));
        assert!(text.contains(r#"expected one of \"hex\"|\"base64\""#));
    }

    async fn mac(args: Value) -> (bool, String) {
        run(&HmacCompute, &ToolContext::for_tests(), args).await
    }

    fn mac_report(algorithm: &str, encoding: &str, result: &str) -> String {
        format!(
            "{{\n  \"algorithm\": \"{algorithm}\",\n  \"encoding\": \"{encoding}\",\n  \"result\": \"{result}\"\n}}"
        )
    }

    #[tokio::test]
    async fn hmac을_계산한다() {
        let cases = [
            ("md5", "hex", "98efd20050add16b1200e4207d9cdc0d"),
            ("md5", "base64", "mO/SAFCt0WsSAOQgfZzcDQ=="),
            ("sha1", "hex", "70ab7ee467d2ce9e09db82e6d013f916c44bd20a"),
            ("sha1", "base64", "cKt+5GfSzp4J24Lm0BP5FsRL0go="),
            (
                "sha256",
                "hex",
                "387660cce31bdcb435f24dbac4b8096e14f505c7737c19edd1581abb42f081d3",
            ),
            (
                "sha256",
                "base64",
                "OHZgzOMb3LQ18k26xLgJbhT1BcdzfBnt0Vgau0LwgdM=",
            ),
            (
                "sha512",
                "hex",
                "e3658a8a490d3590644ecfbf66d481d9b4ea77bbed836d2b202b70ab53cf0bf93812cfb476f156559cce7b512a7039c75492e3233374e36b65457df9b1b4e12d",
            ),
            (
                "sha512",
                "base64",
                "42WKikkNNZBkTs+/ZtSB2bTqd7vtg20rICtwq1PPC/k4Es+0dvFWVZzOe1EqcDnHVJLjIzN042tlRX35sbThLQ==",
            ),
        ];
        for (algorithm, encoding, result) in cases {
            let args = json!({
                "message": "msg 😀", "key": "키", "algorithm": algorithm, "encoding": encoding
            });
            assert_eq!(
                mac(args).await,
                (false, mac_report(algorithm, encoding, result)),
                "{algorithm} {encoding}"
            );
        }
    }

    #[tokio::test]
    async fn hmac은_빈_키와_블록보다_긴_키도_받는다() {
        let cases = [
            (
                "md5",
                "74e6f7298a9c2d168935f58c001bad88",
                "7c0a70fc0e50f8b7afbf5fda867c9674",
            ),
            (
                "sha1",
                "fbdb1d1b18aa6c08324b7d64b71fb76370690e1d",
                "964eff445001f8a7aa84e0f4dd68a2b13773e01c",
            ),
            (
                "sha256",
                "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad",
                "ea5869bc9fe6c8d9ed0ba400ec0d33c663e1b6c11c6ed19bc00325ffffcae30b",
            ),
            (
                "sha512",
                "b936cee86c9f87aa5d3c6f2e84cb5a4239a5fe50480a6ec66b70ab5b1f4ac6730c6c515421b327ec1d69402e53dfb49ad7381eb067b338fd7b0cb22247225d47",
                "cd02fc7b1fa8a875c506516e8eb151df441a83152043eb08c05b04a27feda0aeba8a70134ffd4e3b77a3198c942f002feba7235671a8000e0705b71575e1c825",
            ),
        ];
        for (algorithm, empty, long_key) in cases {
            // The encoding defaults to hex.
            let args = json!({ "message": "", "key": "", "algorithm": algorithm });
            assert_eq!(
                mac(args).await,
                (false, mac_report(algorithm, "hex", empty))
            );
            let args = json!({ "message": "m", "key": "k".repeat(300), "algorithm": algorithm });
            assert_eq!(mac(args).await.1, mac_report(algorithm, "hex", long_key));
        }
    }

    #[tokio::test]
    async fn hmac_인자가_빠지면_검증_오류다() {
        let (failed, text) = mac(json!({})).await;
        assert!(failed);
        assert!(text.starts_with(
            "MCP error -32602: Input validation error: Invalid arguments for tool hmac_compute: ["
        ));
        assert_eq!(
            text.matches("expected string, received undefined").count(),
            2
        );
        assert!(text.contains(r#"expected one of \"md5\"|\"sha1\"|\"sha256\"|\"sha512\""#));
    }

    async fn magic(args: Value) -> (bool, String) {
        run(&MagicHashLookup, &ToolContext::for_tests(), args).await
    }

    const MD5_SAMPLES: &str = "  \"md5\": [\n    {\n      \"value\": \"240610708\",\n      \"hash\": \"0e462097431906509019562988736854\"\n    },\n    {\n      \"value\": \"QNKCDZO\",\n      \"hash\": \"0e830400451993494058024219903391\"\n    },\n    {\n      \"value\": \"aabg7XSs\",\n      \"hash\": \"0e087386482136013740957780965295\"\n    },\n    {\n      \"value\": \"aabC9RqS\",\n      \"hash\": \"0e041022518165728065344349536299\"\n    }\n  ]";
    const RAW_MD5_SAMPLES: &str = "  \"rawMd5SQLi\": [\n    {\n      \"value\": \"ffifdyop\",\n      \"note\": \"md5(ffifdyop, true) contains \\\"'or'6\\\\xc2\\\\xa2\\\" — bypasses ' OR ' filter\"\n    }\n  ]";
    const SHA1_SAMPLES: &str = "  \"sha1\": [\n    {\n      \"value\": \"10932435112\",\n      \"hash\": \"0e07766915004133176347055865026311692244\"\n    }\n  ]";

    #[tokio::test]
    async fn 매직_해시_표본을_전부_또는_알고리즘별로_돌려준다() {
        assert_eq!(
            magic(json!({})).await,
            (
                false,
                format!("{{\n{MD5_SAMPLES},\n{RAW_MD5_SAMPLES},\n{SHA1_SAMPLES}\n}}")
            )
        );
        for (algorithm, samples) in [
            ("md5", MD5_SAMPLES),
            ("rawMd5SQLi", RAW_MD5_SAMPLES),
            ("sha1", SHA1_SAMPLES),
        ] {
            assert_eq!(
                magic(json!({ "algorithm": algorithm })).await,
                (false, format!("{{\n{samples}\n}}"))
            );
        }
    }

    #[tokio::test]
    async fn 표본이_없는_알고리즘은_검증_오류다() {
        for algorithm in [json!("sha256"), json!(""), json!(5)] {
            let (failed, text) = magic(json!({ "algorithm": algorithm })).await;
            assert!(failed);
            assert!(text.ends_with(
                "\"message\": \"Invalid option: expected one of \\\"md5\\\"|\\\"sha1\\\"|\\\"rawMd5SQLi\\\"\"\n  }\n]"
            ));
        }
    }

    #[test]
    fn 표본의_md5와_sha1_값은_실제로_그_해시다() {
        let samples = magic_hash_samples();
        for (algorithm, list) in [("md5", &samples["md5"]), ("sha1", &samples["sha1"])] {
            for sample in list.as_array().into_iter().flatten() {
                let value = sample["value"].as_str().unwrap_or("");
                assert_eq!(
                    Some(hex(&digest(algorithm, value.as_bytes())).as_str()),
                    sample["hash"].as_str(),
                    "{value}"
                );
            }
        }
    }
}
