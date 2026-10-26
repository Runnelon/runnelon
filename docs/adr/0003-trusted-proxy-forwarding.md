# ADR 0003: Trusted-proxy handling for `X-Forwarded-For`

- **Status:** Accepted
- **Date:** 2026-10-26

## Context
Any client can forge `X-Forwarded-*` headers. Runnelon therefore replaces them with values it observed itself. Deployments behind an edge proxy or load balancer, however, need the original client address preserved so upstreams can log it and apply IP-based policy.

## Decision
- Add an optional top-level `trusted_proxies` list of IPv4/IPv6 CIDR networks. A bare address means `/32` or `/128`; host bits are masked.
- Empty by default: inbound `X-Forwarded-For` is discarded and replaced with the observed peer address.
- When the peer belongs to a trusted network, the inbound chain is preserved and the peer address appended, matching conventional multi-hop proxy semantics.
- `X-Forwarded-Proto` and `X-Forwarded-Host` are always derived from what Runnelon observed; trusted peers cannot override them in M1.
- Invalid or duplicate networks fail configuration validation before the listener starts.

## Consequences
- Default operation stays fail-closed; multi-hop deployments must enumerate every trusted hop explicitly.
- The list is a security boundary: a network that includes untrusted clients re-enables spoofing.
- Preserved chains are only as trustworthy as their rightmost untrusted entry; upstreams must parse `X-Forwarded-For` from right to left.
- `X-Forwarded-Proto` remains `http` until TLS termination exists, even for trusted peers.

## Alternatives
- Always append (nginx `$proxy_add_x_forwarded_for`): simple, but trusts every client by default.
- Never preserve chains: safest, but unusable behind an edge proxy.
- Support RFC 7239 `Forwarded` instead: deferred until there is demand; `X-Forwarded-For` remains the de facto standard.
