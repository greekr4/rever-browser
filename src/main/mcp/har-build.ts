export interface HarHeader {
  name: string
  value: string
}

/** Convert a header record into HAR's name/value array form. */
export function toHeaders(h: Record<string, string> | undefined): HarHeader[] {
  if (!h) return []
  return Object.entries(h).map(([name, value]) => ({ name, value }))
}
