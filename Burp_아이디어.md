# 🕷️ Burp Suite 참고 — rever-browser에 추가할 기능들

> 작성일: 2026-05-08
> 출처: 평가.md 후속 — Burp Suite 기능 매핑 + 추가 제안

---

## 먼저: Burp Suite 핵심 기능 지도

Burp의 기능 = 8개 메인 모듈:

| Burp 모듈 | 한 줄 요약 |
|----------|----------|
| **Proxy** | 모든 요청 가로채기 + 수정 후 통과 |
| **Repeater** ⭐ | 요청 1개 반복 전송 + 변형 실험 |
| **Intruder** ⭐ | 페이로드 자동 fuzzing (브루트포스, 인젝션) |
| **Scanner** | 자동 취약점 스캔 (SQLi, XSS 등) |
| **Sequencer** | 토큰 랜덤성 분석 (예측 가능한 세션 ID 탐지) |
| **Decoder** | base64/URL/hex/hash 변환 |
| **Comparer** | 두 응답 diff |
| **Collaborator** | OOB(Out-of-Band) 콜백 서버 |

rever-browser와 매핑:

| Burp | rever-browser 현재 | 갭 |
|------|-------------------|---|
| Proxy 가로채기 | `intercept.ts` 있음 ✅ | 일시정지/수정 UI 없음 |
| **Repeater** | ❌ | **진짜 비어있음** |
| **Intruder** | ❌ | **진짜 비어있음** |
| Scanner | ❌ | (비전 안 맞음, skip OK) |
| Sequencer | ❌ | (틈새, skip OK) |
| Decoder | `decode.ts` 있음 ✅ | base64만, 종류 적음 |
| **Comparer** | `diff.ts` 있음 🟡 | 페이로드 diff만, 응답 시각 diff 약함 |
| Collaborator | ❌ | (셀프호스팅 어려움, skip) |

---

## 🥇 우선순위 1순위 — Repeater (가장 큰 ROI)

### Burp Repeater란?
> 캡처한 요청 1개를 편집창에 띄워놓고, 헤더/바디/URL 바꿔가며 반복 전송. 응답을 옆에 즉시 표시.

리버싱의 일상 도구. **"이 헤더 빼면 서버가 어떻게 반응하지?", "토큰 만료시키면?", "user_id 바꾸면 남의 데이터 보이나?"** 전부 Repeater로 함.

### MCP 도구
```ts
// tools/repeater.ts
mcp.registerTool('repeater_send', {
  description: 'Replay a captured request with optional modifications. Returns response without affecting the page.',
  inputSchema: {
    requestId: z.string(),
    modifications: z.object({
      url: z.string().optional(),
      method: z.string().optional(),
      headers: z.record(z.string()).optional(),    // 덮어쓰기
      removeHeaders: z.array(z.string()).optional(), // 삭제
      body: z.string().optional(),
    }).optional(),
    repeat: z.number().min(1).max(100).default(1)
  }
})
```

구현: CDP `Network.loadNetworkResource` 또는 main 프로세스에서 fetch (브라우저 컨텍스트 쿠키 자동 첨부 위해 CDP 추천).

### UI
```
┌─ Repeater ────────────────────────────────┐
│  [요청 편집]              [응답]          │
│  POST /api/login          200 OK 245ms   │
│  Cookie: ...              {"token":"..."}│
│  {"id":"a","pw":"b"}                     │
│                                          │
│  [ Send ]  [ Send x10 ]                  │
└──────────────────────────────────────────┘
```
**우클릭 → "Send to Repeater"** 가 Burp 시그니처 UX.

### AI 활용 시나리오
> AI: "이 요청에서 Authorization 헤더 빼고 보내볼게요" → `repeater_send` → 401 확인 → "서버 측 인증 정상 동작"

→ AI가 **가설 검증을 자동화**. rever-browser의 진짜 무기.

---

## 🥈 우선순위 2순위 — Intruder (Fuzzer)

### Burp Intruder란?
> 요청에 `§...§` 마커를 박고, 페이로드 리스트를 자동 대입해 수백~수천 번 전송. 응답 길이/상태/시간으로 이상치 탐지.

### 4가지 공격 모드
1. **Sniper**: 마커 1개씩 순회, 다른 마커는 원본 유지
2. **Battering ram**: 모든 마커에 같은 페이로드
3. **Pitchfork**: 마커별 다른 리스트, 같은 인덱스끼리 묶음
4. **Cluster bomb**: 마커별 다른 리스트, 모든 조합 (카르테시안 곱)

### MCP 도구
```ts
mcp.registerTool('intruder_run', {
  description: 'Fuzz a request by substituting payloads into marked positions.',
  inputSchema: {
    requestId: z.string(),
    template: z.string().describe('Request template with §payload§ markers'),
    mode: z.enum(['sniper', 'battering_ram', 'pitchfork', 'cluster_bomb']),
    payloads: z.array(z.array(z.string())),
    concurrency: z.number().default(5),
    delayMs: z.number().default(0),
    grepResponse: z.string().optional()
  }
})
```

### AI 활용 시나리오
- **IDOR 탐지**: `user/§id§/profile` → 1~1000 → 응답 길이 다른 ID 색출
- **숨겨진 엔드포인트**: `/api/§word§` + 워드리스트 → 200만
- **비밀번호 정책 탐지**: 다양한 PW 패턴 → 어떤 게 통과

### ⚠️ 가드레일 필수
- **Rate limit 강제** (concurrency, delayMs 최솟값)
- **타겟 도메인 화이트리스트** (acceptedTargets 설정 파일)
- **요청 카운터 + 사용자 확인** (1000회 넘으면 confirm)
- **법적 disclaimer** (본인 사이트/Bug Bounty/CTF만)

→ Burp는 상용툴이라 풀어놨지만, rever-browser는 AI가 호출 = **자동 폭주 위험**. 이거 안 잡으면 사고 100%.

---

## 🥉 우선순위 3순위 — Comparer 강화

### 현재 `diff.ts` 한계
페이로드 단순 텍스트 diff.

### Burp Comparer 식 강화
```ts
mcp.registerTool('compare_responses', {
  inputSchema: {
    requestIdA: z.string(),
    requestIdB: z.string(),
    mode: z.enum(['words', 'bytes', 'json_structural', 'headers_only'])
  }
})
```

특히 **`json_structural` 모드**:
```
A: {"user":{"id":1,"name":"foo","secret":"abc"}}
B: {"user":{"id":1,"name":"foo"}}
diff: A에만 있는 키: user.secret  ← 권한별 응답 차이 자동 탐지
```

→ **권한 누설/IDOR 자동 탐지에 직결**.

---

## 🏅 우선순위 4순위 — Proxy Intercept UI

### Burp 핵심 UX
> "Intercept ON" 토글 → 요청이 서버 도달 전에 일시정지 → 사용자 편집 → "Forward" / "Drop"

### 현재 상태
`intercept.ts` 245줄 = CDP `Fetch.requestPaused` 기반 있음. UI에서 "지금 잡힌 요청 보기/편집/forward" 흐름 확인 필요.

### 추가 사항
- **사이드 패널**: "Pending intercepts (3)" 큐
- **Match & Replace 룰**: 정규식 자동 치환
- **Scope 제한**: 특정 도메인만 가로채기

```ts
mcp.registerTool('intercept_set_rule', {
  inputSchema: {
    pattern: z.string(),
    action: z.enum(['pause', 'modify', 'block']),
    modify: z.object({
      headers: z.record(z.string()).optional(),
      body: z.string().optional()
    }).optional()
  }
})
```

---

## 🎖️ 우선순위 5순위 — Decoder 확장

### Burp Decoder 항목 그대로 차용
```ts
mcp.registerTool('decode_smart', {
  description: 'Auto-detect and decode common encodings.',
  inputSchema: {
    input: z.string(),
    format: z.enum([
      'auto',
      'url', 'url_double',
      'html_entity',
      'base64', 'base64url',
      'hex',
      'jwt',
      'gzip', 'deflate', 'brotli',
      'unicode_escape',
    ]).default('auto')
  }
})
```

**Smart auto-detect**가 핵심: AI가 "이 문자열 뭔지 모르겠어요" 한 방에 해결.

---

## 🏆 추가 보너스 — Burp엔 없지만 AI 시대에 어울리는 것

### A. Auto-Replay on Token Refresh ⭐
JWT 만료 → 자동 갱신 후 실패한 요청 재시도. 리버싱 세션 끊김 방지.

### B. Request Recipe 저장 ⭐
"사이트 로그인 → 토큰 추출 → API 호출" 시퀀스를 리시피로 저장 → 한 번에 재실행. Postman Collection의 AI 친화 버전.

### C. GraphQL 인트로스펙션 자동화
```ts
mcp.registerTool('graphql_introspect', {
  description: 'Auto-detect GraphQL endpoints and run introspection query.'
})
```
→ schema 추출 + 쿼리 템플릿 생성. Burp엔 InQL 익스텐션 별도.

### D. 요청 크리티컬리티 자동 분류
```
AI가 트래픽 보면서 자동 태깅:
  - 🔴 인증 (Cookie/Authorization)
  - 🟠 결제 (price/amount/charge)
  - 🟡 PII (email/phone/ssn)
  - 🟢 정적 자원
```
→ 리스트 정렬/필터, "중요한 것만" 보기.

### E. 자동 PoC 생성
취약점 의심 → AI가 PoC HTML/curl/Python 자동 생성 → bug bounty 리포트 즉시 첨부.

---

## 📋 통합 로드맵

### Phase 1 — Burp 핵심 카피 (2주)
1. **Repeater** (도구 + UI)
2. **Intercept UI 강화** (Match & Replace)
3. **Decoder smart auto-detect**
4. **Comparer json_structural**

### Phase 2 — Burp Intruder (1~2주)
5. **Intruder 4 모드** + 안전 가드레일
6. **응답 분석 자동화** (status/length/time 그래프)

### Phase 3 — AI 차별화 (2주)
7. **Request Recipe** 저장/재생
8. **자동 크리티컬리티 분류**
9. **GraphQL 인트로스펙션**

### Phase 4 — 평가/안정화 (1주)
10. **Demo scenario 3개** end-to-end
11. **Eval suite** (AI 도구 사용 성공률)

---

## ⚠️ 강한 충고

**Burp 모든 걸 카피하지 마세요.** Burp는 20년 된 상용툴이라 기능이 많은데, rever-browser의 **차별점은 "AI가 알아서 한다"** 이지 "기능이 많다"가 아님.

**선택 기준:**
- ✅ **AI가 활용해서 가치 만드는 기능** (Repeater, Intruder, Comparer)
- ❌ **사람이 GUI로 만지는 게 본질인 기능** (Scanner GUI, Sequencer 그래프)

Repeater 하나만 잘 만들어도 product 가치 2배.

---

## 다음 액션 후보

- [ ] Repeater 구현 (MCP 도구 + UI)
- [ ] Intruder 안전 가드레일 설계
- [ ] Recipe 시스템 설계
- [ ] Demo scenario 3개 정의
