export function tryPretty(text: string): string {
  const trimmed = text.trim()
  if (
    (trimmed.startsWith('{') && trimmed.endsWith('}')) ||
    (trimmed.startsWith('[') && trimmed.endsWith(']'))
  ) {
    try {
      return JSON.stringify(JSON.parse(trimmed), null, 2)
    } catch {
      // fallthrough
    }
  }
  return text
}

export function formatOutput(output: unknown): string {
  if (output == null) return ''
  if (typeof output === 'string') return tryPretty(output)

  const contentArr = Array.isArray(output)
    ? output
    : (output as { content?: unknown }).content
  if (Array.isArray(contentArr)) {
    const texts: string[] = []
    let allText = true
    for (const c of contentArr) {
      if (
        c &&
        typeof c === 'object' &&
        (c as { type?: string }).type === 'text' &&
        typeof (c as { text?: unknown }).text === 'string'
      ) {
        texts.push(tryPretty((c as { text: string }).text))
      } else {
        allText = false
        break
      }
    }
    if (allText && texts.length > 0) return texts.join('\n\n')
  }

  return JSON.stringify(output, null, 2)
}
