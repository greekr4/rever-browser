import { describe, it, expect } from 'vitest'

import { tryPretty, formatOutput } from './format-json'

describe('tryPretty', () => {
  it('pretty-prints a valid JSON object', () => {
    expect(tryPretty('{"a":1,"b":2}')).toBe('{\n  "a": 1,\n  "b": 2\n}')
  })

  it('pretty-prints a valid JSON array', () => {
    expect(tryPretty('[1,2]')).toBe('[\n  1,\n  2\n]')
  })

  it('tolerates surrounding whitespace', () => {
    expect(tryPretty('  {"a":1}  ')).toBe('{\n  "a": 1\n}')
  })

  it('returns the original text when it is not JSON-shaped', () => {
    expect(tryPretty('hello world')).toBe('hello world')
  })

  it('returns the original text when JSON parse fails', () => {
    // Looks like an object but is malformed — must fall through untouched.
    expect(tryPretty('{not valid}')).toBe('{not valid}')
  })
})

describe('formatOutput', () => {
  it('returns an empty string for null/undefined', () => {
    expect(formatOutput(null)).toBe('')
    expect(formatOutput(undefined)).toBe('')
  })

  it('pretty-prints a JSON string', () => {
    expect(formatOutput('{"a":1}')).toBe('{\n  "a": 1\n}')
  })

  it('joins an array of text content blocks', () => {
    const out = [
      { type: 'text', text: 'first' },
      { type: 'text', text: '{"a":1}' }
    ]
    expect(formatOutput(out)).toBe('first\n\n{\n  "a": 1\n}')
  })

  it('reads a content array off an object', () => {
    const out = { content: [{ type: 'text', text: 'hello' }] }
    expect(formatOutput(out)).toBe('hello')
  })

  it('falls back to JSON.stringify for non-text shapes', () => {
    const out = { foo: 'bar' }
    expect(formatOutput(out)).toBe('{\n  "foo": "bar"\n}')
  })

  it('falls back to JSON.stringify when a content block is not text', () => {
    const out = { content: [{ type: 'image', data: 'x' }] }
    expect(formatOutput(out)).toBe(JSON.stringify(out, null, 2))
  })
})
