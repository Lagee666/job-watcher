# AGENTS.md

## Purpose

This file defines the mandatory rules for AI coding agents working on this repository.

Read this file before modifying code.

---

## Required Context

Before making changes, determine which documentation is relevant.

Always read:

* `README.md`
* `AGENTS.md`

Read `docs/architecture.md` when:

* adding a feature
* adding a module
* changing data flow
* changing traits or domain models
* changing subsystem boundaries

Read `docs/104-source.md` when:

* modifying 104 integration
* changing HTTP requests
* changing parsing logic
* changing pagination
* investigating 104 response schemas

Do not assume repository architecture from source code alone when documentation exists.

---

## Engineering Priorities

Prioritize in this order:

1. Correctness
2. Reliability
3. Testability
4. Maintainability
5. Clear architecture
6. Resource efficiency
7. Performance
8. Style

Do not optimize without evidence of a real bottleneck.

---

## Architecture

Preserve the primary dependency flow:

```text
External Job Platform
        │
        ▼
    JobSource
        │
        ▼
      Domain
        │
        ▼
 Change Detection
        │
        ├── Repository
        │
        └── Notifier
```

Platform-specific models must not leak into the domain layer.

104-specific code belongs in the 104 source implementation.

The repository and notifier must not depend on 104-specific types.

---

## Rust Guidelines

Prefer idiomatic Rust.

### Error Handling

Production paths must not rely on unchecked:

```rust
unwrap()
expect()
```

Use `Result` and meaningful error propagation.

Prefer:

* `thiserror` for subsystem/domain errors
* `anyhow::Context` at application boundaries when appropriate

Errors should preserve enough context to identify the failing operation.

### Async

Never block Tokio executor threads.

Avoid synchronous network or filesystem operations inside async execution unless explicitly isolated.

### Ownership

Avoid unnecessary:

* `clone()`
* allocations
* intermediate collections

Do not sacrifice readability merely to eliminate insignificant allocations.

### Naming

Names must describe intent.

Avoid vague names such as:

```text
data
info
handler
manager
process
do_work
```

when a more precise domain name exists.

Boolean functions should read as predicates when practical.

---

## Dependencies

Do not introduce a dependency unless it solves a concrete problem.

Before adding a crate:

1. Check whether the standard library or an existing dependency already solves the problem.
2. Explain why the dependency is required.
3. Prefer mature and maintained crates.

Avoid introducing frameworks for small problems.

---

## Testing

Behavior changes require tests.

Prefer:

```text
Unit tests
    ↓
Fixture-based parser tests
    ↓
Repository integration tests
    ↓
Live external integration tests
```

Most tests must not require access to 104.

104 parser tests should use stored fixtures.

Live network tests must be optional.

---

## External Services

Treat undocumented external endpoints as unstable.

Never assume an undocumented 104 endpoint is a public API contract.

104-specific assumptions must be documented in:

`docs/104-source.md`

Avoid:

* authentication bypasses
* CAPTCHA bypasses
* aggressive crawling
* unnecessary request volume

---

## Scope Control

Do not implement future roadmap features unless requested.

In particular, do not introduce:

* Kubernetes
* distributed databases
* microservices
* web dashboards
* LLM integration
* browser automation

unless the current requirement actually needs them.

Prefer the smallest architecture that cleanly solves the current problem.

---

## Documentation Maintenance

Documentation is part of the implementation.

When a change affects documented behavior, update the corresponding documentation in the same change.

Examples:

Architecture changes:

→ update `docs/architecture.md`

104 integration changes:

→ update `docs/104-source.md`

User-facing setup or behavior changes:

→ update `README.md`

Do not update documentation merely because code formatting or internal implementation details changed.

---

## Verification

Before completing implementation, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

If a command cannot be executed, explicitly report:

* which command was not run
* why it could not run
* what remains unverified

Never claim tests passed unless they were actually executed.

---

## Agent Completion Report

When completing a task, summarize:

1. What changed
2. Architectural decisions
3. Tests added or modified
4. Documentation updated
5. Commands executed
6. Remaining risks or unresolved questions

Do not hide failures or unresolved issues.

---

## Job Source Documentation

Every external job platform must have a corresponding document:

- `docs/104-source.md`
- `docs/linkedin-source.md`
- `docs/cake-source.md`

When adding a new job source:

1. Create a source-specific implementation under `src/source/`.
2. Implement the existing `JobSource` abstraction.
3. Normalize platform-specific data into the domain `Job` model.
4. Do not introduce platform-specific fields into the core domain unless they
   provide meaningful cross-platform value.
5. Add a corresponding `docs/<source>-source.md`.
6. Document:
   - acquisition method
   - endpoint or HTML structure
   - pagination
   - authentication requirements
   - rate limits
   - response schema
   - known instability
   - unresolved questions
7. Add fixture-based parser tests.