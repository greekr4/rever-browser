# rever-browser 리버싱 도구 추가 계획 (쉬운 버전)

> 한 줄 요약: **"트래픽 보기"는 잘 되는데, "그래서 Python 코드로 어떻게 재현해?"가 비어있다.**
> 사용자가 말한 콘솔/스크립트주입/소스맵보다 먼저 채워야 할 빈 자리가 따로 있음.

---

## 🟢 지금 잘 되어 있는 것

| 영역 | 도구 |
|---|---|
| 네트워크 캡처 | `list_requests`, `get_request` |
| 페이지 자동조작 | `navigate`, `click`, `type`, `scroll`, `screenshot` + AX 트리 `snapshot` |
| 일회성 JS 실행 | `browser_evaluate` |
| 정적 분석 | `list_scripts`, `grep_script(s)`, `extract_context`, `detect_bundler`, `deobfuscate_script` (webcrack) |
| 봇 우회 | `navigator.webdriver`/plugins/WebGL 위장, persist 파티션 |

---

## 🔴 비어있는 가장 큰 구멍 3가지

### 1. "이 요청을 누가 보냈나"를 모른다

지금은 `requestWillBeSent` 이벤트에서 **`initiator.stack`** (= "어느 JS 라인이 이 fetch를 호출했나")을 저장하지 않음.
- 영향: "검색 버튼 누르면 어떤 API가 나가는지" 자동 추적 불가 → AI가 매번 grep으로 헤매야 함.
- 비용: traffic-store에 5줄 추가.

### 2. 산출물이 없다

PLAN.md M0의 정의된 결과물은 **"Python 클라이언트 코드"**.
그런데 지금 도구로는:
- 쿠키 한 번에 못 뽑음
- localStorage의 JWT 못 뽑음
- "이 요청 그대로 requests로 만들어줘"가 없음

→ AI가 결과를 만들어야 하는데, 만들 재료가 없는 상태.

### 3. "검색 → 결과 보기"가 없다

페이로드끼리 비교(diff), JWT 디코드, base64 인식 — 매번 쓰는 데 도구가 없음. AI에게 매번 일일이 시켜야 함.

---

## ✅ 수정된 우선순위 (검증 에이전트 권장)

| 순위 | 기능 | 왜 | 작업량 |
|---|---|---|---|
| **1** | `initiator.stack` 캡처 | 가장 큰 누락. 5줄 | XS |
| **2** | `auth_dump` + `export_python_client` | M0 산출물 채우기 | M |
| **3** | `decode_token`, `request_diff`, `find_api_base` | 매번 쓰는 작은 도구 | S |
| **4** | **REPL 콘솔** (사용자 1순위) | 누적 디버깅 세션 | M |
| **5** | **소스맵 매핑** (사용자 3순위) | minified → 원본 file:line | M |
| **6** | **영구 스크립트 주입** (사용자 2순위) | host glob별 항상 주입 | M |
| 이후 | WebSocket 캡처 (M1 정합) | 실시간 사이트 절반 누락 | M |
| 이후 | Fetch 가로채기/리플레이 | 페이로드 변형 실험 | L |
| 이후 | Debugger 브레이크포인트 | 깊은 디버깅 | L |

---

## 🛠️ 사용자 우선 3가지 — 어떻게 만들지

### (1) REPL 콘솔
- DevTools 콘솔처럼 **세션 누적**, `await` 지원, `console.log`/exception 캡처
- 현재 `browser_evaluate` 와 다른 점:
  - `Runtime.evaluate { replMode: true, returnByValue: false }` → `objectId` 핸들 보존 → 다음 호출에서 재참조
  - `Runtime.consoleAPICalled` / `Runtime.exceptionThrown` 구독 (지금은 `Runtime.enable` 자체가 안 됨 → 활성화 필요)
- UI: 챗 패널 옆 또는 하단에 토글 가능한 콘솔 (xterm.js or `<pre>`)

### (2) 영구/일회성 스크립트 주입
- **영구**: `Page.addScriptToEvaluateOnNewDocument` (stealth와 같은 메커니즘) + host glob + electron-store
- **일회성**: 기존 `browser_evaluate`
- **라이브 패치**: `Debugger.setScriptSource` (이미 로드된 함수 본문 교체 — DevTools "Local Overrides" 가벼운 버전)
- UI: 좌측 사이드바 "Snippets" 탭 — host glob, Monaco 에디터, enable 토글

### (3) 소스맵 매핑 뷰어
- 본문 끝의 `//# sourceMappingURL=` 자동 파싱 → `.map` lazy fetch → **`@jridgewell/trace-mapping`** 으로 매핑
- traffic-store에 `parsedSourceMap` 캐시 (raw가 아니라 파싱 결과)
- MCP:
  - `resolve_source({ requestId, byteOffset })` → `{ source, line, column, name, snippet }`
  - `list_sources({ requestId })`
  - `get_original_source({ requestId, sourceIndex })`
- 효과: `grep_script` 결과에 자동으로 "이건 원래 `~/src/auth/sign.ts:42`" 가 붙음

---

## 📦 추가하면 좋은 외부 라이브러리

| 라이브러리 | 용도 | 난이도 | 가치 |
|---|---|---|---|
| `@jridgewell/trace-mapping` | 소스맵 매핑 | ★ | ★★★★★ |
| `acorn` + `acorn-walk` | AST grep | ★★ | ★★★★ |
| `monaco-editor` | snippet/console 에디터 | ★★ | ★★★★ |
| `har-format` | HAR I/O | ★ | ★★★ |

---

## 🚨 검증 에이전트가 잡아낸 내 실수

1. **WebSocket 비판은 부당** — PLAN.md M1로 의도적 deferral인데 로드맵 무시하고 깠음
2. **CDP 도메인** — REPL/디버거 BP는 `Runtime.enable`/`Debugger.enable`/`Fetch.enable` 선행 필요 (지금 `Network`/`Page`만 enable)
3. **우선순위 — auth+codegen이 1순위였어야** — PLAN M0의 정의된 산출물인데 후순위로 밀었음

> 종합 점수 **7/10** — "도구 카탈로그는 풍부한데 product 정의서를 안 읽고 짠 우선순위"

---

## 👉 다음 액션 후보

- [ ] `initiator.stack` traffic-store 캡처 추가 (5분)
- [ ] `auth_dump` MCP 도구
- [ ] `export_python_client` MCP 도구
- [ ] REPL 콘솔 패널 + `Runtime.enable` + console/exception 구독
- [ ] 소스맵 파서 + `resolve_source` 도구
- [ ] persistent script injector + UI

어디부터 시작할지 결정 필요.
