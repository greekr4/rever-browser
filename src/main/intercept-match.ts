// Pure helpers for matching a paused Fetch request against intercept rules.
// Kept electron-free so vitest can cover them.

/**
 * Compile a CDP `Fetch.enable` urlPattern into a RegExp.
 * CDP semantics: `*` = zero or more chars, `?` = exactly one, `\` escapes.
 * Every other regex metacharacter is escaped, so agent input can never throw.
 */
export function compileUrlPattern(pattern: string): RegExp {
  let src = ''
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i]
    if (ch === '\\' && i + 1 < pattern.length) {
      src += '\\' + pattern[++i]
    } else if (ch === '*') {
      src += '.*'
    } else if (ch === '?') {
      src += '.'
    } else {
      src += ch.replace(/[.+^${}()|[\]\\/]/g, '\\$&')
    }
  }
  return new RegExp(`^${src}$`)
}

export interface CompiledRule<R> {
  rule: R
  re: RegExp
}

export function compileRules<R extends { urlPattern: string }>(rules: R[]): CompiledRule<R>[] {
  return rules.map((rule) => ({ rule, re: compileUrlPattern(rule.urlPattern) }))
}

/** First rule whose pattern matches `url`, or undefined. Never throws. */
export function findMatchingRule<R>(compiled: CompiledRule<R>[], url: string): R | undefined {
  for (const { rule, re } of compiled) {
    try {
      if (re.test(url)) return rule
    } catch {
      // defensive: a RegExp.test cannot throw, but never let a listener die
    }
  }
  return undefined
}
