/**
 * "reverzon" — an Amazon-style storefront used as a reverse-engineering demo target.
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
 *   HMAC key           reverzon-price-signing-key-2026
 *   JWT secret         reverzon-hs256-secret
 *   Signature input    `${method}\n${path}\n${body}\n${ts}`   (x-timestamp, x-signature)
 *   API base           /api
 *   Product feed       GET /api/products         (SIGNED — the demo target)
 *   Product detail     GET /api/product/:id      (SIGNED — carries the deal price)
 *   Login              POST /api/login           (issues the JWT)
 *   Deal of the day    product id 7 hides dealPrice 99 (only in detail)
 */

const PORT = 8780
const HMAC_KEY = 'reverzon-price-signing-key-2026'
const JWT_SECRET = 'reverzon-hs256-secret'

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
    JSON.stringify({ sub: 'u_42', name: 'Demo Shopper', iss: 'reverzon', iat: now, exp: now + 3600 })
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

// ── in-memory cart + orders ──────────────────────────────────────────
// NOTE: this is a DELIBERATELY VULNERABLE demo target. The seeded holes below
// (IDOR on orders, unauthenticated admin report, excessive field exposure, and
// the client-shipped signing key) are intentional — they are what a security
// self-audit is meant to surface. See README "Seeded vulnerabilities".
interface CartLine { id: number; title: string; price: number; qty: number }
interface Order {
  id: string
  email: string // customer PII — exposed by the IDOR below
  items: CartLine[]
  total: number
  status: 'placed' | 'cancelled'
  createdAt: string
}
const cart = new Map<number, number>() // productId -> qty
const DEMO_EMAIL = 'demo@reverzon.com'
// Pre-existing orders from OTHER customers. Order ids are sequential/guessable
// and /api/order/:id performs no ownership check -> IDOR leaks their PII.
const orders: Order[] = [
  { id: 'ord_1000', email: 'j.harper@gmail.com', items: [{ id: 5, title: 'Portable SSD 2TB, USB-C', price: 168, qty: 1 }], total: 168, status: 'placed', createdAt: '2026-08-24T10:12:00.000Z' },
  { id: 'ord_1001', email: 'm.tan@outlook.com', items: [{ id: 11, title: 'Monitor 27" QHD 165Hz', price: 258, qty: 1 }], total: 258, status: 'placed', createdAt: '2026-08-24T15:41:00.000Z' }
]
let orderSeq = 1002

function cartLines(): CartLine[] {
  const lines: CartLine[] = []
  for (const [pid, qty] of cart) {
    const prod = PRODUCTS.find((x) => x.id === pid)
    if (prod) lines.push({ id: prod.id, title: prod.title, price: prod.price, qty })
  }
  return lines
}
const cartTotal = (lines: CartLine[]): number => lines.reduce((s, l) => s + l.price * l.qty, 0)
const cartCount = (): number => [...cart.values()].reduce((s, q) => s + q, 0)
const parseBody = (body: string): Record<string, unknown> => {
  try {
    return JSON.parse(body || '{}')
  } catch {
    return {}
  }
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
        headers: { 'content-type': 'text/css; charset=utf-8', 'cache-control': 'no-store' }
      })
    }
    const img = p.match(/^\/img\/([\w-]+)\.jpg$/)
    if (img) {
      return new Response(Bun.file(`${import.meta.dir}/public/img/${img[1]}.jpg`), {
        headers: { 'content-type': 'image/jpeg', 'cache-control': 'no-store' }
      })
    }
    if (p === '/assets/app.js') {
      return new Response(Bun.file(`${import.meta.dir}/public/app.js`), {
        headers: { 'content-type': 'application/javascript; charset=utf-8', 'cache-control': 'no-store' }
      })
    }
    if (p === '/assets/app.js.map') {
      return new Response(Bun.file(`${import.meta.dir}/public/app.js.map`), {
        headers: { 'content-type': 'application/json' }
      })
    }
    // WASM-signed checkout demo (reversing target for wasm_decompile / wasm_xref)
    if (p === '/secure' || p === '/secure.html') {
      return new Response(Bun.file(`${import.meta.dir}/public/secure.html`), {
        headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' }
      })
    }
    if (p === '/checkout.wasm') {
      return new Response(Bun.file(`${import.meta.dir}/public/checkout.wasm`), {
        headers: { 'content-type': 'application/wasm', 'cache-control': 'no-store' }
      })
    }
    if (p === '/checkout-wasm.js') {
      return new Response(Bun.file(`${import.meta.dir}/public/checkout-wasm.js`), {
        headers: { 'content-type': 'application/javascript; charset=utf-8', 'cache-control': 'no-store' }
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
      // VULN (excessive data exposure): the detail response leaks internal-only
      // fields the storefront never renders — dealPrice, unit cost, and margin.
      const cost = Math.round(prod.price * 0.55)
      return json({ ...prod, cost, margin: prod.price - cost })
    }

    // Add to cart
    if (p === '/api/cart' && req.method === 'POST') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const j = parseBody(body)
      const pid = Number(j.productId)
      const qty = Math.max(1, Number(j.qty ?? 1))
      if (!PRODUCTS.find((x) => x.id === pid))
        return json({ error: 'no_such_product', productId: pid }, 404)
      cart.set(pid, (cart.get(pid) ?? 0) + qty)
      const lines = cartLines()
      return json({ ok: true, count: cartCount(), items: lines, total: cartTotal(lines) })
    }

    // View cart
    if (p === '/api/cart' && req.method === 'GET') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const lines = cartLines()
      return json({ count: cartCount(), items: lines, total: cartTotal(lines), currency: 'USD' })
    }

    // Remove from cart
    if (p === '/api/cart/remove' && req.method === 'POST') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      cart.delete(Number(parseBody(body).productId))
      const lines = cartLines()
      return json({ ok: true, count: cartCount(), items: lines, total: cartTotal(lines) })
    }

    // Place order (checkout the cart)
    if (p === '/api/orders' && req.method === 'POST') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const items = cartLines()
      if (items.length === 0) return json({ error: 'empty_cart' }, 400)
      const order: Order = {
        id: `ord_${orderSeq++}`,
        email: DEMO_EMAIL,
        items,
        total: cartTotal(items),
        status: 'placed',
        createdAt: new Date().toISOString()
      }
      orders.unshift(order)
      cart.clear()
      return json(order, 201)
    }

    // List orders
    if (p === '/api/orders' && req.method === 'GET') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      return json({ count: orders.length, orders })
    }

    // Cancel an order
    const cancel = p.match(/^\/api\/order\/(ord_\d+)\/cancel$/)
    if (cancel && req.method === 'POST') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const o = orders.find((x) => x.id === cancel[1])
      if (!o) return json({ error: 'not_found', id: cancel[1] }, 404)
      if (o.status === 'cancelled') return json({ error: 'already_cancelled', id: o.id }, 409)
      o.status = 'cancelled'
      return json({ ok: true, id: o.id, status: o.status })
    }

    // Order detail
    // VULN (IDOR / BOLA): the token is verified but the order is NOT checked
    // against the caller — any authenticated caller can read ANY order by its
    // sequential, guessable id, leaking the customer's email (PII).
    const ord = p.match(/^\/api\/order\/(ord_\d+)$/)
    if (ord && req.method === 'GET') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const o = orders.find((x) => x.id === ord[1])
      return o ? json(o) : json({ error: 'not_found', id: ord[1] }, 404)
    }

    // VULN (broken function-level authorization): an internal admin report with
    // NO role check. Nothing in the UI links to it, but any valid signature —
    // forgeable with the client-shipped key — returns every order and the
    // business's revenue. Classic "hidden = secure" fallacy.
    if (p === '/api/admin/report' && req.method === 'GET') {
      const denied = await authorize(req, url, body)
      if (denied) return denied
      const revenue = orders.filter((o) => o.status === 'placed').reduce((s, o) => s + o.total, 0)
      return json({
        revenue,
        orderCount: orders.length,
        customers: [...new Set(orders.map((o) => o.email))],
        orders
      })
    }

    // SPA fallback: any non-API GET route serves the app shell (client router
    // handles /product/:id, /cart, /login, /orders).
    if (req.method === 'GET' && !p.startsWith('/api/')) {
      return new Response(Bun.file(`${import.meta.dir}/public/index.html`), {
        headers: { 'content-type': 'text/html; charset=utf-8' }
      })
    }

    return new Response('not found', { status: 404 })
  }
})

console.log(`reverzon shop-demo listening on http://127.0.0.1:${PORT}`)
