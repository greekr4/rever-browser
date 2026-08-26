// reverzon client-integrity guard — READABLE SOURCE.
// This is obfuscated with javascript-obfuscator into public/guard.js, which is
// what the site actually ships. A reverser only sees the obfuscated blob; the
// hidden key, telemetry endpoint, and signing algorithm are recovered by
// running `deobfuscate_script` (webcrack) on the captured guard.js.
;(function () {
  // hidden anti-tamper signing secret (only visible after deobfuscation)
  var GUARD_KEY = 'rvz_guard_1f9c2a7b40e6'
  // hidden telemetry endpoint the storefront never links to
  var TELEMETRY = '/api/_telemetry/collect'

  function fingerprint() {
    return [navigator.userAgent, screen.width + 'x' + screen.height, navigator.language].join('|')
  }

  // keyed FNV-1a hash — the anti-tamper token the backend also recomputes
  function token(payload) {
    var s = GUARD_KEY + '|' + payload
    var h = 0x811c9dc5
    for (var i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i)
      h = (h * 0x01000193) >>> 0
    }
    return 'gt_' + h.toString(16)
  }

  window.__reverzonGuard = {
    sign: token,
    report: function () {
      var fp = fingerprint()
      try {
        navigator.sendBeacon(TELEMETRY, JSON.stringify({ fp: fp, t: token(fp) }))
      } catch (e) {}
    }
  }
})()
