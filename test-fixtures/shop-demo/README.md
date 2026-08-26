# shop-demo — "reverzon" storefront (security self-audit demo target)

An **Amazon-style** storefront used as a relatable demo target for rever-browser.
`reverzon` is a **fictional brand** (an Amazon parody), not a real company — safe to
show in a public demo. It is **deliberately vulnerable** (see below).

**Why it exists:** a shopper's product/price feed is loaded through a *private,
HMAC-signed* API — no public API, requests are signed client-side. The demo goal
is the relatable pain "I want a price-tracker for this shop, but it has no API and
everything is signed." rever recovers the signing (via `crypto_trace`), replays a
request (Repeater), and exports a working Python client.

## Run

```bash
bun test-fixtures/shop-demo/server.ts        # http://127.0.0.1:8780
bash test-fixtures/shop-demo/download-images.sh   # fetch product images (see below)
# after editing src/, rebuild the client bundle:
cd test-fixtures/shop-demo && bun build src/app.ts --outdir public --sourcemap=linked --entry-naming app.js
```

## Ground truth (everything checkable)

| | |
|---|---|
| HMAC key | `reverzon-price-signing-key-2026` |
| JWT secret | `reverzon-hs256-secret` |
| Signature input | `` `${method}\n${path}\n${body}\n${ts}` `` → headers `x-timestamp`, `x-signature` |
| Product feed (signed) | `GET /api/products` |
| Product detail (signed) | `GET /api/product/:id` — carries the hidden `dealPrice` |
| Login | `POST /api/login` → JWT |
| Deal of the day | product `7` hides `dealPrice: 99` (only in detail) |

### Commerce endpoints (all signed, in-memory)

| Method | Path | Purpose |
|---|---|---|
| POST | `/api/cart` | add `{ productId, qty? }` → returns cart + count |
| GET | `/api/cart` | view cart (items, total) |
| POST | `/api/cart/remove` | remove `{ productId }` |
| POST | `/api/orders` | place order from cart → `201` `ord_N` (clears cart) |
| GET | `/api/orders` | list orders |
| GET | `/api/order/:id` | order detail |
| POST | `/api/order/:id/cancel` | cancel (`409` if already cancelled) |

## Seeded vulnerabilities (intentional — the audit ground truth)

This is a **deliberately vulnerable** target for a security self-audit demo. Every
hole below is planted on purpose and documented so a tool's finding is checkable,
not guessed. None of this is how a real store should be built.

| # | Vulnerability | Class (OWASP) | How to demonstrate |
|---|---|---|---|
| V1 | **Signing key shipped in the client** — `reverzon-price-signing-key-2026` is in the JS bundle (`src/signing.ts` → `app.js`), so the HMAC "signature" gate is theater: anyone can forge valid requests. | Cryptographic failure / hardcoded secret | White-box: grep the bundle / `crypto_trace`. Black-box: forge a signed request → `200`. |
| V2 | **IDOR on orders** — `GET /api/order/:id` verifies the token but never checks ownership; ids are sequential (`ord_1000`, `ord_1001`, …). | Broken object-level auth (BOLA) | Request `ord_1000` / `ord_1001` → other customers' orders + emails (PII). |
| V3 | **Unauthenticated admin report** — `GET /api/admin/report` has no role check and is linked from nowhere. | Broken function-level auth | Guess the path → revenue, all orders, customer emails. |
| V4 | **Excessive data exposure** — `GET /api/product/:id` leaks internal `dealPrice`, `cost`, `margin` the UI never shows. | Excessive data exposure | Compare the detail JSON to what the page renders. |
| V5 | **Unscoped order list** — `GET /api/orders` returns *every* customer's orders, not just the caller's. | Broken object-level auth | The Orders page shows other people's orders. |
| V6 | **Bearer token never validated** — `authorize()` only checks the header starts with `Bearer `; the JWT signature and `exp` are never verified, and `x-timestamp` is folded into the signature but never checked for freshness (no replay window). | Identification & auth failures | Any string as the Bearer token + a valid HMAC sig → `200`; old signatures never expire. |

Ground truth for V1 signing is the same key/scheme listed above; V2/V3/V5 leak the
seeded emails `j.harper@gmail.com` and `m.tan@outlook.com`. V6: a bogus token such
as `Bearer not-a-jwt` with a valid signature still returns `200`.

## Pages (client-side SPA)

A tiny history-API router in `src/app.ts` renders these views (the Bun server
serves the app shell for any non-`/api` route):

- `/` — home (hero + product grid)
- `/product/:id` — product detail (breadcrumb, image, buy box)
- `/cart` — shopping cart (delete, subtotal, checkout)
- `/login` — Amazon-style sign-in
- `/orders` — order history (cancel order)

Every view calls the signed API, so navigating the store naturally exercises the
whole endpoint surface for a reversing demo.

## Product images

Product images are **not committed** to the repo. They are CC-licensed stock from
LoremFlickr (some NC/ND, with attribution watermarks) — fine for local demo use,
but not for redistribution. Get them one of two ways:

- `bash download-images.sh` — re-fetch from LoremFlickr into `public/img/`.
- Drop your own / CC0 photos as `public/img/1.jpg … 12.jpg`.

If an image is missing, the card falls back to an emoji thumbnail (see
`src/app.ts`). For a shipped/public demo video, prefer CC0 or your own photos.
