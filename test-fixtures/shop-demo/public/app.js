// src/signing.ts
var HMAC_KEY = "amajon-price-signing-key-2026";
var API_BASE = "/api";
var enc = new TextEncoder;
function toHex(bytes) {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}
async function hmacSha256(key, message) {
  const k = await crypto.subtle.importKey("raw", enc.encode(key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const sig = await crypto.subtle.sign("HMAC", k, enc.encode(message));
  return toHex(new Uint8Array(sig));
}
function canonicalString(method, path, body, ts) {
  return `${method}
${path}
${body}
${ts}`;
}
async function signedHeaders(method, path, body, token) {
  const ts = String(Date.now());
  const sig = await hmacSha256(HMAC_KEY, canonicalString(method, path, body, ts));
  return {
    authorization: `Bearer ${token}`,
    "x-timestamp": ts,
    "x-signature": sig,
    "content-type": "application/json"
  };
}

// src/app.ts
var token = localStorage.getItem("auth.token") ?? "";
async function login() {
  const r = await fetch(`${API_BASE}/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user: "demo", pass: "demo" })
  });
  const data = await r.json();
  token = data.token;
  localStorage.setItem("auth.token", token);
  localStorage.setItem("auth.user", JSON.stringify(data.user));
}
async function fetchProducts() {
  if (!token)
    await login();
  const path = `${API_BASE}/products`;
  const headers = await signedHeaders("GET", path, "", token);
  const r = await fetch(path, { headers });
  if (!r.ok)
    throw new Error(`products ${r.status}`);
  const data = await r.json();
  return data.products;
}
var usd = (n) => "$" + n.toLocaleString("en-US");
function stars(rating) {
  const full = Math.round(rating);
  return "★★★★★☆☆☆☆☆".slice(5 - full, 10 - full);
}
function card(p) {
  const off = Math.round((1 - p.price / p.list) * 100);
  return `
    <article class="card" data-id="${p.id}">
      <div class="thumb" style="--hue:${p.hue}">
        <img src="/img/${p.id}.jpg?v=7" alt="${p.title}" loading="lazy"
             onerror="this.replaceWith(Object.assign(document.createElement('span'),{textContent:'${p.emoji}'}))" />
      </div>
      <div class="brand">${p.brand}</div>
      <h3 class="title">${p.title}</h3>
      <div class="rate"><span class="stars">${stars(p.rating)}</span>
        <span class="rev">${p.reviews.toLocaleString("en-US")}</span></div>
      <div class="pricerow">
        <span class="price">${usd(p.price)}</span>
        <span class="list">${usd(p.list)}</span>
        <span class="off">-${off}%</span>
      </div>
      ${p.prime ? '<div class="prime">✔ amajon Prime · FREE next-day</div>' : '<div class="ship">$3.99 shipping</div>'}
      <button class="cart-btn">Add to Cart</button>
    </article>`;
}
async function render() {
  const grid = document.getElementById("grid");
  if (!grid)
    return;
  try {
    const products = await fetchProducts();
    grid.innerHTML = products.map(card).join("");
  } catch (e) {
    grid.innerHTML = `<p class="err">Failed to load products: ${String(e)}</p>`;
  }
}
render();

//# debugId=B5B3EF2E571E417864756E2164756E21
//# sourceMappingURL=app.js.map
