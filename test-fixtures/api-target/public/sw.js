// Present so sw_list / sw_caches / sw_unregister have something real to find.
const CACHE = 'rever-fixture-v2'

self.addEventListener('install', (e) => {
  e.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(['/', '/assets/app.js', '/wasm-target.html', '/sign.wasm']))
  )
})

self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()))

self.addEventListener('fetch', (e) => {
  e.respondWith(caches.match(e.request).then((hit) => hit ?? fetch(e.request)))
})
