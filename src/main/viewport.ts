import { getActiveTarget } from './chrome-cdp'

export type ViewportMode = 'desktop' | 'mobile'

const MOBILE_UA =
  'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1'

const MOBILE_METRICS = {
  width: 390,
  height: 844,
  deviceScaleFactor: 3,
  mobile: true
}

let currentMode: ViewportMode = 'desktop'
let originalUA: string | null = null

export function getViewport(): ViewportMode {
  return currentMode
}

export async function setViewport(mode: ViewportMode): Promise<ViewportMode> {
  const target = getActiveTarget()
  if (!target) throw new Error('no active browser target — open a page first')

  if (originalUA == null) originalUA = target.wc.getUserAgent()

  if (mode === 'mobile') {
    target.wc.setUserAgent(MOBILE_UA)
    await target.dbg
      .sendCommand('Emulation.setDeviceMetricsOverride', MOBILE_METRICS)
      .catch((e) => console.error('[viewport] setDeviceMetricsOverride:', e))
  } else {
    target.wc.setUserAgent(originalUA ?? '')
    await target.dbg
      .sendCommand('Emulation.clearDeviceMetricsOverride')
      .catch((e) => console.error('[viewport] clearDeviceMetricsOverride:', e))
  }

  currentMode = mode
  target.wc.reload()
  return mode
}
