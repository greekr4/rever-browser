// Bot-detection bypass payload + spoofed-identity constants, extracted from
// chrome-cdp.ts so the ~600-line stealth script can be reviewed and tested on
// its own. Injected via Page.addScriptToEvaluateOnNewDocument before every doc.

// Pick WebGL vendor/renderer matching the actual host hardware so OS-GPU
// cross-checks (Google CAPTCHA, etc.) don't trigger on a mismatch.
function pickWebGLIdentity(): { vendor: string; renderer: string } {
  if (process.platform === 'darwin') {
    if (process.arch === 'arm64') {
      // Apple Silicon — match what real Chrome reports
      return {
        vendor: 'Google Inc. (Apple)',
        renderer: 'ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)'
      }
    }
    return { vendor: 'Intel Inc.', renderer: 'Intel Iris OpenGL Engine' }
  }
  if (process.platform === 'win32') {
    return {
      vendor: 'Google Inc. (Intel)',
      renderer: 'ANGLE (Intel, Intel(R) UHD Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)'
    }
  }
  return { vendor: 'Mesa/X.org', renderer: 'Mesa Intel(R) UHD Graphics (TGL GT2)' }
}

// Bot-detection bypass injected before every document loads in the webview.
// Patches the standard signals that WAFs (yes24 Code 12, Cloudflare, Akamai, PerimeterX) check:
// navigator.webdriver, chrome.runtime, plugins, languages, permissions, hardwareConcurrency,
// outerWidth/Height, WebGL vendor/renderer.
const WEBGL_IDENTITY = pickWebGLIdentity()
// Match Electron 41's embedded Chromium (146.0.7680.188) so every spoofed
// surface — UA, userAgentData, sec-ch-ua, fullVersionList — is coherent
// with the V8/Blink internals the engine actually exposes. Bumping this
// past the embedded version causes Pixelscan-style "legitimate: false"
// flags because internal behaviour stops matching the claimed version.
export const SPOOFED_CHROME_VERSION = '146.0.7680.188'
export const SPOOFED_CHROME_MAJOR = '146'

export const STEALTH_INIT_SCRIPT = `
(() => {
  const SPOOFED_VERSION = ${JSON.stringify(SPOOFED_CHROME_VERSION)}
  const SPOOFED_MAJOR = ${JSON.stringify(SPOOFED_CHROME_MAJOR)}

  // ── toString cloak ──────────────────────────────────────────────────────
  // Pixelscan / many other detectors iterate getters on Navigator.prototype
  // and call .toString() on each — our spoofed getters used to leak their
  // JS source ("() => false", "() => fakePlugins"), which triggered
  // "masking detected". We register every wrapped fn/getter into a WeakSet
  // and override Function.prototype.toString so those return the canonical
  // "function name() { [native code] }" string instead.
  const nativeFns = new WeakSet()
  const origFnToString = Function.prototype.toString
  function nativeStringFor(fn) {
    return 'function ' + (fn.name || '') + '() { [native code] }'
  }
  const proxiedToString = new Proxy(origFnToString, {
    apply(target, thisArg, args) {
      if (nativeFns.has(thisArg)) return nativeStringFor(thisArg)
      return Reflect.apply(target, thisArg, args)
    }
  })
  // toString itself must look native too (some detectors call
  // Function.prototype.toString.toString() to spot tampering).
  nativeFns.add(proxiedToString)
  try {
    Object.defineProperty(Function.prototype, 'toString', {
      value: proxiedToString,
      configurable: true,
      writable: true
    })
  } catch {}
  const markNative = (fn) => { try { nativeFns.add(fn) } catch {} ; return fn }

  try {
    // 1. navigator.webdriver — redefine on Navigator.prototype (CDP attach sets this true on prototype)
    // Must match native Chrome descriptor exactly: enumerable: true, configurable: true, no setter.
    // Also delete any instance-level property that CDP may have set directly on navigator.
    try { delete navigator.webdriver } catch {}
    try {
      const webdriverGetter = markNative(Object.getOwnPropertyDescriptor({ get webdriver() { return false } }, 'webdriver').get)
      Object.defineProperty(Navigator.prototype, 'webdriver', {
        get: webdriverGetter,
        set: undefined,
        configurable: true,
        enumerable: true
      })
    } catch {}
    // Guard against CDP re-setting webdriver after this script runs:
    // use a Proxy on Navigator.prototype to intercept defineProperty attempts
    // that try to set webdriver back to true.
    try {
      const origDefProp = Object.defineProperty
      const wdTrap = markNative(function defineProperty(obj, prop, desc) {
        if ((obj === Navigator.prototype || obj === navigator) && prop === 'webdriver') {
          // Silently block — keep our false getter
          return obj
        }
        return origDefProp.call(Object, obj, prop, desc)
      })
      Object.defineProperty = wdTrap
    } catch {}

    // 2. chrome.runtime + chrome.app / chrome.csi / chrome.loadTimes
    // amiunique flagged detailChrome.{app,csi,loadTimes} = UNKNOWN as the
    // decisive Electron tell — real Chrome always exposes these.
    if (!window.chrome) window.chrome = {}
    if (!window.chrome.runtime) {
      // connect and sendMessage must look like native functions with correct .length
      // (pixelscan measures them). Real Chrome: connect.length = 0, sendMessage.length = 0
      const rtConnect = markNative(function connect() {})
      const rtSendMessage = markNative(function sendMessage() {})
      // getManifest returns empty object in non-extension context
      const rtGetManifest = markNative(function getManifest() { return {} })
      // id is undefined for non-extension pages (but property must exist)
      window.chrome.runtime = {
        connect: rtConnect,
        sendMessage: rtSendMessage,
        getManifest: rtGetManifest,
        id: undefined,
        PlatformOs: { MAC: 'mac', WIN: 'win', ANDROID: 'android', CROS: 'cros', LINUX: 'linux', OPENBSD: 'openbsd' },
        PlatformArch: { ARM: 'arm', X86_32: 'x86-32', X86_64: 'x86-64' },
        OnInstalledReason: { INSTALL: 'install', UPDATE: 'update', CHROME_UPDATE: 'chrome_update', SHARED_MODULE_UPDATE: 'shared_module_update' },
        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
        RequestUpdateCheckStatus: { THROTTLED: 'throttled', NO_UPDATE: 'no_update', UPDATE_AVAILABLE: 'update_available' }
      }
    }
    if (!window.chrome.app) {
      const appInstallState = { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' }
      const runningState = { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' }
      window.chrome.app = {
        isInstalled: false,
        InstallState: appInstallState,
        RunningState: runningState,
        getDetails: function () { return null },
        getIsInstalled: function () { return false }
      }
    }
    if (!window.chrome.csi) {
      const startTime = Date.now() - Math.floor(Math.random() * 2000) - 500
      window.chrome.csi = function () {
        return {
          startE: startTime,
          onloadT: startTime + 280 + Math.floor(Math.random() * 200),
          pageT: 320 + Math.random() * 180,
          tran: 15
        }
      }
    }
    if (!window.chrome.loadTimes) {
      const navStart = (performance.timeOrigin || Date.now()) / 1000
      window.chrome.loadTimes = function () {
        return {
          requestTime: navStart,
          startLoadTime: navStart + 0.001,
          commitLoadTime: navStart + 0.05,
          finishDocumentLoadTime: navStart + 0.4,
          finishLoadTime: navStart + 0.6,
          firstPaintTime: navStart + 0.55,
          firstPaintAfterLoadTime: 0,
          navigationType: 'Other',
          wasFetchedViaSpdy: true,
          wasNpnNegotiated: true,
          npnNegotiatedProtocol: 'h2',
          wasAlternateProtocolAvailable: false,
          connectionInfo: 'h2'
        }
      }
    }

    // 2b. userAgentData.brands — must include "Google Chrome", not just "Chromium".
    // Electron's default brand list omits Google Chrome which is a hard tell.
    try {
      if (navigator.userAgentData) {
        const fakeBrands = [
          { brand: 'Chromium', version: SPOOFED_MAJOR },
          { brand: 'Google Chrome', version: SPOOFED_MAJOR },
          { brand: 'Not.A/Brand', version: '99' }
        ]
        const brandsGetter = markNative(Object.getOwnPropertyDescriptor({ get brands() { return fakeBrands } }, 'brands').get)
        Object.defineProperty(navigator.userAgentData, 'brands', { get: brandsGetter, configurable: true })
        const origGHE = navigator.userAgentData.getHighEntropyValues?.bind(navigator.userAgentData)
        if (origGHE) {
          navigator.userAgentData.getHighEntropyValues = markNative(async function getHighEntropyValues(hints) {
            const v = await origGHE(hints)
            v.brands = fakeBrands
            v.fullVersionList = [
              { brand: 'Chromium', version: SPOOFED_VERSION },
              { brand: 'Google Chrome', version: SPOOFED_VERSION },
              { brand: 'Not.A/Brand', version: '99.0.0.0' }
            ]
            v.uaFullVersion = SPOOFED_VERSION
            return v
          })
        }
      }
    } catch {}

    // 3. navigator.plugins — must be a real PluginArray (instanceof check), not a plain Array
    try {
      const makePlugin = (name, filename, description) => {
        const mime = Object.create(MimeType.prototype)
        Object.defineProperties(mime, {
          type: { value: 'application/pdf' },
          suffixes: { value: 'pdf' },
          description: { value: description }
        })
        const plugin = Object.create(Plugin.prototype)
        Object.defineProperties(plugin, {
          name: { value: name },
          filename: { value: filename },
          description: { value: description },
          length: { value: 1 },
          0: { value: mime },
          item: { value: () => mime },
          namedItem: { value: () => mime }
        })
        mime.enabledPlugin = plugin
        return plugin
      }
      const p1 = makePlugin('PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format')
      const p2 = makePlugin('Chrome PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format')
      const p3 = makePlugin('Chromium PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format')
      const fakePlugins = Object.create(PluginArray.prototype)
      Object.defineProperties(fakePlugins, {
        length: { value: 3 },
        0: { value: p1, enumerable: true },
        1: { value: p2, enumerable: true },
        2: { value: p3, enumerable: true },
        item: { value: (i) => fakePlugins[i] || null },
        namedItem: { value: (n) => [p1, p2, p3].find((p) => p.name === n) || null },
        refresh: { value: () => {} }
      })
      const pluginsGetter = markNative(Object.getOwnPropertyDescriptor({ get plugins() { return fakePlugins } }, 'plugins').get)
      Object.defineProperty(Navigator.prototype, 'plugins', {
        get: pluginsGetter,
        configurable: true
      })
    } catch {}

    // 4. navigator.languages — must align with Accept-Language.
    // ko-KR-first was 0.00% unique on amiunique; en-US default is the global modal value.
    const languagesGetter = markNative(Object.getOwnPropertyDescriptor({ get languages() { return ['en-US', 'en'] } }, 'languages').get)
    Object.defineProperty(navigator, 'languages', {
      get: languagesGetter,
      configurable: true
    })

    // 5. permissions API — return realistic per-permission defaults like a freshly-installed Chrome.
    // Real Chrome: most names → 'prompt', notifications → 'default' (matches Notification.permission),
    // and unknown / removed names (accessibility, ambient-light-sensor) THROW TypeError.
    // Previously returning 'prompt' for unknown names AND 'granted' for sensors created an
    // unprecedented response vector (0.00% on amiunique).
    const origQuery = navigator.permissions && navigator.permissions.query
    if (origQuery) {
      // Names Chrome's Permissions API actually accepts. Anything else throws TypeError.
      // Sensors (accelerometer/gyroscope/magnetometer) ARE valid and return 'granted'
      // by default — earlier we wrongly classified them as invalid which made amiunique
      // print "Not supported" for those rows, an unprecedented vector.
      const VALID_NAMES = new Set([
        'accelerometer', 'background-fetch', 'background-sync', 'bluetooth',
        'camera', 'clipboard-read', 'clipboard-write', 'display-capture',
        'geolocation', 'gyroscope', 'idle-detection', 'local-fonts',
        'magnetometer', 'microphone', 'midi', 'nfc', 'notifications',
        'payment-handler', 'periodic-background-sync', 'persistent-storage',
        'push', 'screen-wake-lock', 'speaker-selection', 'storage-access',
        'system-wake-lock', 'top-level-storage-access', 'window-management',
        'window-placement'
      ])
      // Chrome's defaults for a freshly-installed profile with no site grants:
      // sensors and clipboard-write are 'granted' silently; notifications reflects
      // Notification.permission; everything else is 'prompt'.
      const FORCED = {
        'accelerometer': () => 'granted',
        'gyroscope': () => 'granted',
        'magnetometer': () => 'granted',
        'clipboard-write': () => 'granted',
        'notifications': () => (typeof Notification !== 'undefined' ? Notification.permission : 'default')
      }
      const makeStatus = (state) => {
        const status = { state, onchange: null }
        try {
          status.addEventListener = markNative(function addEventListener() {})
          status.removeEventListener = markNative(function removeEventListener() {})
          status.dispatchEvent = markNative(function dispatchEvent() { return false })
        } catch {}
        return status
      }
      const wrappedQuery = markNative(function query(params) {
        if (!params || typeof params !== 'object' || !params.name) {
          return origQuery.call(navigator.permissions, params)
        }
        const name = params.name
        if (!VALID_NAMES.has(name)) {
          // Real Chrome throws TypeError for unsupported / removed permission names.
          return Promise.reject(new TypeError(
            "Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': The provided value '" + name + "' is not a valid enum value of type PermissionName."
          ))
        }
        if (FORCED[name]) return Promise.resolve(makeStatus(FORCED[name]()))
        // Default for everything else: 'prompt' — matches a fresh Chrome with no prior site grants.
        return Promise.resolve(makeStatus('prompt'))
      })
      navigator.permissions.query = wrappedQuery
    }

    // 6. hardware
    const hcGetter = markNative(Object.getOwnPropertyDescriptor({ get hardwareConcurrency() { return 8 } }, 'hardwareConcurrency').get)
    Object.defineProperty(navigator, 'hardwareConcurrency', { get: hcGetter, configurable: true })
    const dmGetter = markNative(Object.getOwnPropertyDescriptor({ get deviceMemory() { return 8 } }, 'deviceMemory').get)
    Object.defineProperty(navigator, 'deviceMemory', { get: dmGetter, configurable: true })

    // 7. outer dimensions offset (headless has them == inner)
    const owGetter = markNative(Object.getOwnPropertyDescriptor({ get outerWidth() { return window.innerWidth } }, 'outerWidth').get)
    Object.defineProperty(window, 'outerWidth', { get: owGetter, configurable: true })
    const ohGetter = markNative(Object.getOwnPropertyDescriptor({ get outerHeight() { return window.innerHeight + 85 } }, 'outerHeight').get)
    Object.defineProperty(window, 'outerHeight', { get: ohGetter, configurable: true })

    // 7b. Normalise screen.avail* — on macOS the menu bar makes
    // screen.availTop = 30 / availHeight = screen.height - 30 (amiunique 0.21%).
    // Pretend the menu bar is hidden / we're on Windows-style chrome:
    // availTop = 0, availHeight = screen.height. Same for availLeft/Width.
    try {
      const atGetter = markNative(Object.getOwnPropertyDescriptor({ get availTop() { return 0 } }, 'availTop').get)
      Object.defineProperty(Screen.prototype, 'availTop', { get: atGetter, configurable: true })
      const alGetter = markNative(Object.getOwnPropertyDescriptor({ get availLeft() { return 0 } }, 'availLeft').get)
      Object.defineProperty(Screen.prototype, 'availLeft', { get: alGetter, configurable: true })
      const ahGetter = markNative(function availHeight() { return screen.height })
      Object.defineProperty(Screen.prototype, 'availHeight', { get: ahGetter, configurable: true })
      const awGetter = markNative(function availWidth() { return screen.width })
      Object.defineProperty(Screen.prototype, 'availWidth', { get: awGetter, configurable: true })
    } catch {}

    // 8. WebGL vendor/renderer — match host hardware so OS-GPU cross-checks pass
    const __vendor = ${JSON.stringify(WEBGL_IDENTITY.vendor)}
    const __renderer = ${JSON.stringify(WEBGL_IDENTITY.renderer)}
    const getParam = WebGLRenderingContext.prototype.getParameter
    const wrappedGetParam = markNative(function getParameter(p) {
      if (p === 37445) return __vendor
      if (p === 37446) return __renderer
      return getParam.apply(this, arguments)
    })
    WebGLRenderingContext.prototype.getParameter = wrappedGetParam
    if (window.WebGL2RenderingContext) {
      const getParam2 = WebGL2RenderingContext.prototype.getParameter
      const wrappedGetParam2 = markNative(function getParameter(p) {
        if (p === 37445) return __vendor
        if (p === 37446) return __renderer
        return getParam2.apply(this, arguments)
      })
      WebGL2RenderingContext.prototype.getParameter = wrappedGetParam2
    }

    // 9. Canvas noise — perturb a few random non-transparent pixels in
    // toDataURL/toBlob output so the per-browser canvas hash varies across
    // sessions. Only modifies pixels with alpha > 0 to avoid corrupting
    // transparency tests (tpCanvas). Noise is spread across R/G/B channels.
    // getImageData is NOT wrapped — wrapping it caused double-noise when
    // noisify reads pixel data internally.
    try {
      const seed = (Math.random() * 1e9) | 0
      let s = seed
      const rng = () => { s = (s * 1664525 + 1013904223) | 0; return ((s >>> 0) % 1000) / 1000 }
      const origGetImageData = CanvasRenderingContext2D.prototype.getImageData
      const noisify = (canvas) => {
        const ctx = canvas.getContext && canvas.getContext('2d')
        if (!ctx) return
        const w = canvas.width, h = canvas.height
        if (!w || !h) return
        try {
          const img = origGetImageData.call(ctx, 0, 0, w, h)
          const d = img.data
          let applied = 0
          for (let attempts = 0; applied < 10 && attempts < 60; attempts++) {
            const px = (Math.floor(rng() * w * h)) * 4
            // Skip fully transparent pixels — modifying them is a masking tell
            if (d[px + 3] === 0) continue
            // Spread noise across R, G, B (pick one channel per pixel)
            const ch = Math.floor(rng() * 3)
            d[px + ch] = (d[px + ch] + (rng() < 0.5 ? 1 : -1)) & 0xff
            applied++
          }
          ctx.putImageData(img, 0, 0)
        } catch {}
      }
      const origToDataURL = HTMLCanvasElement.prototype.toDataURL
      HTMLCanvasElement.prototype.toDataURL = markNative(function toDataURL() {
        noisify(this)
        return origToDataURL.apply(this, arguments)
      })
      const origToBlob = HTMLCanvasElement.prototype.toBlob
      HTMLCanvasElement.prototype.toBlob = markNative(function toBlob() {
        noisify(this)
        return origToBlob.apply(this, arguments)
      })
    } catch {}

    // 10. Audio fingerprint noise — perturb floats coming out of
    // AudioBuffer.getChannelData / AnalyserNode.getFloatFrequencyData so
    // audio fingerprinters can't pin the same hash across runs.
    // Only add noise when the buffer has real data (non-zero) to avoid
    // corrupting empty/silent buffers which produce detectable empty hashes.
    try {
      const origGetChannelData = AudioBuffer.prototype.getChannelData
      AudioBuffer.prototype.getChannelData = markNative(function getChannelData() {
        const arr = origGetChannelData.apply(this, arguments)
        if (arr && arr.length) {
          // Check if buffer has real data before adding noise
          let hasData = false
          for (let i = 0; i < Math.min(arr.length, 200); i++) {
            if (arr[i] !== 0) { hasData = true; break }
          }
          if (hasData) {
            const noise = (Math.random() * 1e-7) - 5e-8
            for (let i = 0; i < arr.length; i += 100) arr[i] = arr[i] + noise
          }
        }
        return arr
      })
      if (window.AnalyserNode) {
        const origFreq = AnalyserNode.prototype.getFloatFrequencyData
        AnalyserNode.prototype.getFloatFrequencyData = markNative(function getFloatFrequencyData(arr) {
          origFreq.apply(this, arguments)
          if (arr && arr.length) {
            let hasData = false
            for (let i = 0; i < Math.min(arr.length, 200); i++) {
              if (arr[i] !== 0 && arr[i] !== -Infinity) { hasData = true; break }
            }
            if (hasData) {
              const noise = (Math.random() * 0.1) - 0.05
              for (let i = 0; i < arr.length; i += 50) arr[i] = arr[i] + noise
            }
          }
        })
      }
    } catch {}

    // 11. Hide __reverAi from window enumeration so WASM fingerprinters
    // doing Object.getOwnPropertyNames(window) / Reflect.ownKeys(window)
    // don't see our injected helper. The property still works for direct
    // access (window.__reverAi.flashElement(...)) — only enumeration is filtered.
    try {
      const filterKey = '__reverAi'
      const origGOPN = Object.getOwnPropertyNames
      Object.getOwnPropertyNames = markNative(function getOwnPropertyNames(obj) {
        const names = origGOPN.apply(this, arguments)
        if (obj === window) return names.filter((n) => n !== filterKey)
        return names
      })
      const origKeys = Object.keys
      Object.keys = markNative(function keys(obj) {
        const r = origKeys.apply(this, arguments)
        if (obj === window) return r.filter((k) => k !== filterKey)
        return r
      })
      const origOwnKeys = Reflect.ownKeys
      Reflect.ownKeys = markNative(function ownKeys(obj) {
        const r = origOwnKeys.apply(this, arguments)
        if (obj === window) return r.filter((k) => k !== filterKey)
        return r
      })
    } catch {}

    // 12. Remove CDP / ChromeDriver artefacts from window and document.
    // ChromeDriver injects cdc_ prefixed properties; CDP may inject others.
    // These are strong automation signals that pixelscan checks.
    try {
      for (const obj of [window, document, Navigator.prototype]) {
        for (const prop of Object.getOwnPropertyNames(obj)) {
          if (/^(cdc_|__cdc_|_selenium|_Selenium|calledSelenium|_phantom|__nightmare|domAutomation|webdriver)/.test(prop)) {
            try { delete obj[prop] } catch {}
          }
        }
      }
    } catch {}

    // 13. Notification.permission — must be 'default' (consistent with permissions query)
    try {
      if (typeof Notification !== 'undefined' && Notification.permission !== 'default') {
        const notifPermGetter = markNative(Object.getOwnPropertyDescriptor({ get permission() { return 'default' } }, 'permission').get)
        Object.defineProperty(Notification, 'permission', { get: notifPermGetter, configurable: true })
      }
    } catch {}

    // 14. window.chrome.runtime property descriptor hardening.
    // Make connect/sendMessage non-enumerable like real Chrome.
    try {
      if (window.chrome && window.chrome.runtime) {
        for (const k of ['connect', 'sendMessage', 'getManifest']) {
          if (window.chrome.runtime[k]) {
            Object.defineProperty(window.chrome.runtime, k, {
              value: window.chrome.runtime[k],
              writable: true,
              enumerable: true,
              configurable: true
            })
          }
        }
      }
    } catch {}

    // 15. NetworkInformation API — amiunique flagged downlink:5.2/rtt:50 as 0.01% unique.
    // Pin to the Chrome-modal values: effectiveType '4g', downlink 10 (Chrome clamps
    // anything > 10 Mbps to 10 since 2020 to reduce fingerprintable surface), rtt rounded
    // to 50ms, saveData false. Patch on the prototype so both navigator.connection and
    // any new instance see the same values, and override the change-event surface.
    try {
      if (window.NetworkInformation && navigator.connection) {
        const NIP = NetworkInformation.prototype
        const fixedValues = {
          effectiveType: '4g',
          downlink: 10,
          rtt: 50,
          saveData: false,
          type: 'wifi',
          downlinkMax: Infinity
        }
        for (const key of Object.keys(fixedValues)) {
          try {
            const val = fixedValues[key]
            const getter = markNative(Object.getOwnPropertyDescriptor({ get x() { return val } }, 'x').get)
            Object.defineProperty(NIP, key, { get: getter, configurable: true, enumerable: true })
          } catch {}
        }
        // Neutralise change-event listeners — real Chrome rarely fires these on a
        // stable wifi connection, and stable values are what we want to project.
        try {
          NIP.addEventListener = markNative(function addEventListener() {})
          NIP.removeEventListener = markNative(function removeEventListener() {})
          const onchangeGetter = markNative(Object.getOwnPropertyDescriptor({ get onchange() { return null } }, 'onchange').get)
          Object.defineProperty(NIP, 'onchange', { get: onchangeGetter, set: markNative(function onchange() {}), configurable: true })
        } catch {}
      }
    } catch {}

    // 18. Intl API locale consistency — patch resolvedOptions() on each Intl
    // class prototype so that the OS default locale ('ko' on Korean Mac) is
    // replaced with 'en-US' to match navigator.languages and Accept-Language.
    // Pixelscan's "Internationalization API" check returns this value and
    // flags any disagreement with navigator.languages as 'masking detected'.
    //
    // We touch ONLY resolvedOptions (not the constructor) — wrapping the
    // constructor breaks the prototype.constructor === Intl.DateTimeFormat
    // invariant which pixelscan was independently flagging as a Browser tile
    // failure. Explicit "new Intl.DateTimeFormat('ko-KR')" calls still return
    // 'ko-KR' (we only override when the result equals the OS default).
    try {
      const FORCED_DEFAULT_LOCALE = 'en-US'
      // Sample the OS default once so we know what to overwrite.
      let osDefault = ''
      try { osDefault = new Intl.DateTimeFormat().resolvedOptions().locale } catch {}
      for (const clsName of ['DateTimeFormat', 'NumberFormat', 'Collator', 'PluralRules', 'RelativeTimeFormat', 'ListFormat', 'DisplayNames', 'Segmenter']) {
        if (!window.Intl || !Intl[clsName] || !Intl[clsName].prototype || !Intl[clsName].prototype.resolvedOptions) continue
        const proto = Intl[clsName].prototype
        const orig = proto.resolvedOptions
        const wrapped = markNative(function resolvedOptions() {
          const r = orig.apply(this, arguments)
          if (r && typeof r === 'object' && osDefault && r.locale === osDefault) {
            r.locale = FORCED_DEFAULT_LOCALE
          }
          return r
        })
        try { proto.resolvedOptions = wrapped } catch {}
      }
    } catch {}

    // 17. Self-probe for fingerprint test sites. When the user navigates to a
    // known fingerprinting analyser, dump the rendered page text via console.log
    // with a magic prefix; main process picks it up via Runtime.consoleAPICalled
    // and writes it to userData/fingerprint-probes/. Lets the dev (or AI agent)
    // diagnose stealth gaps without a separate testing harness.
    try {
      const PROBE_HOSTS = ['amiunique.org', 'abrahamjuliot.github.io', 'browserleaks.com', 'pixelscan.net', 'bot.sannysoft.com']
      const host = location.hostname
      const isProbe = PROBE_HOSTS.some(h => host === h || host.endsWith('.' + h))
      if (isProbe) {
        const probe = () => {
          try {
            const text = document.body && document.body.innerText
            if (!text || text.length < 500) return false
            // Reject pages that are still showing a loading placeholder — many
            // fingerprint analysers (amiunique, creepjs) render their JS section
            // asynchronously after the rest of the page is interactive.
            if (/Loading items|loading\.\.\.|Computing/i.test(text)) return false
            const payload = { url: location.href, host: host, ts: Date.now(), text: text.substring(0, 200000) }
            console.log('[REVER_PROBE]', JSON.stringify(payload))
            return true
          } catch { return false }
        }
        // Re-emit on every meaningful DOM change up to ~30 seconds.
        // Each emit overwrites the previous file (timestamp-based name); the
        // last one captured is the most-complete view.
        let lastDump = 0
        const tryDump = () => {
          const now = Date.now()
          if (now - lastDump < 1500) return
          if (probe()) lastDump = now
        }
        const start = Date.now()
        const interval = setInterval(() => {
          tryDump()
          if (Date.now() - start > 30000) clearInterval(interval)
        }, 2000)
        if (document.readyState !== 'complete') {
          window.addEventListener('load', tryDump)
        }
      }
    } catch {}

    // 16. Fonts enumeration via CSS Font Loading API.
    // 213 distinct fonts via document.fonts is a strong macOS-with-extras tell.
    // Clamp document.fonts.check() to a curated whitelist of fonts every macOS
    // Chrome ships with by default. Measurement-based detection (offsetWidth)
    // is intentionally NOT patched — too high a false-positive risk for legit pages.
    try {
      const FONT_WHITELIST = new Set([
        'Arial', 'Arial Black', 'Helvetica', 'Helvetica Neue', 'Times', 'Times New Roman',
        'Courier', 'Courier New', 'Verdana', 'Tahoma', 'Trebuchet MS', 'Georgia',
        'Comic Sans MS', 'Impact', 'Lucida Console', 'Lucida Sans Unicode', 'Palatino',
        'Monaco', 'Menlo', 'Apple Color Emoji', 'Apple Symbols', 'Apple SD Gothic Neo',
        'Geneva', 'Optima', 'Didot', 'American Typewriter', 'Andale Mono', 'Avenir',
        'Avenir Next', 'Baskerville', 'Big Caslon', 'Brush Script MT', 'Chalkboard',
        'Cochin', 'Copperplate', 'Futura', 'Gill Sans', 'Hoefler Text', 'PingFang SC',
        'Hiragino Sans', 'serif', 'sans-serif', 'monospace', 'cursive', 'fantasy',
        'system-ui', '-apple-system', 'BlinkMacSystemFont', 'ui-monospace', 'ui-serif',
        'ui-sans-serif'
      ])
      if (document.fonts && typeof document.fonts.check === 'function') {
        const origCheck = document.fonts.check.bind(document.fonts)
        document.fonts.check = markNative(function check(font, text) {
          try {
            // Extract font-family token(s) from a CSS font shorthand.
            // Strip leading style/variant/weight/size, keep what's after the last numeric size.
            const m = String(font).match(/(?:\\d+(?:\\.\\d+)?(?:px|pt|em|rem|%)?\\s+)(.+)$/) || [null, font]
            const familiesRaw = (m[1] || '').split(',')
            for (const raw of familiesRaw) {
              const cleaned = raw.trim().replace(/^["']|["']$/g, '')
              if (!cleaned) continue
              if (!FONT_WHITELIST.has(cleaned)) return false
            }
            return origCheck(font, text)
          } catch {
            return false
          }
        })
      }
    } catch {}
  } catch (e) {
    // Never break the page; stealth is best-effort
    console && console.debug && console.debug('[stealth]', e)
  }
})();
`
