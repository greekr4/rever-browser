// Red-team multi-tenant state. Confirming BOLA/IDOR (broken object-level
// authorization) needs a SECOND identity: the scan registers a throwaway
// attacker account during auth bootstrap and stashes its session Cookie here,
// so idor_probe can replay an owner's resource request with the attacker's
// cookie and see whether the server lets a different tenant read it.
//
// A plain string (the raw `Cookie:` header value for the alternate tenant),
// electron-free so it stays unit-testable.

let altSession: string | null = null

export function setAltSession(cookie: string | null): void {
  altSession = cookie && cookie.trim() ? cookie.trim() : null
}

export function getAltSession(): string | null {
  return altSession
}

export function clearAltSession(): void {
  altSession = null
}
