# ADR 0004: Bounded retry for idempotent requests

- **Status:** Accepted
- **Date:** 2026-10-26

## Context
Passive health checks park an endpoint after a connect failure, but the request that discovers the failure still returns `502`. Retrying that request improves availability. Retrying non-idempotent requests, however, changes observable semantics and can duplicate side effects.

## Decision
- Allow at most two total connection attempts per request (initial attempt plus one retry).
- Retry only after a **connect** failure, where the request never reached an upstream.
- Retry only idempotent methods: `GET`, `HEAD`, `OPTIONS`, `TRACE`, `PUT`, `DELETE`.
- Never retry `POST`, `PATCH`, `CONNECT`, or any other method.
- The retry must select an endpoint that has not been tried for this request and is not inside its health cooldown. If no such endpoint exists, return the original error.
- Errors after a connection is established (read/write timeout, protocol errors) are not retried in this milestone.
- The attempt budget is a constant, not configuration, until operators show a need to tune it.

## Consequences
- A dead endpoint costs one failed attempt, and an idempotent request still succeeds on a healthy peer.
- Non-idempotent requests keep strict at-most-once connection semantics.
- Retries are bounded and cannot loop; Pingora's own `max_retries` remains a separate fail-safe.
- Request bodies are not replayed because connect failures happen before body streaming.
- The retry path consumes one extra schedule slot, so weighted distribution shifts slightly around failures.

## Alternatives
- Retry on any error: risks duplicate side effects and requires request-body buffering.
- Configurable retry budgets: deferred until operators need tuning.
- Rely on client retries: simpler, but leaves avoidable `502` responses.
