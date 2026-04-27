# rever-browser

웹 API 리버싱 전용 에이전트 데스크톱 앱.
사용자의 실제 브라우저 세션을 AI 에이전트가 옆에서 보며 분석/조작하는 양방향 워크플로우를 제공한다.

## 비전 (한 줄)

> "사용자가 브라우저에서 한 일 → 챗으로 분석" + "챗으로 시킨 일 → 브라우저가 실행" 두 방향이 같은 채널에서 매끄럽게 동작하는 도구.

## MVP 범위 (M0)

성공 기준 = 다음 2개 시나리오가 다나와에서 끝까지 동작.

1. **수동 → 분석**
   사용자가 다나와에서 검색 → 챗에 "방금 검색 API 분석해줘" → 에이전트가 트래픽 리스트에서 API 식별 → 응답 구조 설명 + Python 클라이언트 코드 생성

2. **자동 분석**
   챗에 "다나와에서 노트북 검색해보고 분석해" → 에이전트가 MCP 도구로 브라우저 조작 → 캡처된 트래픽 분석 → 동일한 산출물

### M0 포함

- 외부 Chrome (`--remote-debugging-port`) 연결
- ACP 에이전트 1종(Claude Code) 우선
- HTTP/HTTPS 트래픽 캡처 (Network 도메인)
- 트래픽 리스트/상세 뷰어
- 스트리밍 챗 응답 (Vercel AI SDK 6 패턴)
- 시스템 프롬프트: API 리버싱 전용

### M0 명시적 제외

- 멀티 에이전트 동시 운용 → M1
- WebSocket 캡처 → M1
- HAR 영구 저장/세션 관리 → M1
- Codex / Gemini 지원 → M1
- 인증/세션 자동 재생 → M2
- Chrome 임베딩 (외부 창 → 한 창 통합) → M2 이후

## 아키텍처

```
┌──────────────── Tauri 앱 창 ────────────────┐    ┌── 외부 Chrome ──┐
│                                             │    │                  │
│  좌측 패널                  우측 패널        │    │  사용자가 직접   │
│  ┌─────────────┐           ┌─────────────┐ │    │  조작 또는       │
│  │ 트래픽 리스트│           │ ChatPanel   │ │    │  에이전트가      │
│  │ + 검색      │           │ (ACP)       │ │    │  자동 조작       │
│  │             │           │             │ │    │                  │
│  │ 트래픽 상세 │           │ - claude    │ │    │  (--remote-      │
│  │  - req/res  │           │ - codex(M1) │ │    │   debugging-     │
│  │  - 헤더/바디│           │ - gemini(M1)│ │    │   port=9222)     │
│  └─────────────┘           └─────────────┘ │    └────────┬─────────┘
│         ↑                          ↓        │             │
│         │  Tauri events           │ stdio   │             │
│         │  (network capture)      │ ndJSON  │             │
│  ┌──────┴───────────┐    ┌────────┴─────┐  │             │
│  │ Rust: CDP 클라이언트 │    │ ACP transport │  │             │
│  │  + Chrome 런처     │    │ + spawn agent │  │             │
│  └──────┬───────────┘    └────────┬─────┘  │             │
│         │ WebSocket CDP           │         │             │
│         └─────────────────────────┼─────────┼─────────────┘
│                                   │         │
│  ┌────────────────────────────────┴──────┐ │
│  │ HTTP MCP 서버 (앱 내장, 127.0.0.1)    │ │
│  │ - list_requests / get_request         │ │
│  │ - browser_navigate / click / eval     │ │
│  │ - search_requests / save_har          │ │
│  └───────────────────────────────────────┘ │
│            ↑                                │
│            │ ACP 에이전트가 newSession 시   │
│            │ mcpServers로 등록 → 도구 호출   │
└─────────────────────────────────────────────┘
```

핵심 통신 흐름:
1. Rust가 Chrome 런칭 + CDP WebSocket 연결
2. Network 도메인 이벤트(`requestWillBeSent`, `responseReceived`, `loadingFinished`)를 Tauri event로 프론트에 푸시
3. 프론트는 트래픽 store에 누적 + 리스트 렌더
4. 사용자가 챗에 메시지 → ACP transport가 stdio로 에이전트 호출
5. 에이전트는 우리가 띄운 HTTP MCP 서버 도구를 호출 → 트래픽 조회 + 브라우저 명령
6. 브라우저 명령은 다시 CDP로 Rust → Chrome 실행

## 디렉토리 구조 (계획)

```
rever-browser/
├── PLAN.md                       # this
├── README.md
├── package.json
├── src/                          # React 19 + Vite frontend
│   ├── App.tsx                   # 2분할 레이아웃 셸
│   ├── main.tsx
│   ├── components/
│   │   ├── chat/
│   │   │   ├── ChatPanel.tsx     # career-pencil ChatPanel 패턴 포팅
│   │   │   ├── ChatInput.tsx
│   │   │   ├── ProviderSelect.tsx
│   │   │   └── ACPPermissionDialog.tsx
│   │   ├── browser/
│   │   │   ├── BrowserStatusBar.tsx   # 연결/URL 표시
│   │   │   └── BrowserControls.tsx    # 시작/정지/주소 입력
│   │   └── network/
│   │       ├── TrafficList.tsx        # 캡처된 요청 목록
│   │       ├── TrafficDetail.tsx      # 헤더/바디/응답
│   │       └── TrafficSearch.tsx
│   ├── hooks/
│   │   ├── use-chat.ts                # @ai-sdk/react useChat 래퍼
│   │   ├── use-cdp-events.ts          # Tauri event 구독
│   │   └── use-traffic-store.ts
│   ├── ai/
│   │   ├── acp-transport.ts           # career-pencil에서 이식
│   │   ├── acp-permission.ts          # career-pencil에서 이식
│   │   ├── acp-map-update.ts          # career-pencil에서 이식
│   │   └── system-prompt.md           # API 리버싱 전용 신규 작성
│   ├── automation/
│   │   ├── mcp-server.ts              # esbuild 번들 → src-tauri/resources
│   │   └── tools/
│   │       ├── list-requests.ts
│   │       ├── get-request.ts
│   │       ├── browser-navigate.ts
│   │       ├── browser-click.ts
│   │       ├── browser-eval-js.ts
│   │       └── search-requests.ts
│   ├── stores/
│   │   ├── traffic.ts                 # 캡처된 요청 상태
│   │   └── browser.ts                 # 연결 상태, 현재 URL
│   ├── constants.ts                   # ACP_AGENTS 등
│   └── types/
│       └── traffic.ts
└── src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── resources/
    │   └── mcp-server.mjs             # esbuild 산출물 (M0.4에서 빌드)
    └── src/
        ├── main.rs
        ├── lib.rs                     # Tauri 셋업, 명령 등록
        ├── chrome.rs                  # 런처 + 프로세스 관리
        ├── cdp.rs                     # WebSocket 클라이언트 + Network 이벤트 → Tauri event
        └── commands.rs                # JS에서 invoke하는 Tauri 명령
```

## 의사결정 로그

| ID | 결정 | 사유 | 대안 |
|----|------|------|------|
| D1 | 외부 Chrome + CDP | 풀 CDP, career-pencil의 figma debug 패턴 동일, MVP 빠르게 검증 | Tauri WebView (CDP 미지원), CEF (빌드 복잡) |
| D2 | React 19 + Vite + Bun + Tauri 2 | 본인 React/Next.js 친숙도 + 생태계(@ai-sdk/react, shadcn/ui). Tauri 데스크톱은 SSR 불필요 → Next.js 대신 Vite | Vue 3 (career-pencil 코드 재사용 이점은 ACP TS 로직 한정으로 약함) |
| D3 | ACP 에이전트 stdio spawn | career-pencil의 검증된 패턴 그대로 | HTTP-only LLM API (멀티 에이전트 어려움) |
| D4 | 앱 내장 HTTP MCP 서버 | 에이전트가 우리 도구를 호출하는 표준 채널 | 에이전트에 직접 도구 주입 (비표준) |
| D5 | M0은 Claude Code만 | 검증 폭 축소, 작동 후 확장 | 셋 다 동시 (테스트 비용 ↑) |

## 마일스톤

| # | 작업 | 예상 시간 | 결과물 |
|---|------|-----------|--------|
| M0.1 | PLAN.md + 폴더 스켈레톤 | 30분 | this |
| M0.2 | ACP transport 이식 + 기본 챗 UI | 4~6시간 | 챗 패널에서 Claude Code와 대화 가능 |
| M0.3 | Rust CDP 런처 + 캡처 → Tauri event | 6~8시간 | 트래픽 리스트가 실시간으로 업데이트 |
| M0.4 | MCP 서버 + 브라우저 도구 6개 | 4~6시간 | 에이전트가 도구로 트래픽/브라우저 조작 |
| M0.5 | 2분할 레이아웃 + 트래픽 뷰어 컴포넌트 | 3~4시간 | UI 완성 |
| M0.6 | E2E 시나리오 2개 다나와에서 검증 | 2시간 | 데모 가능 |

총합: **약 1.5~2주 (반일 기준 ~10일)**

## 리스크 & 대응

| 리스크 | 영향 | 대응 |
|--------|------|------|
| CDP 응답 본문 별도 호출 (`Network.getResponseBody`) 비용 | 트래픽 多 시 느려짐 | 본문은 lazy fetch — 사용자가 상세 클릭 시 또는 에이전트가 요청 시만 |
| Chrome 런칭 시 사용자 프로필 분리 필요 | 기본 프로필 간섭 위험 | 전용 user-data-dir 사용 |
| 에이전트가 너무 많은 도구 호출 | 토큰 낭비 | system prompt에 트래픽 페이지네이션 가이드 명시 |
| 봇 탐지 (네이버 등) | 자동 조작 시나리오 실패 | 사용자 본인 세션 사용 + Stealth 미사용 (MVP는 양심적 사용) |

## 다음 액션

M0.2 — `acp-transport.ts` 이식 + 의존성 추가 + 시스템 프롬프트 신규 작성.
