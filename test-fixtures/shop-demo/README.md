# shop-demo — "nile" storefront (reverse-engineering demo target)

An **Amazon-style** storefront used as a relatable demo target for rever-browser.
`nile` is a **fictional brand** (an Amazon parody), not a real company — safe to
show in a public demo.

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
| HMAC key | `amajon-price-signing-key-2026` |
| JWT secret | `amajon-hs256-secret` |
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

## Product images

Product images are **not committed** to the repo. They are CC-licensed stock from
LoremFlickr (some NC/ND, with attribution watermarks) — fine for local demo use,
but not for redistribution. Get them one of two ways:

- `bash download-images.sh` — re-fetch from LoremFlickr into `public/img/`.
- Drop your own / CC0 photos as `public/img/1.jpg … 12.jpg`.

If an image is missing, the card falls back to an emoji thumbnail (see
`src/app.ts`). For a shipped/public demo video, prefer CC0 or your own photos.
