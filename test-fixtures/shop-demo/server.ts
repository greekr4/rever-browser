/**
 * "amajon" — an Amazon-style storefront used as a reverse-engineering demo target.
 *
 * It is a FICTIONAL brand (an Amazon parody), not a real company. Everything is
 * local and every secret is printed here so a tool's answer is checkable.
 *
 * The relatable demo: a shopper's product/price feed is loaded through a PRIVATE,
 * HMAC-SIGNED API — no public API, requests are signed client-side. The reversing
 * goal is "recover the price API and build a price-tracker script."
 *
 * Run:  bun test-fixtures/shop-demo/server.ts        (listens on 8780)
 *
 * Ground truth
 *   HMAC key           amajon-price-signing-key-2026
 *   JWT secret         amajon-hs256-secret
 *   Signature input    `${method}\n${path}\n${body}\n${ts}`   (x-timestamp, x-signature)
 *   API base           /api
 *   Product feed       GET /api/products         (SIGNED — the demo target)
 *   Product detail     GET /api/product/:id      (SIGNED — carries the deal price)
 *   Login              POST /api/login           (issues the JWT)
 *   Deal of the day    product id 7 hides dealPrice 99 (only in detail)
 */

const PORT = 8780
const HMAC_KEY = 'amajon-price-signing-key-2026'
const JWT_SECRET = 'amajon-hs256-secret'

const enc = new TextEncoder()

function b64url(bytes: Uint8Array | string): string {
  const b = typeof bytes === 'string' ? enc.encode(bytes) : bytes
  return btoa(String.fromCharCode(...b)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

async function hmac(key: string, msg: string): Promise<Uint8Array> {
  const k = await crypto.subtle.importKey(
    'raw',
    enc.encode(key),
    { name: 'HMAC', hash: 'SHA-256' },
    false,
    ['sign']
  )
  return new Uint8Array(await crypto.subtle.sign('HMAC', k, enc.encode(msg)))
}

async function hmacHex(key: string, msg: string): Promise<string> {
  const sig = await hmac(key, msg)
  return [...sig].map((b) => b.toString(16).padStart(2, '0')).join('')
}

async function makeJwt(): Promise<string> {
  const now = Math.floor(Date.now() / 1000)
  const header = b64url(JSON.stringify({ alg: 'HS256', typ: 'JWT' }))
  const payload = b64url(
    JSON.stringify({ sub: 'u_42', name: 'Demo Shopper', iss: 'amajon', iat: now, exp: now + 3600 })
  )
  const sig = b64url(await hmac(JWT_SECRET, `${header}.${payload}`))
  return `${header}.${payload}.${sig}`
}

const json = (body: unknown, status = 200, extra: Record<string, string> = {}): Response =>
  new Response(JSON.stringify(body, null, 2), {
    status,
    headers: { 'content-type': 'application/json', 'access-control-allow-origin': '*', ...extra }
  })

/** Bearer token + a correct HMAC signature over the request — the demo target. */
async function authorize(req: Request, url: URL, body: string): Promise<Response | null> {
  const auth = req.headers.get('authorization') ?? ''
  if (!auth.startsWith('Bearer ')) {
    return json({ error: 'missing_token', hint: 'Authorization: Bearer <jwt>' }, 401)
  }
  const ts = req.headers.get('x-timestamp') ?? ''
  const got = req.headers.get('x-signature') ?? ''
  const want = await hmacHex(HMAC_KEY, `${req.method}\n${url.pathname}\n${body}\n${ts}`)
  if (got !== want) {
    return json(
      { error: 'bad_signature', expected: want, received: got || null, signed: 'METHOD\\nPATH\\nBODY\\nTS' },
      403
    )
  }
  return null
}

interface Product {
  id: number
  title: string
  brand: string
  price: number
  list: number
  rating: number
  reviews: number
  prime: boolean
  emoji: string
  hue: number
  dealPrice?: number
}

const PRODUCTS: Product[] = [
  { id: 1, title: 'Wireless Noise-Cancelling Headphones', brand: 'Sonoro', price: 189, list: 259, rating: 4.6, reviews: 12483, prime: true, emoji: '🎧', hue: 205 },
  { id: 2, title: 'Mechanical Keyboard, Hot-Swap RGB', brand: 'KeyForge', price: 96, list: 129, rating: 4.7, reviews: 8021, prime: true, emoji: '⌨️', hue: 265 },
  { id: 3, title: '4K Webcam with Auto-Framing', brand: 'Clario', price: 74, list: 89, rating: 4.3, reviews: 3390, prime: false, emoji: '📷', hue: 20 },
  { id: 4, title: 'Ergonomic Office Chair, Mesh Back', brand: 'Restly', price: 214, list: 310, rating: 4.5, reviews: 5567, prime: true, emoji: '🪑', hue: 150 },
  { id: 5, title: 'Portable SSD 2TB, USB-C', brand: 'FluxDrive', price: 168, list: 199, rating: 4.8, reviews: 20114, prime: true, emoji: '💾', hue: 230 },
  { id: 6, title: 'Smart Standing Desk, 120cm', brand: 'Altura', price: 329, list: 429, rating: 4.4, reviews: 2211, prime: false, emoji: '🖥️', hue: 195 },
  { id: 7, title: 'Espresso Machine, 15-Bar', brand: 'Cremia', price: 149, list: 219, rating: 4.6, reviews: 9902, prime: true, emoji: '☕', hue: 25, dealPrice: 99 },
  { id: 8, title: 'Robot Vacuum with LiDAR', brand: 'Sweepr', price: 279, list: 389, rating: 4.5, reviews: 7180, prime: true, emoji: '🤖', hue: 285 },
  { id: 9, title: 'Air Purifier, HEPA 13', brand: 'Puria', price: 118, list: 159, rating: 4.7, reviews: 6634, prime: true, emoji: '🌀', hue: 175 },
  { id: 10, title: 'Electric Kettle, 1.7L Glass', brand: 'Boilio', price: 39, list: 55, rating: 4.4, reviews: 4450, prime: false, emoji: '🫖', hue: 210 },
  { id: 11, title: 'Monitor 27" QHD 165Hz', brand: 'Vizor', price: 258, list: 349, rating: 4.6, reviews: 8890, prime: true, emoji: '🖥️', hue: 245 },
  { id: 12, title: 'Bluetooth Bookshelf Speakers', brand: 'Sonoro', price: 132, list: 179, rating: 4.5, reviews: 3021, prime: true, emoji: '🔊', hue: 15 }
]

const publicView = (p: Product): Omit<Product, 'dealPrice'> => {
  const { dealPrice: _drop, ...rest } = p
  return rest
}

Bun.serve({
  port: PORT,
  async fetch(req) {
    const url = new URL(req.url)
    const p = url.pathname
    const body = req.method === 'POST' || req.method === 'PUT' ? await req.text() : ''

    if (p === '/' || p === '/index.html') {
      return new Response(Bun.file(`${import.meta.dir}/public/index.html`), {
        headers: { 'content-type': 'text/html; charset=utf-8' }
      })
    }
    if (p === '/styles.css') {
      return new Response(Bun.file(`${import.meta.dir}/public/styles.css`), {
        headers: { 'content-type': 'text/css; charset=utf-8' }
      })
    }
    const img = p.match(/^\/img\/(\d+)\.jpg$/)
    if (img) {
      return new Response(Bun.file(`${import.meta.dir}/public/img/${img[1]}.jpg`), {
        headers: { 'content-type': 'image/jpeg', 'cache-control': 'no-store' }
      })
    }
    if (p === '/assets/app.js') {
      return new Response(Bun.file(`${import.meta.dir}/public/app.js`), {
        headers: { 'content-type': 'application/javascript; charset=utf-8' }
      })
    }
    if (p === '/assets/app.js.map') {
      return new Response(Bun.file(`${import.meta.dir}/public/app.js.map`), {
        headers: { 'content-type': 'application/json' }
      })
    }

    // --- API ------------------------------------------------------------
    if (p === '/api/login' && req.method === 'POST') {
      const token = await makeJwt()
      return json({ token, user: { id: 'u_42', name: 'Demo Shopper' } })
    }

    if (p === '/api/products') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      return json({ count: PRODUCTS.length, currency: 'USD', products: PRODUCTS.map(publicView) })
    }

    const detail = p.match(/^\/api\/product\/(\d+)$/)
    if (detail) {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const prod = PRODUCTS.find((x) => x.id === Number(detail[1]))
      if (!prod) return json({ error: 'not_found' }, 404)
      return json(prod) // detail carries dealPrice (the hidden ground truth)
    }

    return new Response('not found', { status: 404 })
  }
})

console.log(`amajon shop-demo listening on http://127.0.0.1:${PORT}`)
