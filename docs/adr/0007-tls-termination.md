# ADR 0007: TLS termination via Pingora/OpenSSL

- **Status:** Accepted
- **Date:** 2026-10-27

## Context
Runnelon needs to terminate TLS for real deployments. Pingora 0.9 supports TLS through OpenSSL or BoringSSL features; this workspace already depends on the system OpenSSL toolchain. Certificate rotation and SNI multi-cert routing are separate operational problems.

## Decision
- Enable `pingora`'s `openssl` feature and terminate TLS per listener:
  ```yaml
  listeners:
    - address: "0.0.0.0:8443"
      tls:
        cert_path: "/etc/runnelon/tls/cert.pem"
        key_path: "/etc/runnelon/tls/key.pem"
  ```
- Validate that both files are readable at configuration load; invalid PEM material fails startup through Pingora's TLS setup error.
- Each listener gets its own Pingora service instance carrying its scheme (`http`/`https`), so `X-Forwarded-Proto` reflects the actual downstream protocol.
- Use Pingora's `TlsSettings::intermediate` defaults; no custom cipher/version/cipher-suite configuration in this milestone.
- Certificate and key changes require a restart. Config reload compares TLS paths and rejects TLS-listener changes, like other listener changes.
- Plain HTTP listeners remain supported and can coexist; there is no automatic HTTP→HTTPS redirect.
- No SNI-based multi-certificate selection; one cert/key per listener.

## Consequences
- Deployment gains encrypted client traffic with modern defaults.
- The binary links system OpenSSL; container images must include compatible OpenSSL libraries.
- Certificate rotation is manual (restart), and there is no ACME automation.
- HSTS, redirect policy, client certificates (mTLS), and per-SNI certificates remain unimplemented and must not be claimed.
- Handshake failures are per-connection events; they do not affect other listeners.

## Alternatives
- BoringSSL: larger build complexity; revisit if FIPS/BoringSSL-specific needs appear.
- Rustls: not supported by the pinned Pingora TLS path.
- Terminating TLS in a sidecar: valid deployment option, but leaves Runnelon unable to set `X-Forwarded-Proto` itself.
