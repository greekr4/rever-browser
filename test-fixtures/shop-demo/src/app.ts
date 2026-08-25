import { API_BASE, signedHeaders } from './signing'

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
interface CartLine { id: number; title: string; price: number; qty: number }
interface Order { id: string; items: CartLine[]; total: number; status: string; createdAt: string }

let token = localStorage.getItem('auth.token') ?? ''
let user: { id: string; name: string } | null = JSON.parse(localStorage.getItem('auth.user') ?? 'null')

const view = (): HTMLElement => document.getElementById('view') as HTMLElement
const usd = (n: number): string => '$' + n.toLocaleString('en-US')
const stars = (r: number): string => '★★★★★☆☆☆☆☆'.slice(5 - Math.round(r), 10 - Math.round(r))

// ── signed API helper (auto-logs-in a guest token for browsing) ───────────────
async function login(): Promise<void> {
  const r = await fetch(`${API_BASE}/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ user: 'demo', pass: 'demo' })
  })
  const data = await r.json()
  token = data.token
  user = data.user
  localStorage.setItem('auth.token', token)
  localStorage.setItem('auth.user', JSON.stringify(user))
}

async function api(method: string, path: string, bodyObj?: unknown): Promise<Response> {
  if (!token) await login()
  const body = bodyObj !== undefined ? JSON.stringify(bodyObj) : ''
  const headers = await signedHeaders(method, path, body, token)
  return fetch(path, { method, headers, body: body || undefined })
}

async function refreshBadge(): Promise<void> {
  try {
    const r = await api('GET', `${API_BASE}/cart`)
    const d = await r.json()
    const el = document.getElementById('cart-count')
    if (el) el.textContent = String(d.count ?? 0)
  } catch {
    /* ignore */
  }
}

function syncAcct(): void {
  const acct = document.getElementById('acct')
  if (acct && user) acct.querySelector('small')!.textContent = `Hello, ${user.name.split(' ')[0]}`
}

// ── views ─────────────────────────────────────────────────────────────────────
function productCard(p: Product): string {
  const off = Math.round((1 - p.price / p.list) * 100)
  return `
    <article class="card" data-id="${p.id}">
      <a class="thumb" style="--hue:${p.hue}" href="/product/${p.id}" data-link>
        <img src="/img/${p.id}.jpg?v=7" alt="${p.title}" loading="lazy"
             onerror="this.replaceWith(Object.assign(document.createElement('span'),{textContent:'${p.emoji}'}))" />
      </a>
      <div class="brand">${p.brand}</div>
      <a class="title" href="/product/${p.id}" data-link>${p.title}</a>
      <div class="rate"><span class="stars">${stars(p.rating)}</span>
        <span class="rev">${p.reviews.toLocaleString('en-US')}</span></div>
      <div class="pricerow">
        <span class="price">${usd(p.price)}</span>
        <span class="list">${usd(p.list)}</span>
        <span class="off">-${off}%</span>
      </div>
      ${p.prime ? '<div class="prime">✔ amajon Prime · FREE next-day</div>' : '<div class="ship">$3.99 shipping</div>'}
      <button class="cart-btn" data-add="${p.id}">Add to Cart</button>
    </article>`
}

async function renderHome(): Promise<void> {
  view().innerHTML = `
    <section class="hero">
      <div class="hero-text">
        <p class="hero-kicker">Today's Deals</p>
        <h1>Popular electronics<br />at every price</h1>
        <a class="hero-cta" href="#deals">Shop now</a>
      </div>
    </section>
    <section id="deals" class="deals">
      <div class="deals-head">
        <h2>Featured products</h2>
        <span class="deals-note">Prices are loaded from a signed private API</span>
      </div>
      <div id="grid" class="grid"><p class="loading">Loading products…</p></div>
    </section>`
  try {
    const r = await api('GET', `${API_BASE}/products`)
    const data = await r.json()
    document.getElementById('grid')!.innerHTML = (data.products as Product[]).map(productCard).join('')
  } catch (e) {
    document.getElementById('grid')!.innerHTML = `<p class="err">Failed to load products: ${String(e)}</p>`
  }
}

async function renderProduct(id: number): Promise<void> {
  view().innerHTML = `<div class="pdp"><p class="loading">Loading…</p></div>`
  try {
    const r = await api('GET', `${API_BASE}/product/${id}`)
    if (!r.ok) throw new Error(`product ${r.status}`)
    const p = (await r.json()) as Product
    const off = Math.round((1 - p.price / p.list) * 100)
    view().innerHTML = `
      <nav class="crumbs"><a href="/" data-link>amajon</a> › <span>${p.brand}</span> › ${p.title}</nav>
      <div class="pdp">
        <div class="pdp-media">
          <img src="/img/${p.id}.jpg?v=7" alt="${p.title}"
               onerror="this.replaceWith(Object.assign(document.createElement('span'),{className:'pdp-emoji',textContent:'${p.emoji}'}))" />
        </div>
        <div class="pdp-info">
          <div class="brand">${p.brand}</div>
          <h1>${p.title}</h1>
          <div class="rate"><span class="stars">${stars(p.rating)}</span>
            <span class="rev">${p.reviews.toLocaleString('en-US')} ratings</span></div>
          <hr />
          <div class="pricerow big">
            <span class="off">-${off}%</span>
            <span class="price">${usd(p.price)}</span>
            <span class="list">List: ${usd(p.list)}</span>
          </div>
          ${p.prime ? '<div class="prime">✔ amajon Prime · FREE next-day delivery</div>' : '<div class="ship">$3.99 shipping</div>'}
          <p class="pdp-desc">Ships from and sold by amajon.com. This is a demo product on a
            fictional storefront used to reverse-engineer a signed price API.</p>
        </div>
        <aside class="buybox">
          <div class="buybox-price">${usd(p.price)}</div>
          <div class="buybox-prime">${p.prime ? 'FREE next-day delivery' : '$3.99 shipping'}</div>
          <div class="instock">In Stock</div>
          <button class="cart-btn" data-add="${p.id}">Add to Cart</button>
          <button class="buy-btn" data-buy="${p.id}">Buy Now</button>
        </aside>
      </div>`
  } catch (e) {
    view().innerHTML = `<div class="pdp"><p class="err">Failed to load product: ${String(e)}</p></div>`
  }
}

async function renderCart(): Promise<void> {
  view().innerHTML = `<div class="page"><p class="loading">Loading cart…</p></div>`
  const r = await api('GET', `${API_BASE}/cart`)
  const d = await r.json()
  const lines = (d.items ?? []) as CartLine[]
  const rows = lines.length
    ? lines
        .map(
          (l) => `
      <div class="cart-line">
        <img src="/img/${l.id}.jpg?v=7" alt="" onerror="this.style.visibility='hidden'" />
        <div class="cart-line-info">
          <a href="/product/${l.id}" data-link class="title">${l.title}</a>
          <div class="prime">In Stock · Qty ${l.qty}</div>
          <button class="link-btn" data-remove="${l.id}">Delete</button>
        </div>
        <div class="cart-line-price">${usd(l.price * l.qty)}</div>
      </div>`
        )
        .join('')
    : `<p class="empty">Your amajon Cart is empty. <a href="/" data-link>Shop deals</a></p>`
  view().innerHTML = `
    <div class="cartpage">
      <div class="cart-main">
        <h1>Shopping Cart</h1>
        ${rows}
      </div>
      <aside class="cart-summary">
        <div class="subtotal">Subtotal (${d.count ?? 0} items): <strong>${usd(d.total ?? 0)}</strong></div>
        <button class="cart-btn" id="checkout" ${lines.length ? '' : 'disabled'}>Proceed to checkout</button>
      </aside>
    </div>`
}

async function renderLogin(): Promise<void> {
  view().innerHTML = `
    <div class="authwrap">
      <a class="auth-logo" href="/" data-link><span class="logo-word">amajon</span><span class="logo-tld">.com</span></a>
      <form class="authbox" id="loginform">
        <h1>Sign in</h1>
        <label>Email or mobile phone number
          <input type="text" id="email" value="demo@amajon.com" />
        </label>
        <label>Password
          <input type="password" id="pass" value="demo1234" />
        </label>
        <button class="cart-btn" type="submit">Sign in</button>
        <p class="auth-fine">By signing in you agree to amajon's (demo) Conditions of Use.</p>
      </form>
      <div class="auth-new"><span>New to amajon?</span>
        <button class="ghost-btn" id="createacct">Create your amajon account</button></div>
    </div>`
}

async function renderOrders(): Promise<void> {
  view().innerHTML = `<div class="page"><p class="loading">Loading orders…</p></div>`
  const r = await api('GET', `${API_BASE}/orders`)
  const d = await r.json()
  const orders = (d.orders ?? []) as Order[]
  const rows = orders.length
    ? orders
        .map(
          (o) => `
      <div class="order">
        <div class="order-head">
          <div><small>ORDER PLACED</small><div>${o.createdAt.slice(0, 10)}</div></div>
          <div><small>TOTAL</small><div>${usd(o.total)}</div></div>
          <div><small>ORDER #</small><div>${o.id}</div></div>
          <div class="order-status status-${o.status}">${o.status.toUpperCase()}</div>
        </div>
        <div class="order-items">
          ${o.items
            .map(
              (l) =>
                `<div class="order-item"><img src="/img/${l.id}.jpg?v=7" onerror="this.style.visibility='hidden'" /><a href="/product/${l.id}" data-link class="title">${l.title}</a> ×${l.qty}</div>`
            )
            .join('')}
        </div>
        ${o.status === 'placed' ? `<button class="ghost-btn" data-cancel="${o.id}">Cancel order</button>` : ''}
      </div>`
        )
        .join('')
    : `<p class="empty">No orders yet. <a href="/" data-link>Start shopping</a></p>`
  view().innerHTML = `<div class="page"><h1>Your Orders</h1>${rows}</div>`
}

// ── router ──────────────────────────────────────────────────────────────────
async function route(): Promise<void> {
  const path = location.pathname
  const pm = path.match(/^\/product\/(\d+)$/)
  if (path === '/' || path === '') await renderHome()
  else if (pm) await renderProduct(Number(pm[1]))
  else if (path === '/cart') await renderCart()
  else if (path === '/login') await renderLogin()
  else if (path === '/orders') await renderOrders()
  else await renderHome()
  syncAcct()
  window.scrollTo(0, 0)
}

function navigate(path: string): void {
  history.pushState({}, '', path)
  void route()
}

// intercept internal links
document.addEventListener('click', (e) => {
  const t = e.target as HTMLElement
  const link = t.closest('a[data-link]') as HTMLAnchorElement | null
  if (link) {
    const href = link.getAttribute('href') || '/'
    if (href.startsWith('#')) return
    e.preventDefault()
    navigate(href)
    return
  }
  // add to cart (home + pdp)
  const add = t.closest('[data-add]') as HTMLElement | null
  if (add) {
    e.preventDefault()
    void api('POST', `${API_BASE}/cart`, { productId: Number(add.dataset.add), qty: 1 }).then(() =>
      refreshBadge()
    )
    return
  }
  // buy now -> add then go to cart
  const buy = t.closest('[data-buy]') as HTMLElement | null
  if (buy) {
    e.preventDefault()
    void api('POST', `${API_BASE}/cart`, { productId: Number(buy.dataset.buy), qty: 1 }).then(() => {
      void refreshBadge()
      navigate('/cart')
    })
    return
  }
  // remove from cart
  const rm = t.closest('[data-remove]') as HTMLElement | null
  if (rm) {
    e.preventDefault()
    void api('POST', `${API_BASE}/cart/remove`, { productId: Number(rm.dataset.remove) }).then(() => {
      void refreshBadge()
      void renderCart()
    })
    return
  }
  // checkout
  if (t.closest('#checkout')) {
    e.preventDefault()
    void api('POST', `${API_BASE}/orders`, {}).then(async (r) => {
      if (r.ok) {
        await refreshBadge()
        navigate('/orders')
      }
    })
    return
  }
  // cancel order
  const cancel = t.closest('[data-cancel]') as HTMLElement | null
  if (cancel) {
    e.preventDefault()
    void api('POST', `${API_BASE}/order/${cancel.dataset.cancel}/cancel`, {}).then(() => renderOrders())
    return
  }
})

// login form submit
document.addEventListener('submit', (e) => {
  if ((e.target as HTMLElement).id === 'loginform') {
    e.preventDefault()
    void login().then(() => navigate('/'))
  }
})

window.addEventListener('popstate', () => void route())

void (async () => {
  await route()
  void refreshBadge()
})()
