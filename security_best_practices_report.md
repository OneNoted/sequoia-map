# Security Best Practices Audit Report

Date: 2026-03-03

## Executive Summary

This review covered the Rust server (`server`), ingest service (`services/sequoia-ingest`), and deployment edge/runtime configs (`ops`, Docker Compose, Dockerfiles). The codebase has solid baseline controls (token-based auth for internal ingest, request body limits, rate limiting, signed-request checks, parameterized SQL, and no dependency advisories from `cargo audit`), but there are several important gaps.

Top risks are:

1. Unbounded persistence of attestation challenges can be abused for storage-exhaustion DoS.
2. Production server container runs as root.
3. Production compose defaults allow single-reporter degraded ingest updates, reducing integrity guarantees.
4. Proxy-trust defaults are broad and can weaken IP-based controls in shared private networks.

---

## Critical / High

### SBP-001 (High): `attestation_challenges` can grow without bound (storage DoS risk)

- Impact: A distributed attacker can continuously hit `/v1/attest/challenge` and force unbounded SQLite growth, eventually exhausting disk and degrading or stopping ingest service availability.
- Evidence:
  - Challenge endpoint is public and persists each challenge:
    - `services/sequoia-ingest/src/main.rs:696`
    - `services/sequoia-ingest/src/main.rs:737`
  - Persistence inserts every challenge row:
    - `services/sequoia-ingest/src/main.rs:3162`
    - `services/sequoia-ingest/src/main.rs:3167`
  - Retention purge deletes `raw_reports` and `reporters`, but not `attestation_challenges`:
    - `services/sequoia-ingest/src/main.rs:2621`
    - `services/sequoia-ingest/src/main.rs:2623`
    - `services/sequoia-ingest/src/main.rs:2630`
- Recommendation:
  - Add DB cleanup for expired/used attestation challenges in `purge_expired`.
  - Add an index on `expires_at` (and optionally `used_at`) to keep cleanup efficient.
  - Consider challenge issuance quotas per device key hash in addition to IP rate limits.

### SBP-002 (High): Main server runtime container runs as root

- Impact: Any remote code execution in the server process executes as root in-container, increasing blast radius and container-breakout impact.
- Evidence:
  - Server runtime image has no non-root user and no `USER` directive:
    - `Dockerfile:75`
    - `Dockerfile:88`
  - Ingest runtime image already uses a dedicated non-root user:
    - `services/sequoia-ingest/Dockerfile:20`
    - `services/sequoia-ingest/Dockerfile:29`
- Recommendation:
  - Mirror ingest hardening in server runtime image:
    - create dedicated UID/GID,
    - chown runtime directories,
    - set `USER` for final stage.

---

## Medium

### SBP-003 (Medium): Production compose defaults reduce quorum integrity guarantees

- Impact: A single active reporter can influence canonical updates in degraded mode if quorum is unavailable, increasing integrity risk from compromised/malicious reporters.
- Evidence:
  - Production compose default:
    - `docker-compose.yml:67`
  - Coolify compose default:
    - `docker-compose.coolify.yml:69`
  - Project docs explicitly note this default:
    - `README.md:105`
    - `README.md:201`
- Recommendation:
  - Set production defaults to `false` and require explicit opt-in for degraded mode.
  - Alert whenever degraded mode is enabled.
  - If kept enabled, tighten corroboration and shorten acceptance windows.

### SBP-004 (Medium): Trusted-proxy defaults are broad (`RFC1918` ranges)

- Impact: Any host inside trusted private ranges can influence `X-Forwarded-For` trust logic, weakening IP-based rate-limit/quarantine attribution in shared private networks.
- Evidence:
  - Broad defaults in compose:
    - `docker-compose.yml:70`
    - `docker-compose.coolify.yml:72`
  - `X-Forwarded-For` is trusted based on configured proxy CIDRs:
    - `services/sequoia-ingest/src/main.rs:1823`
    - `services/sequoia-ingest/src/main.rs:1866`
- Recommendation:
  - Restrict `INGEST_TRUSTED_PROXY_CIDRS` to exact edge proxy addresses/subnets.
  - Avoid blanket RFC1918 trust unless the private network is fully controlled.

---

## Low

### SBP-005 (Low): Missing browser security headers at edge

- Impact: Reduced defense-in-depth against clickjacking and MIME-sniffing; weaker posture if future UI XSS is introduced.
- Evidence:
  - Edge config proxies traffic but does not set security headers:
    - `ops/caddy/Caddyfile:5`
    - `ops/caddy/Caddyfile:34`
    - `ops/caddy/Caddyfile.coolify:1`
- Recommendation:
  - Add headers at edge for at least:
    - `X-Content-Type-Options: nosniff`
    - `X-Frame-Options: DENY` (or CSP `frame-ancestors 'none'`)
    - `Referrer-Policy: strict-origin-when-cross-origin`
    - a scoped `Content-Security-Policy` appropriate for the app.

### SBP-006 (Low): Operational metrics endpoints are publicly reachable by default routing

- Impact: Enables reconnaissance (traffic patterns, rejection counters, enrollment activity) that can assist targeted abuse.
- Evidence:
  - Server exposes `/api/metrics` route:
    - `server/src/app.rs:65`
  - Ingest exposes `/metrics` route:
    - `services/sequoia-ingest/src/main.rs:633`
  - Edge routes can expose ingest directly on iris domain:
    - `ops/caddy/Caddyfile:34`
    - `ops/caddy/Caddyfile:39`
- Recommendation:
  - Protect metrics endpoints with network ACL, auth, or private-only routing.

---

## Positive Controls Observed

- Internal ingest token required and validated with minimum length / placeholder rejection:
  - `server/src/config.rs:104`
  - `services/sequoia-ingest/src/main.rs:256`
- Constant-time comparison for server internal ingest token check:
  - `server/src/routes/ingest.rs:342`
  - `server/src/routes/ingest.rs:356`
- Request body limits are configured on both services:
  - `server/src/app.rs:95`
  - `services/sequoia-ingest/src/main.rs:638`
- Signed request verification with nonce replay protection:
  - `services/sequoia-ingest/src/main.rs:1401`
  - `services/sequoia-ingest/src/main.rs:1467`
- Dependency advisories check:
  - `cargo audit` completed for workspace and ingest crate with no reported advisories.

