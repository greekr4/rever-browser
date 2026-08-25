# WASM 분석

> English: [`wasm-analysis.md`](./wasm-analysis.md)

WebAssembly를 JavaScript처럼 읽습니다. 사이트가 요청 서명을 `.wasm` 모듈
(Rust / C / Go / AssemblyScript 컴파일) 안에서 계산하면, **Web Crypto API**만
보는 `crypto_trace` 도구는 아무것도 보지 못합니다. 이 기능이 그 공백을 메웁니다:
이미 캡처된 `.wasm` body를 읽을 수 있는 텍스트로 바꾸고, export를 그것을 호출하는
JS와 연결합니다.

재다운로드도, 별도 캡처 경로도 없습니다 — CDP 계층이 `traffic-store`에 이미
저장해 둔 `.wasm` 바이트를 그대로 읽습니다.

## 도구

다섯 개 모두 MCP 도구입니다. 셸에서 `scripts/mcp-call.py`로 구동하세요.

| 도구 | 하는 일 |
|---|---|
| `list_wasm` | 캡처된 모듈(`application/wasm` body 또는 `.wasm` URL)을 큰 것부터 나열. 다른 도구에 필요한 `requestId`를 줍니다. |
| `wasm_decompile` | 모듈 하나를 텍스트로 디스어셈블. `format:"wat"`(기본) / `"decompile"`(C 유사) / `"c"`(wasm2c 완전 C 소스). |
| `wasm_info` | `wasm-objdump -x` 요약: 타입, import, export, 그리고 name 섹션이 있으면 실제 함수/심볼 이름. |
| `grep_wasm` | body의 `strings` 스캔 — 알고리즘 식별자, 내장 키, URL, 심볼 이름. 정규식 필터 선택 가능. |
| `wasm_xref` | WASM ↔ JS 연결: 각 export 이름으로 캡처된 스크립트를 grep해 어디서 호출되는지 표시. |

### `wasm_decompile` 출력 단계

같은 바이트, 세 가지 가독성 수준:

- **`wat`** — 보장되는 baseline. `wabt` JS API로 **in-process** 실행되어, `wabt`만
  설치돼 있으면 항상 사용 가능. 서브프로세스도 임시 파일도 없음.
- **`decompile`** — 더 상위 수준의 C 유사 뷰(`wasm-decompile`).
- **`c`** — 완전한 C 소스(`wasm2c`). 복잡한 암호/서명 루틴에 가장 읽기 쉬움.

`decompile`과 `c`는 번들된 `wabt` 바이너리로 shell-out 합니다. 그 경로를 쓸 수
없으면 오류 대신 **WAT로 안전하게 degrade** 합니다.

> `full:true`는 `format:"decompile"`의 deprecated 별칭으로 유지됩니다.

## 빠른 시작

```bash
# 1. .wasm을 가져오는 페이지를 로드 (바이트가 캡처됨)
python3 scripts/mcp-call.py browser_navigate '{"url":"https://example.com/app"}'

# 2. 모듈 찾기
python3 scripts/mcp-call.py list_wasm
#   -> [{ "requestId": "123.4", "url": ".../sign.wasm", "bytes": 4096, ... }]

# 3. 먼저 구조 파악
python3 scripts/mcp-call.py wasm_info   '{"requestId":"123.4"}'

# 4. 로직 읽기 (단계 선택)
python3 scripts/mcp-call.py wasm_decompile '{"requestId":"123.4","format":"c"}'

# 5. 상수 훑기 + JS 호출부 찾기
python3 scripts/mcp-call.py grep_wasm '{"requestId":"123.4","pattern":"HMAC|sign|key"}'
python3 scripts/mcp-call.py wasm_xref  '{"requestId":"123.4"}'
```

## 이 기능을 가능하게 한 캡처 수정

보통은 CDP가 응답 body를 넘겨주고 앱이 base64 플래그를 그대로 유지하므로 `.wasm`
캡처는 이미 잘 됩니다. 하지만 body가 **캐시나 서비스 워커에서** 서빙되면 CDP에는
사본이 없어 앱이 URL을 재요청합니다. 그 fallback(`chrome-cdp.ts`의
`refetchBody`)이 예전엔 모든 body를 UTF-8로 강제 디코드했는데 — 이게 `\0asm`
매직과 모든 비 UTF-8 바이트를 망가뜨려, 캐시로 전달된 `.wasm`을 디코드 불가로
만들었습니다.

수정: `encodeRefetchedBody`가 바이너리 MIME 타입(`application/wasm`,
`application/octet-stream`, image/video/audio/font)은 base64로 인코딩하고,
텍스트는 UTF-8로 둡니다. 서비스 워커 캐시에서 `.wasm`을 서빙하는 실제 사이트도
이제 정확히 디코드됩니다.

## 의존성: `wabt`

`wabt`는 PATH 바이너리가 아니라 **npm devDependency**입니다:

```bash
bun add -d wabt
```

- WAT는 in-process JS API로 실행됩니다.
- `decompile` / `c` / `wasm_info`는 Electron-as-node(`ELECTRON_RUN_AS_NODE=1`)로
  `node_modules/wabt/bin/*`를 spawn 하므로, 외부 `node`가 필요 없습니다.
- 따라서 "wabt 없음"은 ENOENT가 아니라 `import('wabt')` 실패를 뜻합니다 — 도구는
  `bun add -d wabt` 힌트를 반환하고 절대 크래시하지 않습니다.

서브프로세스 경로 제한: 타임아웃 30,000밀리초, 출력 상한 5,242,880바이트.

## 내부 구조

- `src/main/mcp/tools/wasm.ts` — 다섯 개 MCP 도구 등록.
- `src/main/mcp/wasm-analysis.ts` — 순수(electron 비의존) 헬퍼(vitest에서 실행
  가능): `encodeRefetchedBody`, `listWasm`, `getWasmBuffer`, `wasmToWat`,
  `runWabtBin` + `wasmDecompileFull` / `wasmToC` / `wasmObjdump`,
  `extractWasmStrings`, `parseWatExports`, `xrefExports`, `decompileRequest`.
- `src/main/chrome-cdp.ts` — `refetchBody` 수정을 위해 `encodeRefetchedBody`를
  import.

## 테스트

- 단위: `bun run test` (`src/main/mcp/wasm-analysis.test.ts`).
- 라이브: 픽스처를 띄우고 도구를 구동 —
  [`agent-testing.md`](./agent-testing.md) 참고. `wasm-target.html` 픽스처
  (포트 8779)는 외부 `/wasm-caller.js`를 통해 `/sign.wasm`(export `checksum`)을
  로드하며, 서비스 워커 프리캐시되어 재로드 시 `refetchBody` 수정을 검증합니다.

## 범위

Phase 1은 읽기/분석 전용입니다. 의도적으로 제외: name 섹션을 넘어선 심볼 복구,
더 깊은 WASM↔JS 데이터플로우, `crypto_trace` 통합 플래그.
