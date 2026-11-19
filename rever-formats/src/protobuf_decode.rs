//! Port of `src/main/mcp/protobuf-decode.ts` — a protobuf wire-format decoder,
//! the `protoc --decode_raw` equivalent. Recovers field-number → value
//! structure without a .proto schema. Pure functions only, so it stays
//! unit-testable.

use base64::Engine as _;
use indexmap::IndexMap;
use indexmap::map::Entry;
use serde::Serialize;

/// A 64-bit integer as JSON carried it in the TypeScript: a number while it is
/// exactly representable as a double, a decimal string beyond that.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ProtobufNumber {
    Int(i64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProtobufValue {
    Varint {
        value: ProtobufNumber,
        signed: ProtobufNumber,
    },
    Fixed64 {
        hex: String,
        uint64: ProtobufNumber,
        double: f64,
    },
    Fixed32 {
        hex: String,
        uint32: u32,
        /// Widened to f64 so it serializes with the digits JavaScript printed.
        float: f64,
    },
    Message {
        fields: ProtobufMessage,
    },
    String {
        value: String,
    },
    Bytes {
        base64: String,
        hex: String,
        length: usize,
    },
}

/// A field number seen once holds a value; seen again it becomes a list.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ProtobufField {
    One(ProtobufValue),
    Repeated(Vec<ProtobufValue>),
}

/// Keys are `#<field-number>`, in the order the fields first appear on the wire.
pub type ProtobufMessage = IndexMap<String, ProtobufField>;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtobufFrame {
    pub byte_length: usize,
    /// gRPC frame flag byte (0 = data, 1 = trailer). Absent when unframed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailer: Option<bool>,
    /// Trailer frames carry ASCII metadata (e.g. `grpc-status: 0`), not protobuf.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailer_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<ProtobufMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProtobufDecodeOptions {
    /// Force (true) or skip (false) gRPC frame parsing. Auto-detected when omitted.
    pub grpc_web: Option<bool>,
    /// Max nesting depth for length-delimited recursion (default 8), capped at
    /// `MAX_DEPTH_LIMIT`.
    pub max_depth: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtobufDecodeResult {
    pub grpc_web: bool,
    pub byte_length: usize,
    pub frames: Vec<ProtobufFrame>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrpcFrame<'a> {
    pub flag: u8,
    pub body: &'a [u8],
}

const DEFAULT_MAX_DEPTH: usize = 8;
/// The decoder recurses once per level. Node turned a blown stack into a caught
/// RangeError and reported the chunk as bytes; here it would abort the process,
/// so depth stops at protobuf's own recursion limit instead.
const MAX_DEPTH_LIMIT: usize = 100;
/// Number.MAX_SAFE_INTEGER: the largest integer a JSON number carries exactly.
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

/// Returns the value and the position after it. u128 because the tenth byte
/// can add seven bits at shift 63, and the TypeScript BigInt kept all of them.
fn read_varint(buf: &[u8], pos: usize) -> Result<(u128, usize), String> {
    let mut result: u128 = 0;
    let mut shift = 0;
    let mut p = pos;
    while let Some(&b) = buf.get(p) {
        p += 1;
        result |= u128::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok((result, p));
        }
        shift += 7;
        if shift > 63 {
            return Err("varint too long".into());
        }
    }
    Err("truncated varint".into())
}

fn num(v: i128) -> ProtobufNumber {
    match i64::try_from(v) {
        Ok(n) if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&v) => ProtobufNumber::Int(n),
        _ => ProtobufNumber::Text(v.to_string()),
    }
}

/// protobuf zigzag: sint32/sint64 encode negatives as small unsigned varints.
fn zigzag(v: i128) -> i128 {
    (v >> 1) ^ -(v & 1)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn take<const N: usize>(buf: &[u8], pos: usize) -> Option<[u8; N]> {
    buf.get(pos..pos.checked_add(N)?)?.try_into().ok()
}

/// Strict text test: valid UTF-8 with no control characters other than tab /
/// newline / carriage return. Nested messages almost always contain a length or
/// tag byte in the control range, so text wins only when it really is text.
fn as_text(slice: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(slice).ok()?;
    let control = |c: char| matches!(c, '\0'..='\x08' | '\x0b' | '\x0c' | '\x0e'..='\x1f' | '\x7f');
    (!s.contains(control)).then_some(s)
}

fn decode_message(buf: &[u8], depth: usize, max_depth: usize) -> Result<ProtobufMessage, String> {
    let mut out = ProtobufMessage::new();
    let mut pos = 0;
    while pos < buf.len() {
        let (tag, after_tag) = read_varint(buf, pos)?;
        pos = after_tag;
        let field = tag >> 3;
        let wire = tag & 7;
        if field == 0 {
            return Err("zero field number".into());
        }

        let val = match wire {
            0 => {
                let (v, p) = read_varint(buf, pos)?;
                pos = p;
                // A varint is at most 70 bits, so it always fits i128.
                let v = i128::try_from(v).map_err(|_| "varint too long")?;
                ProtobufValue::Varint {
                    value: num(v),
                    signed: num(zigzag(v)),
                }
            }
            1 => {
                let bytes: [u8; 8] = take(buf, pos).ok_or("truncated 64-bit value")?;
                pos += 8;
                ProtobufValue::Fixed64 {
                    hex: format!("0x{}", hex(&bytes)),
                    uint64: num(u64::from_le_bytes(bytes).into()),
                    double: f64::from_le_bytes(bytes),
                }
            }
            2 => {
                let (len, p) = read_varint(buf, pos)?;
                pos = p;
                let slice = usize::try_from(len)
                    .ok()
                    .and_then(|l| buf.get(pos..pos.checked_add(l)?))
                    .ok_or("truncated length-delimited value")?;
                pos += slice.len();
                interpret(slice, depth + 1, max_depth)
            }
            5 => {
                let bytes: [u8; 4] = take(buf, pos).ok_or("truncated 32-bit value")?;
                pos += 4;
                ProtobufValue::Fixed32 {
                    hex: format!("0x{}", hex(&bytes)),
                    uint32: u32::from_le_bytes(bytes),
                    float: f32::from_le_bytes(bytes).into(),
                }
            }
            // 3/4 = start/end group (deprecated since proto2), 6/7 = reserved.
            _ => return Err(format!("unsupported wire type {wire}")),
        };

        // Through f64 because the TypeScript key was a JS number: an oversized
        // field number (a 10-byte tag) prints rounded, exactly as it did there.
        match out.entry(format!("#{}", field as f64)) {
            Entry::Vacant(slot) => {
                slot.insert(ProtobufField::One(val));
            }
            Entry::Occupied(mut slot) => {
                let slot = slot.get_mut();
                *slot = match std::mem::replace(slot, ProtobufField::Repeated(Vec::new())) {
                    ProtobufField::One(first) => ProtobufField::Repeated(vec![first, val]),
                    ProtobufField::Repeated(mut list) => {
                        list.push(val);
                        ProtobufField::Repeated(list)
                    }
                };
            }
        }
    }
    Ok(out)
}

/// A length-delimited chunk is one of: string, nested message, or raw bytes.
fn interpret(slice: &[u8], depth: usize, max_depth: usize) -> ProtobufValue {
    if let Some(text) = as_text(slice) {
        return ProtobufValue::String {
            value: text.to_owned(),
        };
    }
    // A chunk that fails to parse is not a nested message: fall through to raw bytes.
    if depth < max_depth
        && let Ok(fields) = decode_message(slice, depth, max_depth)
        && !fields.is_empty()
    {
        return ProtobufValue::Message { fields };
    }
    ProtobufValue::Bytes {
        base64: base64::engine::general_purpose::STANDARD.encode(slice),
        hex: hex(slice),
        length: slice.len(),
    }
}

/// gRPC / gRPC-web framing: each frame is a 1-byte flag (0 = data, 1 = trailer)
/// plus a 4-byte big-endian length. Returns None when the buffer is not an exact
/// sequence of frames (i.e. it is a bare protobuf message).
pub fn split_grpc_frames(buf: &[u8]) -> Option<Vec<GrpcFrame<'_>>> {
    let mut frames = Vec::new();
    let mut rest = buf;
    while !rest.is_empty() {
        let (header, tail) = rest.split_first_chunk::<5>()?;
        let [flag, len @ ..] = *header;
        if flag > 1 {
            return None;
        }
        let len = usize::try_from(u32::from_be_bytes(len)).ok()?;
        let (body, after) = tail.split_at_checked(len)?;
        frames.push(GrpcFrame { flag, body });
        rest = after;
    }
    (!frames.is_empty()).then_some(frames)
}

/// Decode raw bytes as protobuf, transparently handling gRPC/gRPC-web framing.
/// Never fails for malformed input: a frame that fails to parse reports `error`.
pub fn decode_protobuf(buf: &[u8], options: ProtobufDecodeOptions) -> ProtobufDecodeResult {
    let max_depth = options
        .max_depth
        .unwrap_or(DEFAULT_MAX_DEPTH)
        .min(MAX_DEPTH_LIMIT);
    let framed = if options.grpc_web == Some(false) {
        None
    } else {
        split_grpc_frames(buf)
    };

    let decode_one = |body: &[u8], flag: Option<u8>| -> ProtobufFrame {
        let mut frame = ProtobufFrame {
            byte_length: body.len(),
            flag,
            ..Default::default()
        };
        if flag == Some(1) {
            frame.trailer = Some(true);
            frame.trailer_text = Some(String::from_utf8_lossy(body).into_owned());
            return frame;
        }
        match decode_message(body, 0, max_depth) {
            Ok(fields) => frame.fields = Some(fields),
            Err(e) => frame.error = Some(e),
        }
        frame
    };

    if let Some(framed) = framed {
        return ProtobufDecodeResult {
            grpc_web: true,
            byte_length: buf.len(),
            frames: framed
                .iter()
                .map(|f| decode_one(f.body, Some(f.flag)))
                .collect(),
        };
    }
    if options.grpc_web == Some(true) {
        return ProtobufDecodeResult {
            grpc_web: false,
            byte_length: buf.len(),
            frames: vec![ProtobufFrame {
                byte_length: buf.len(),
                error: Some(
                    "not a valid sequence of gRPC frames (1-byte flag + 4-byte big-endian length)"
                        .into(),
                ),
                ..Default::default()
            }],
        };
    }
    ProtobufDecodeResult {
        grpc_web: false,
        byte_length: buf.len(),
        frames: vec![decode_one(buf, None)],
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    // Wire bytes are hand-built here so the expectations are schema-free by construction.
    fn varint(n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut v = n;
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v > 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                return out;
            }
        }
    }

    fn tag(field: u64, wire: u64) -> Vec<u8> {
        varint(field * 8 + wire)
    }

    fn len_delim(field: u64, payload: &[u8]) -> Vec<u8> {
        [
            tag(field, 2),
            varint(payload.len() as u64),
            payload.to_vec(),
        ]
        .concat()
    }

    fn grpc_frame(flag: u8, payload: &[u8]) -> Vec<u8> {
        [
            vec![flag],
            (payload.len() as u32).to_be_bytes().to_vec(),
            payload.to_vec(),
        ]
        .concat()
    }

    fn decode(buf: &[u8]) -> ProtobufDecodeResult {
        decode_protobuf(buf, ProtobufDecodeOptions::default())
    }

    /// The first frame's fields as the JSON the tool returns.
    fn fields(buf: &[u8]) -> Value {
        json(&decode(buf).frames[0].fields)
    }

    fn json<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    fn data_frame() -> Vec<u8> {
        grpc_frame(0, &[tag(1, 0), varint(150)].concat())
    }

    fn trailer_frame() -> Vec<u8> {
        grpc_frame(1, b"grpc-status:0\r\n")
    }

    // wire types

    #[test]
    fn varint_필드_번호와_값을_복원한다() {
        let res = decode(&[tag(1, 0), varint(150)].concat());
        assert!(!res.grpc_web);
        assert_eq!(res.frames.len(), 1);
        assert_eq!(
            json(&res.frames[0].fields),
            json!({ "#1": { "kind": "varint", "value": 150, "signed": 75 } })
        );
    }

    #[test]
    fn varint의_zigzag_부호_해석을_함께_보여준다() {
        assert_eq!(
            fields(&[tag(1, 0), varint(3)].concat())["#1"],
            json!({ "kind": "varint", "value": 3, "signed": -2 })
        );
    }

    #[test]
    fn max_safe_integer를_넘는_varint는_문자열로_표시한다() {
        // 2^63 - 1
        let buf = [
            tag(1, 0),
            vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        ]
        .concat();
        let field = &fields(&buf)["#1"];
        assert_eq!(field["kind"], "varint");
        assert_eq!(field["value"], "9223372036854775807");
    }

    #[test]
    fn 출력_가능한_텍스트인_길이_구분_필드는_문자열로_읽는다() {
        assert_eq!(
            fields(&len_delim(2, b"testing")),
            json!({ "#2": { "kind": "string", "value": "testing" } })
        );
    }

    #[test]
    fn 중첩_메시지로_재귀한다() {
        let nested = [tag(1, 0), varint(42)].concat();
        assert_eq!(
            fields(&len_delim(3, &nested)),
            json!({
                "#3": {
                    "kind": "message",
                    "fields": { "#1": { "kind": "varint", "value": 42, "signed": 21 } }
                }
            })
        );
    }

    #[test]
    fn 메시지도_텍스트도_아닌_조각은_base64와_hex_바이트로_돌려준다() {
        assert_eq!(
            fields(&len_delim(7, &[0x00, 0xff]))["#7"],
            json!({ "kind": "bytes", "base64": "AP8=", "hex": "00ff", "length": 2 })
        );
    }

    #[test]
    fn fixed32는_uint32와_float으로_fixed64는_uint64와_double로_보여준다() {
        let f32_bytes = 1.5f32.to_le_bytes();
        let f64_bytes = 2.5f64.to_le_bytes();
        let buf = [tag(5, 5), f32_bytes.to_vec(), tag(6, 1), f64_bytes.to_vec()].concat();
        let fields = fields(&buf);
        assert_eq!(fields["#5"]["kind"], "fixed32");
        assert_eq!(fields["#5"]["uint32"], u32::from_le_bytes(f32_bytes));
        assert_eq!(fields["#5"]["float"], 1.5);
        assert_eq!(fields["#6"]["kind"], "fixed64");
        assert_eq!(fields["#6"]["double"], 2.5);
    }

    #[test]
    fn 반복된_필드_번호는_목록으로_모은다() {
        let buf = [tag(4, 0), varint(1), tag(4, 0), varint(2)].concat();
        assert_eq!(
            fields(&buf)["#4"],
            json!([
                { "kind": "varint", "value": 1, "signed": -1 },
                { "kind": "varint", "value": 2, "signed": 1 }
            ])
        );
    }

    #[test]
    fn varint_문자열_중첩_반복_필드가_섞인_메시지를_해석한다() {
        let buf = [
            tag(1, 0),
            varint(150),
            len_delim(2, b"hello"),
            len_delim(3, &[tag(1, 0), varint(7)].concat()),
            tag(4, 0),
            varint(1),
            tag(4, 0),
            varint(2),
        ]
        .concat();
        let decoded = decode(&buf);
        let message = decoded.frames[0].fields.as_ref().unwrap();
        assert_eq!(message.keys().collect::<Vec<_>>(), ["#1", "#2", "#3", "#4"]);
        let fields = json(message);
        assert_eq!(fields["#1"]["value"], 150);
        assert_eq!(fields["#2"], json!({ "kind": "string", "value": "hello" }));
        assert_eq!(
            fields["#3"],
            json!({
                "kind": "message",
                "fields": { "#1": { "kind": "varint", "value": 7, "signed": -4 } }
            })
        );
        assert_eq!(fields["#4"].as_array().unwrap().len(), 2);
    }

    // gRPC-web framing

    #[test]
    fn 프레임_헤더를_떼고_모든_프레임을_해석한다() {
        let res = decode(&[data_frame(), trailer_frame()].concat());
        assert!(res.grpc_web);
        assert_eq!(res.frames.len(), 2);
        assert_eq!(res.frames[0].flag, Some(0));
        assert_eq!(
            json(&res.frames[0].fields),
            json!({ "#1": { "kind": "varint", "value": 150, "signed": 75 } })
        );
        let trailer = json(&res.frames[1]);
        assert_eq!(trailer["flag"], 1);
        assert_eq!(trailer["trailer"], true);
        assert_eq!(trailer["trailerText"], "grpc-status:0\r\n");
    }

    #[test]
    fn base64_grpc_web_text_페이로드도_똑같이_해석한다() {
        let engine = base64::engine::general_purpose::STANDARD;
        let b64 = engine.encode([data_frame(), trailer_frame()].concat());
        let res = decode(&engine.decode(b64).unwrap());
        assert!(res.grpc_web);
        assert_eq!(json(&res.frames[0].fields)["#1"]["value"], 150);
    }

    #[test]
    fn grpc_web이_명시적으로_false면_프레이밍을_그대로_둔다() {
        let options = ProtobufDecodeOptions {
            grpc_web: Some(false),
            ..Default::default()
        };
        let res = decode_protobuf(&data_frame(), options);
        assert!(!res.grpc_web);
        // the leading flag byte reads as field 0, which is invalid protobuf
        assert!(
            res.frames[0]
                .error
                .as_ref()
                .unwrap()
                .contains("zero field number")
        );
    }

    #[test]
    fn grpc_web을_강제했는데_프레임이_아니면_에러를_보고한다() {
        let options = ProtobufDecodeOptions {
            grpc_web: Some(true),
            ..Default::default()
        };
        let res = decode_protobuf(&[tag(1, 0), varint(150)].concat(), options);
        assert!(!res.grpc_web);
        assert!(
            res.frames[0]
                .error
                .as_ref()
                .unwrap()
                .contains("gRPC frames")
        );
    }

    #[test]
    fn 프레임_없는_protobuf_메시지에_split_grpc_frames는_none을_돌려준다() {
        assert_eq!(split_grpc_frames(&[tag(1, 0), varint(150)].concat()), None);
    }

    // malformed input

    #[test]
    fn 잘린_varint는_던지지_않고_에러로_보고한다() {
        let res = decode(&[tag(1, 0), vec![0x96]].concat());
        assert!(
            res.frames[0]
                .error
                .as_ref()
                .unwrap()
                .contains("truncated varint")
        );
    }

    #[test]
    fn 지원하지_않는_group_wire_type을_보고한다() {
        let res = decode(&tag(1, 3));
        assert!(
            res.frames[0]
                .error
                .as_ref()
                .unwrap()
                .contains("unsupported wire type 3")
        );
    }

    #[test]
    fn 빈_입력은_필드_없는_메시지로_취급한다() {
        assert_eq!(
            json(&decode(&[])),
            json!({ "grpcWeb": false, "byteLength": 0, "frames": [{ "byteLength": 0, "fields": {} }] })
        );
    }

    // Not in the TypeScript suite: pins the BigInt-width behaviour this port has to imitate.

    #[test]
    fn 열_바이트_varint는_64비트를_넘는_값과_반올림된_필드_키를_그대로_보여준다() {
        // Tag 2^69 (field 2^66, wire 0), then the largest ten-byte varint, 2^70 - 1.
        let tag = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40];
        let value = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
        assert_eq!(
            fields(&[tag, value].concat()),
            json!({
                // String(2 ** 66) in JavaScript
                "#73786976294838210000": {
                    "kind": "varint",
                    "value": "1180591620717411303423",
                    "signed": "-590295810358705651712"
                }
            })
        );
        let eleven = [[0x80; 10].to_vec(), vec![0x00]].concat();
        assert_eq!(
            decode(&eleven).frames[0].error.as_deref(),
            Some("varint too long")
        );
    }

    #[test]
    fn 아주_깊게_중첩된_입력은_max_depth가_커도_스택을_넘기지_않고_바이트로_끊는다() {
        // 5,000 levels of field 1 wrapping `08 01`; the TypeScript survived this
        // with maxDepth = MAX_SAFE_INTEGER by catching the stack overflow.
        let mut buf = vec![0x08, 0x01];
        for _ in 0..5_000 {
            buf = len_delim(1, &buf);
        }
        let options = ProtobufDecodeOptions {
            grpc_web: Some(false),
            max_depth: Some(usize::MAX),
        };
        let decoded = decode_protobuf(&buf, options);
        let mut current = decoded.frames.first().and_then(|f| f.fields.as_ref());
        let mut messages = 0;
        let mut innermost = None;
        while let Some(fields) = current {
            current = match fields.get("#1") {
                Some(ProtobufField::One(ProtobufValue::Message { fields })) => {
                    messages += 1;
                    Some(fields)
                }
                other => {
                    innermost = other;
                    None
                }
            };
        }
        // Chunks at depth 1..=99 decode as messages; the one at depth 100 does not.
        assert_eq!(messages, MAX_DEPTH_LIMIT - 1);
        assert!(matches!(
            innermost,
            Some(ProtobufField::One(ProtobufValue::Bytes { .. }))
        ));
    }

    #[test]
    fn 직렬화한_json이_타입스크립트_출력과_같다() {
        // fixed32 0.1f, fixed64 NaN, then an empty length-delimited field.
        let buf = [
            tag(5, 5),
            0.1f32.to_le_bytes().to_vec(),
            tag(6, 1),
            vec![0, 0, 0, 0, 0, 0, 0xf8, 0x7f],
            len_delim(2, &[]),
        ]
        .concat();
        // What JSON.stringify(decodeProtobuf(buf)) printed for these bytes.
        let from_typescript = r##"{"grpcWeb":false,"byteLength":16,"frames":[{"byteLength":16,"fields":{"#5":{"kind":"fixed32","hex":"0xcdcccc3d","uint32":1036831949,"float":0.10000000149011612},"#6":{"kind":"fixed64","hex":"0x000000000000f87f","uint64":"9221120237041090560","double":null},"#2":{"kind":"string","value":""}}}]}"##;
        assert_eq!(
            serde_json::to_string(&decode(&buf)).unwrap(),
            from_typescript
        );
    }
}
