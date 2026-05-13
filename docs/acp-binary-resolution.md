# ACP 바이너리 이름 해결 (Claude Code)

## 배경

`AgentPicker`에서 Claude Code 타일이 "NOT INSTALLED"로 표시되는 문제 발생 (Windows 환경).

원인: `src/renderer/src/constants.ts`의 `claude-code` 항목이 `command: 'claude-agent-acp'` 하나만 탐색했지만,
사용자가 `npm i -g @zed-industries/claude-code-acp` (installHint와 동일)로 설치한 경우 실제 PATH에 깔리는 바이너리는 `claude-code-acp` 였음.

## 패키지/바이너리 매핑

| npm 패키지 | 설치되는 bin | 상태 |
|---|---|---|
| `@zed-industries/claude-code-acp` | `claude-code-acp` | **deprecated** (0.16.2부터 rename 안내) |
| `@agentclientprotocol/claude-agent-acp` | `claude-agent-acp` | 현재 권장 |

> `npm view @zed-industries/claude-code-acp` 실행 시 아래 경고가 뜸:
> _"This package has been renamed to @agentclientprotocol/claude-agent-acp. Please migrate to continue receiving updates."_

## 수정 내용 (Windows 검증 완료)

`src/renderer/src/constants.ts` — `claude-code` 엔트리:

```ts
{
  id: 'claude-code',
  name: 'Claude Code',
  command: 'claude-agent-acp',        // 신 패키지 우선
  fallbackBins: ['claude-code-acp'],  // 구 패키지 (deprecated)도 인식
  args: [],
  acpSupported: true,
  installHint: 'npm i -g @agentclientprotocol/claude-agent-acp',
  icon: 'C'
}
```

탐색 로직 (`acp-detect.ts`)은 `command` → `fallbackBins` 순으로 PATH/PATHEXT를 훑으므로, 사용자가 두 패키지 중 어느 쪽을 설치했든 detection이 성공한다.

## macOS / Linux에서 동작 안 할 가능성

다음 경우 detection이 실패할 수 있다:

1. **npm 전역 prefix가 Electron PATH에 없음** — `acp-detect.ts`가 `npm prefix -g`를 한 번 호출해 보강하지만, nvm/asdf/volta 같은 버전 매니저 환경에서 `npm.cmd`가 아닌 `npm`을 못 찾으면 실패함. 사용자가 zsh/bash 프로필에 PATH를 정의해 둔 경우 Electron이 그 프로필을 로드하지 않을 수 있음.
2. **Homebrew로 깐 node** — `/opt/homebrew/bin`은 `extraDirs()`에 이미 포함됨. 다만 `~/.nvm/versions/node/vXX/bin`은 명시 안 됨.
3. **shebang/symlink 차이** — Linux/Mac은 `constants.X_OK`로 실행 권한을 체크함. 일부 환경(예: 마운트된 FS)에서 권한 비트가 빠지면 detection이 실패할 수 있음.

## 롤백 절차

이 커밋(이번 fix)만 되돌리면 됨:

```bash
git revert <이번 fix 커밋 해시>
```

수동 롤백:

```ts
// constants.ts 의 claude-code 엔트리에서
command: 'claude-agent-acp',
fallbackBins: ['claude-code-acp'],
// ↓
command: 'claude-agent-acp',
// (fallbackBins 줄 삭제)
```

> 단순히 신 패키지로만 통일하고 싶다면 위 롤백 + 사용자에게 `npm i -g @agentclientprotocol/claude-agent-acp` 안내로 충분.

## macOS에서 깨졌을 때 분기처리 옵션

### 옵션 A — `extraDirs()`에 nvm 경로 추가

`src/main/acp-detect.ts` `extraDirs()`의 비-Windows 분기에 nvm 현재 버전 bin을 추가:

```ts
return [
  '/opt/homebrew/bin',
  '/usr/local/bin',
  '/usr/bin',
  join(home, '.local', 'bin'),
  join(home, '.bun', 'bin'),
  join(home, '.cargo', 'bin'),
  join(home, '.npm-global', 'bin'),
  // 추가 후보
  ...(process.env.NVM_BIN ? [process.env.NVM_BIN] : []),
  ...(process.env.VOLTA_HOME ? [join(process.env.VOLTA_HOME, 'bin')] : [])
]
```

이 변경은 Windows에 영향 없음.

### 옵션 B — 플랫폼별 `command` 분기 (최후의 수단)

`ACP_AGENTS` 엔트리에 `commandByPlatform` 같은 필드를 새로 만들고, `acp-detect.ts`에서 `process.platform`에 따라 선택. 권장하지 않음 (npm 패키지 동일하면 바이너리도 동일하므로 분기 불필요. PATH 문제는 옵션 A로 해결되는 게 정상).

### 옵션 C — 사용자에게 직접 경로 입력 받기

`AgentPicker`에 "Browse…" 버튼 추가해 detection 실패해도 수동 경로 등록 가능하게 함. PR 단위로 분리해서 진행.

## 검증 체크리스트

- [x] Windows: `npm i -g @zed-industries/claude-code-acp` → `claude-code-acp.cmd` PATH 등록 확인
- [x] Windows: `npm i -g @agentclientprotocol/claude-agent-acp` → `claude-agent-acp.cmd` PATH 등록 확인
- [ ] macOS (Intel/Apple Silicon)
- [ ] Linux (Ubuntu/Debian)
- [ ] nvm 사용 환경
