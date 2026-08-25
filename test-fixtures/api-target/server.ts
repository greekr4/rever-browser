/**
 * A deliberately realistic reverse-engineering target.
 *
 * The snapshot fixtures cover the DOM tools; nothing covered the API-analysis
 * half of the tool surface, so those tools could only ever be tested against
 * live sites — where there is no ground truth to check an answer against.
 * This server supplies that ground truth: every secret it uses is printed
 * below, so a tool's answer is either right or wrong, not "looks plausible".
 *
 * Run:  bun test-fixtures/api-target/server.ts        (listens on 8779)
 *
 * Ground truth
 *   HMAC key           s3cr3t-signing-key
 *   JWT secret         jwt-hs256-secret
 *   WASM module        /sign.wasm exports "checksum" (loaded by /wasm-target.html)
 *   JWT payload        { sub: "u_42", role: "admin", ... }
 *   Signature input    `${method}\n${path}\n${body}\n${ts}`
 *   API base           /api/v3
 *   Protected routes   /api/v3/profile, /api/v3/items, /api/v3/admin/keys
 *   Hidden route       /api/v3/admin/keys      (referenced by no page code)
 *   Reflects CRLF      /api/v3/echo?name=      (unsanitised header write)
 *   Path traversal     /api/v3/file?p=         (serves ./public/<p>)
 */

const PORT = 8779
const HMAC_KEY = 's3cr3t-signing-key'
const JWT_SECRET = 'jwt-hs256-secret'

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

/** A real HS256 JWT, so decode_token has something authentic to chew on. */
async function makeJwt(): Promise<string> {
  const now = Math.floor(Date.now() / 1000)
  const header = b64url(JSON.stringify({ alg: 'HS256', typ: 'JWT' }))
  const payload = b64url(
    JSON.stringify({
      sub: 'u_42',
      role: 'admin',
      scope: ['read:items', 'write:items'],
      iss: 'rever-fixture',
      iat: now,
      exp: now + 3600
    })
  )
  const sig = b64url(await hmac(JWT_SECRET, `${header}.${payload}`))
  return `${header}.${payload}.${sig}`
}

const json = (body: unknown, status = 200, extra: Record<string, string> = {}): Response =>
  new Response(JSON.stringify(body, null, 2), {
    status,
    headers: { 'content-type': 'application/json', 'access-control-allow-origin': '*', ...extra }
  })

/** Shared auth check: Bearer token + a correct HMAC signature over the request. */
async function authorize(req: Request, url: URL, body: string): Promise<Response | null> {
  const auth = req.headers.get('authorization') ?? ''
  if (!auth.startsWith('Bearer ')) {
    return json({ error: 'missing_token', hint: 'Authorization: Bearer <jwt>' }, 401)
  }
  const parts = auth.slice(7).split('.')
  if (parts.length !== 3) return json({ error: 'malformed_token' }, 401)

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

const ITEMS = Array.from({ length: 47 }, (_, i) => ({
  id: i + 1,
  name: `item-${i + 1}`,
  price: 1000 * (i + 1),
  secret: i === 41 ? 'flag{deep-pagination}' : null
}))

Bun.serve({
  port: PORT,
  async fetch(req, server) {
    const url = new URL(req.url)
    const p = url.pathname
    const body = req.method === 'POST' || req.method === 'PUT' ? await req.text() : ''

    if (p === '/ws') {
      if (server.upgrade(req)) return undefined as unknown as Response
      return new Response('expected websocket', { status: 400 })
    }

    if (p === '/' || p === '/index.html') {
      return new Response(Bun.file(`${import.meta.dir}/public/index.html`), {
        headers: { 'content-type': 'text/html; charset=utf-8' }
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
    if (p === '/sw.js') {
      return new Response(Bun.file(`${import.meta.dir}/public/sw.js`), {
        headers: { 'content-type': 'application/javascript; charset=utf-8' }
      })
    }
    if (p === '/wasm-target.html') {
      return new Response(Bun.file(`${import.meta.dir}/public/wasm-target.html`), {
        headers: { 'content-type': 'text/html; charset=utf-8' }
      })
    }
    if (p === '/sign.wasm') {
      return new Response(Bun.file(`${import.meta.dir}/public/sign.wasm`), {
        headers: { 'content-type': 'application/wasm' }
      })
    }
    if (p === '/wasm-caller.js') {
      return new Response(Bun.file(`${import.meta.dir}/public/wasm-caller.js`), {
        headers: { 'content-type': 'application/javascript; charset=utf-8' }
      })
    }

    // --- API -----------------------------------------------------------
    if (p === '/api/v3/login' && req.method === 'POST') {
      const token = await makeJwt()
      return json(
        { token, user: { id: 'u_42', role: 'admin' } },
        200,
        { 'set-cookie': `session=${token}; Path=/; HttpOnly; SameSite=Lax` }
      )
    }

    // Unsanitised reflection into a response header — what crlf_test looks for.
    if (p === '/api/v3/echo') {
      const name = url.searchParams.get('name') ?? ''
      try {
        return json({ echoed: name }, 200, { 'x-echo': name })
      } catch {
        return json({ echoed: name, note: 'header rejected by runtime' })
      }
    }

    // Naive traversal — what path_probe / lfi_probe look for.
    if (p === '/api/v3/file') {
      const rel = url.searchParams.get('p') ?? ''
      try {
        const f = Bun.file(`${import.meta.dir}/public/${rel}`)
        if (await f.exists()) return new Response(f)
      } catch { /* fall through */ }
      return json({ error: 'not_found', p: rel }, 404)
    }

    if (p.startsWith('/api/v3/')) {
      const denied = await authorize(req, url, body)
      if (denied) return denied

      if (p === '/api/v3/profile') {
        return json({ id: 'u_42', name: 'Fixture User', role: 'admin', email: 'u42@example.test' })
      }
      if (p === '/api/v3/items') {
        const page = Number(url.searchParams.get('page') ?? '1')
        const size = 10
        return json({
          page,
          size,
          total: ITEMS.length,
          items: ITEMS.slice((page - 1) * size, page * size)
        })
      }
      // Reachable only by guessing — nothing in the page references it.
      if (p === '/api/v3/admin/keys') {
        return json({ keys: [{ id: 'k1', value: 'flag{undiscoverable-by-crawling}' }] })
      }
      return json({ error: 'no_such_endpoint', path: p }, 404)
    }

    return new Response('not found', { status: 404 })
  },

  websocket: {
    open(ws) {
      ws.send(JSON.stringify({ type: 'hello', server: 'rever-fixture', ts: Date.now() }))
      ws.subscribe('ticks')
    },
    message(ws, msg) {
      ws.send(JSON.stringify({ type: 'echo', received: String(msg) }))
    },
    close() {}
  }
})

console.log(`api-target listening on http://127.0.0.1:${PORT}`)
