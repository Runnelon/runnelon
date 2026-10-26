# Contributing to Runnelon

Thanks for considering a contribution. Runnelon is early-stage; discuss large changes in an issue before implementing them.

## Workflow
1. Check `ROADMAP.md`, `AGENTS.md`, and relevant ADRs.
2. Open or claim an issue with observable acceptance criteria.
3. Create a focused branch; include tests and documentation.
4. Run:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo build --workspace
   ```
5. Submit a PR explaining behavior, tests, risks, and compatibility impact.

## Design expectations
- Favor correctness and secure defaults over premature optimization.
- Keep networking behavior testable using local mock upstreams.
- Do not make unverified performance or security claims.
- Architectural changes should include an ADR.

## Conduct
Be respectful and constructive. A formal code of conduct may be added as the community grows.

## Licensing
By contributing, you agree that your contribution will be distributed under the [Apache License, Version 2.0](LICENSE).
