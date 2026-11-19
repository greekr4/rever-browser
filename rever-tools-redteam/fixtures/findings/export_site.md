# Reversing Session Findings

_site: shop.test_

**2 confirmed · 2 to verify · 2 recon** _(1 duplicate report merged)_

## Confirmed (2)

### API key in bundle
_🟠 high · Secrets / Keys · status: validated_

key=abc

**Evidence:** `req-7`, `req-8`

### SQL injection in /v1/search q
_🟠 high · Vulnerabilities · status: validated · tags: sqli, db · request: `req-3`_

Error-based SQLi at https://api.shop.test/v1/search?q=1
contact ad****@shop.test or 010-****-****

**Evidence:** `req-4`, `e1`

**Repro:**
```
curl "https://api.shop.test/v1/search?q=1%27" -H "X-User: te****@shop.test"
```

## Needs verification (2)

_Unproven — this read-only scan could not confirm these. Check the code or a live request before acting; absence of a client-side control is not proof of a server-side one._

### Reflected XSS in /v1/echo msg
_🔴 critical · Vulnerabilities · status: candidate · tags: xss_

Payload reflected at https://api.shop.test/v1/echo

**Evidence:** `req-9`

### Missing rate limit on login
_🟡 medium · Auth Flow · status: candidate_

No lockout after 50 attempts.

## Attack surface & recon (2)

- **Legacy TLS accepted** _(🟡 medium, Other)_
- **shop.test item list** _(🟢 low, API Endpoints)_ · `req-1`
