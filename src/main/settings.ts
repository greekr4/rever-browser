import { app, safeStorage } from 'electron'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'

// Anthropic API 키를 userData 아래에 safeStorage로 암호화 저장한다.
// safeStorage(OS 키체인)를 쓸 수 없는 환경에서는 평문 폴백을 사용하되,
// 파일에 어떤 방식으로 저장됐는지 프리픽스로 구분한다.
function keyFile(): string {
  return path.join(app.getPath('userData'), 'anthropic-key.bin')
}

export function setAnthropicApiKey(key: string): void {
  const trimmed = key.trim()
  const file = keyFile()
  if (!trimmed) {
    // 빈 값은 저장 파일 자체를 비워 "키 없음"으로 만든다.
    writeFileSync(file, '')
    return
  }
  if (safeStorage.isEncryptionAvailable()) {
    const enc = safeStorage.encryptString(trimmed)
    writeFileSync(file, Buffer.concat([Buffer.from('enc:'), enc]))
  } else {
    writeFileSync(file, Buffer.concat([Buffer.from('raw:'), Buffer.from(trimmed, 'utf8')]))
  }
}

export function getAnthropicApiKey(): string | null {
  const file = keyFile()
  if (existsSync(file)) {
    const buf = readFileSync(file)
    if (buf.length > 4) {
      const tag = buf.subarray(0, 4).toString('utf8')
      const body = buf.subarray(4)
      if (tag === 'enc:' && safeStorage.isEncryptionAvailable()) {
        try {
          return safeStorage.decryptString(body)
        } catch {
          return null
        }
      }
      if (tag === 'raw:') return body.toString('utf8')
    }
  }
  // 파일이 없거나 비었으면 환경변수로 폴백한다(개발 편의).
  return process.env.ANTHROPIC_API_KEY ?? null
}

export function hasAnthropicApiKey(): boolean {
  return !!getAnthropicApiKey()
}
