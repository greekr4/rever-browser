import { delimiter, dirname, isAbsolute } from 'node:path'

import { extraDirs } from './acp-detect'
import { findClaudeCli } from './claude-cli'

// Claude Code CLI가 부모 프로세스에서 물려받은 CLAUDECODE / CLAUDE_CODE_* 변수를
// 보고 "nested session"으로 판단해 기동을 거부한다 (rever-browser 자체를 Claude
// Code 세션 안에서 실행한 경우). 에이전트 자식 프로세스는 독립 세션이어야 하므로
// 해당 변수들을 제거한 env를 만들어 넘긴다.
export function agentEnv(command: string): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env }
  // CLAUDE_CODE_EXECUTABLE is not a session marker — it tells claude-agent-acp
  // which Claude Code binary to spawn, and must survive the scrub below.
  const isClaudeSessionVar = (name: string): boolean =>
    name !== 'CLAUDE_CODE_EXECUTABLE' && (name === 'CLAUDECODE' || name.startsWith('CLAUDE_CODE_'))
  if (process.platform === 'win32') {
    // Windows는 환경변수 이름이 대소문자를 구분하지 않아 'ClaudeCode' 같은
    // 변형 표기로도 상속될 수 있다. 대문자로 정규화해 비교한다.
    for (const key of Object.keys(env)) {
      if (isClaudeSessionVar(key.toUpperCase())) delete env[key]
    }
  } else {
    for (const key of Object.keys(env)) {
      if (isClaudeSessionVar(key)) delete env[key]
    }
  }

  // Finder에서 실행된 패키지 앱은 로그인 셸의 PATH를 물려받지 못해
  // `/usr/bin:/bin`만 남는다. 이러면 에이전트 바이너리(절대경로로 넘어옴)의
  // `#!/usr/bin/env node` shebang이 node를 못 찾아 spawn이 즉시 죽는다.
  // node는 보통 에이전트 바이너리와 같은 bin/에 있으므로(nvm/volta) 그 디렉터리와
  // 흔한 설치 위치들을 PATH 앞에 붙여 shebang이 항상 해석되게 한다.
  // Without this, claude-agent-acp runs the Claude Code CLI bundled in its SDK
  // dependency (2.1.83 as of acp 0.23.x), which the API refuses for Claude 5
  // models ("version 2.1.280 or newer is required") and whose `default`
  // alias is still Opus 4.6. Prefer the user's installed CLI; an explicit
  // CLAUDE_CODE_EXECUTABLE in the environment always wins.
  if (!env.CLAUDE_CODE_EXECUTABLE) {
    const cli = findClaudeCli()
    if (cli) env.CLAUDE_CODE_EXECUTABLE = cli
  }

  // claude-agent-acp 세션의 기본 모델이 Claude 5 계열로 결정되면 API가 요청을
  // 거부한다. Claude 계열 에이전트는 기본 모델을 opus-4-8로 고정한다.
  // 세션 내 ModelPicker 전환(setSessionModel)은 env보다 우선하므로 그대로 동작.
  if (command.includes('claude')) {
    env.ANTHROPIC_MODEL = 'claude-opus-4-8'
  }

  const prepend: string[] = []
  if (command && isAbsolute(command)) prepend.push(dirname(command))
  prepend.push(...extraDirs())
  const existing = (env.PATH ?? '').split(delimiter).filter(Boolean)
  const seen = new Set<string>()
  env.PATH = [...prepend, ...existing].filter((d) => d && !seen.has(d) && seen.add(d)).join(delimiter)
  return env
}
