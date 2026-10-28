// Address-bar input → a loadable URL. A bare host gets a scheme prepended;
// loopback/localhost default to http:// (dev servers rarely serve https), so
// typing `localhost:8080/my` reaches the dev server instead of failing on TLS.
const LOOPBACK =
  /^(localhost|[^./\s]+\.localhost|127(?:\.\d{1,3}){3}|0\.0\.0\.0|\[?::1\]?)(:\d+)?(\/|$|\?|#)/i

export function normalizeAddress(input: string): string {
  const target = input.trim()
  if (!target) return target
  if (/^(https?|view-source|file|about|data):/i.test(target)) return target
  return (LOOPBACK.test(target) ? 'http://' : 'https://') + target
}
