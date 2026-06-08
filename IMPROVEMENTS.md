# rever-browser 개선 로드맵 (진단 로그)

> 5분 루프(`/loop`, cron `*/5 * * * *`)가 갱신하는 누적 진단 문서.
> 새 분석은 위쪽에 날짜 헤더로 추가한다. 이전 항목의 진척은 체크박스로 갱신한다.

---

## 2026-06-08 — 2차 진단

**갱신**: 1차 항목 1(WebGL 동적화)은 **이미 구현됨** 확인 → `chrome-cdp.ts:277-294`. HANDOFF가 stale. 체크박스 [x] 처리.
**신규 커밋**: 없음(같은 세션). 진척 없음.
**이번 초점**: `traffic-store.ts` 정독 → 메모리 관련 실제 결함 2건 + god-file 1건 도출.

### 🟡 안정성 — 새 항목

- [x] **6. `wsFrames` Map 메모리 누수 (실제 버그)** ✅ 수정 (TDD, 테스트 2건 추가)
  - `evictIfNeeded()`에서 evict 시 `wsFrames.delete(oldest)` 동반 → 요청 churn 시 프레임 해제.
  - `appendWsFrame`에 `MAX_WS_FRAMES_PER_REQUEST=2000` 상한 → 장수명 소켓도 무한 증가 불가.
  - `traffic-store.test.ts`: per-request cap + evict 시 프레임 해제 두 케이스 검증.

- [x] **7. response body 무제한 누적** ✅ 수정 (TDD, 테스트 2건) — **8MB 캡 채택**
  - `upsertRequest`에 본문당 `MAX_BODY_CHARS = 8MB` 캡 + `responseBodyTruncated` 플래그.
  - 8MB는 webcrack 5MB 한계 **위** → 대형 JS 번들 분석(이 도구의 핵심 용도) 보존하면서 병적 케이스만 차단.
    진단 제안 1–2MB는 번들 분석을 깨뜨려 채택하지 않음(사용자 확인).
  - `traffic-store.test.ts`: 초과 시 truncate+flag / 정상 본문 무변경 검증.

### 🟢 유지보수 — 새 항목

- [x] **8. `chrome-cdp.ts` god-file 분해** ✅ 수정 (순수 이동, 동작 불변)
  - `STEALTH_INIT_SCRIPT` + `pickWebGLIdentity` + `SPOOFED_CHROME_*` 상수를 `src/main/stealth-init.ts`로 추출.
  - chrome-cdp.ts는 그 3개 심볼을 import해 사용(setUserAgentOverride는 상수 재사용). chrome-cdp 55,591자 → 23,800자(절반 이하).
  - **STEALTH 스크립트 본문 byte-identical 검증**(git HEAD diff) — 봇 우회 동작 변화 0. typecheck/test/build 통과.

---

## 2026-06-08 — 1차 진단

**현재 규모**: ~14,500 LOC · MCP 도구 29종 · 패널 5종 · ACP 에이전트 + stealth 봇 우회 레이어.
**검증**: `bun run typecheck` → 통과(exit 0). HANDOFF 1순위였던 "typecheck 미실행"은 닫음.

### 🔴 즉시 착수 권장 (프로젝트 정체성 = 봇 우회에 직결)

- [x] **1. WebGL 스푸핑 동적화 — 실제 버그** ✅ 이미 구현됨 (2차 진단에서 확인)
  - `src/main/chrome-cdp.ts:277-294` `pickWebGLIdentity()`가 이미 `process.arch === 'arm64'`면
    `ANGLE (Apple, ANGLE Metal Renderer: Apple M2 ...)`로 분기. HANDOFF 38–46행이 stale했던 것.
  - 잔여: 실제 Apple Silicon에서 Google 캡챠 재발 여부 모니터링만 남음.

- [x] **2. 실제 Chrome 쿠키 import** ✅ 구현 (`chrome-cookie-import.ts`, macOS)
  - `security` Keychain → PBKDF2-SHA1 → AES-128-CBC(IV=공백16) → 32B 도메인해시 strip → `persist:rever` 주입.
  - system `sqlite3` CLI로 Cookies DB 읽음(신규 npm 의존성 0). CookiesPanel "Import from Chrome" UI(프로필+도메인 필터).
  - 실측: 이 머신 4,510 쿠키 전부 `v10`(복호화 가능), 샘플 8/8 32B prefix 확인. v20(app-bound)는 보고만 하고 위조 안 함.
  - 미검증: 실제 주입(앱 실행)은 이 환경 밖 — 최초 실행 시 Keychain 허용 프롬프트 필요.

### 🟡 중기 — 안정성/신뢰성

- [x] **3. 테스트 0 → 회귀 안전망 (vitest)** ✅ 구현 (`vitest@4.1.8`, `bun run test` 39 passed)
  - `vitest.config.ts` + `package.json` `"test": "vitest run"`.
  - `traffic-store.test.ts`(ring buffer·필터·eviction·ws·console·exception + 항목6 누수 회귀),
    `script-analysis.test.ts`(grepBody·detectBundler·listScripts), `format-json.test.ts`.

- [x] **4. Agent permission IPC 라우팅** ✅ 배선 완료 (실행 미검증)
  - main `acp-session.requestPermission` → correlation-id로 renderer 왕복(`acp:permission-request`/`-respond`).
  - `PermissionPrompt` 오버레이(Enter=allow/Esc=reject) + ChatPanel Auto-approve 토글. 기본 auto-approve=true 유지(기존 동작 불변).
  - main 65초 가드 타임아웃(렌더러 60초보다 김) → UI 부재 시에도 에이전트 deadlock 불가(auto-approve fallback).
  - 배선·타입은 검증(typecheck/build)됐으나 **앱 실행 중 왕복은 미검증**. 수동 확인: ChatPanel에서
    Auto-approve를 끄고 → 에이전트에 도구 실행 시키고 → `PermissionPrompt`가 뜨고 Enter/Esc/클릭으로
    해소되는지 확인. (핸들러는 App mount의 useEffect에서 등록되고 에이전트는 ChatPanel 상호작용 이후
    spawn되므로 등록 누락 윈도우 없음.)

### 🟢 낮은 우선순위 (관점)

- [x] **5. findings 중심 워크플로우 강화** ✅ 구현
  - `userData/findings.json` 디스크 영속화(재시작 생존) + `category`(endpoint/auth/vuln/secret/other) 구조화.
  - `finding_export` 도구 신설 — 카테고리별·심각도순 Markdown 리포트(세션 산출물). `finding_list`에 category 필터.

---

<!-- 다음 루프 반복은 이 위에 새 날짜 섹션을 추가하세요. -->
