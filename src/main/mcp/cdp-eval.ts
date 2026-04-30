import { getActiveTarget } from '../chrome-cdp'

interface CdpEvalResult {
  result: { type: string; value?: unknown }
  exceptionDetails?: { text: string; exception?: { description?: string } }
}

export async function evalInPage<T>(expression: string): Promise<T> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target — open a page first')
  const res = (await target.dbg.sendCommand('Runtime.evaluate', {
    expression,
    returnByValue: true,
    awaitPromise: true,
    userGesture: true
  })) as CdpEvalResult
  if (res.exceptionDetails) {
    const desc =
      res.exceptionDetails.exception?.description ?? res.exceptionDetails.text ?? 'eval failed'
    throw new Error(desc)
  }
  return res.result.value as T
}

export function jsLiteral(s: string): string {
  return JSON.stringify(s)
}
