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
}

let token = localStorage.getItem('auth.token') ?? ''

async function login(): Promise<void> {
  const r = await fetch(`${API_BASE}/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ user: 'demo', pass: 'demo' })
  })
  const data = await r.json()
  token = data.token
  localStorage.setItem('auth.token', token)
  localStorage.setItem('auth.user', JSON.stringify(data.user))
}

// The private, HMAC-signed price feed — the request a reverser wants.
async function fetchProducts(): Promise<Product[]> {
  if (!token) await login()
  const path = `${API_BASE}/products`
  const headers = await signedHeaders('GET', path, '', token)
  const r = await fetch(path, { headers })
  if (!r.ok) throw new Error(`products ${r.status}`)
  const data = await r.json()
  return data.products as Product[]
}

const usd = (n: number): string => '$' + n.toLocaleString('en-US')

function stars(rating: number): string {
  const full = Math.round(rating)
  return '★★★★★☆☆☆☆☆'.slice(5 - full, 10 - full)
}

function card(p: Product): string {
  const off = Math.round((1 - p.price / p.list) * 100)
  return `
    <article class="card" data-id="${p.id}">
      <div class="thumb" style="--hue:${p.hue}">
        <img src="/img/${p.id}.jpg?v=7" alt="${p.title}" loading="lazy"
             onerror="this.replaceWith(Object.assign(document.createElement('span'),{textContent:'${p.emoji}'}))" />
      </div>
      <div class="brand">${p.brand}</div>
      <h3 class="title">${p.title}</h3>
      <div class="rate"><span class="stars">${stars(p.rating)}</span>
        <span class="rev">${p.reviews.toLocaleString('en-US')}</span></div>
      <div class="pricerow">
        <span class="price">${usd(p.price)}</span>
        <span class="list">${usd(p.list)}</span>
        <span class="off">-${off}%</span>
      </div>
      ${p.prime ? '<div class="prime">✔ amajon Prime · FREE next-day</div>' : '<div class="ship">$3.99 shipping</div>'}
      <button class="cart-btn">Add to Cart</button>
    </article>`
}

async function render(): Promise<void> {
  const grid = document.getElementById('grid')
  if (!grid) return
  try {
    const products = await fetchProducts()
    grid.innerHTML = products.map(card).join('')
  } catch (e) {
    grid.innerHTML = `<p class="err">Failed to load products: ${String(e)}</p>`
  }
}

void render()
