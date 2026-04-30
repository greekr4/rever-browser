export function ok(text: string) {
  return { content: [{ type: 'text' as const, text }] }
}

export function err(text: string) {
  return { isError: true, content: [{ type: 'text' as const, text }] }
}

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
