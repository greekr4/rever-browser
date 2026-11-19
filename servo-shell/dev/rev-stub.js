// TEMPORARY stand-in for the preload API (`window.rev`), so rever's interface can boot
// inside the Servo shell before the real bridge to the core process exists.
// Every call resolves to a value that behaves like an empty array / object / string.
// Delete this once `window.rev` is served by the real bridge.
(() => {
  const ARRAY = ['map', 'filter', 'forEach', 'find', 'findIndex', 'some', 'every', 'reduce', 'slice', 'includes', 'join', 'sort', 'flatMap', 'concat', 'indexOf', 'at', 'keys', 'values', 'entries']
  const value = () =>
    new Proxy(function () {}, {
      get(_t, k) {
        if (k === 'then') return undefined
        if (k === Symbol.iterator) return function* () {}
        if (k === Symbol.toPrimitive) return () => ''
        if (typeof k === 'symbol') return undefined
        if (k === 'length' || k === 'size') return 0
        if (k === 'toString' || k === 'valueOf' || k === 'toJSON') return () => ''
        if (ARRAY.includes(k)) return Array.prototype[k].bind([])
        return value()
      },
      apply() {
        return value()
      },
      ownKeys() {
        return []
      },
      getOwnPropertyDescriptor() {
        return undefined
      }
    })
  const api = () =>
    new Proxy(function () {}, {
      get(_t, k) {
        if (typeof k === 'symbol') return undefined
        if (k === 'then') return (resolve) => resolve(value())
        return api()
      },
      apply() {
        return api()
      }
    })
  window.rev = api()
  // Skip first-run dialogs: they cover the page area, and the page is drawn on top of the interface.
  localStorage.setItem('rev:agent-onboarded', 'skipped')
  localStorage.setItem('rev:browser-import-offered', '1')
})()
