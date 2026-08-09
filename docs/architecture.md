# Architecture

## Overview

Rust Job Watcher is a long-running service.

It performs one synchronization cycle at startup and then at 07:00 and 17:00
in the machine's local timezone.

The application maintains the schedule internally.

The current implementation is intentionally contained in two source files:
`main.rs` starts Axum and coordinates the Tokio tasks; `watcher.rs` contains the
blocking Chromium extraction, scheduler, change comparison, and SQLite writes.

The public HTTP endpoint is a health endpoint only. Job data is acquired by the
background watcher, not by an Axum request handler.

## Data Flow

```text
In-process scheduler
      │
      ▼
  Application
      │
      ▼
  JobSource
      │
      ▼
External Platform
      │
      ▼
Platform Response
      │
      ▼
Normalization
      │
      ▼
   Extracted job records
      │
      ▼
Compare with SQLite
      │
      ├── New
      ├── Updated
      └── Unchanged
      │
      ▼
  SQLite (`jobs.sqlite3`)

New / Updated
      │
      ▼
   Log output + LINE notifier
```

## Domain

The core domain must remain independent from external job platforms.

Example:

```rust
pub struct Job {
    pub source: JobSourceId,
    pub external_id: String,

    pub title: String,
    pub company: String,

    pub location: Option<String>,
    pub salary: Option<String>,
    pub description: String,

    pub url: String,

    pub published_at: Option<DateTime<Utc>>,
    pub platform_updated_at: Option<DateTime<Utc>>,
}
```

Platform-specific response models must be converted into this representation before entering the rest of the application.

## JobSource

External platforms are represented through a source abstraction.

Conceptually:

```rust
#[async_trait]
pub trait JobSource {
    async fn search(
        &self,
        query: &JobQuery,
    ) -> Result<Vec<Job>, JobSourceError>;
}
```

Initial implementation:

```text
JobSource
    │
    └── Job104Source
```

Potential future implementations:

```text
JobSource
    ├── Job104Source
    ├── LinkedInSource
    ├── YouratorSource
    └── CakeSource
```

Adding another source should not require modifying change-detection or persistence logic.

## Change Detection

Jobs are identified by:

```text
(source, external_id)
```

Relevant normalized content is hashed.

Conceptually:

```text
normalize
    ↓
canonical representation
    ↓
SHA-256
    ↓
content_hash
```

Classification:

```text
Unknown ID
    → New

Known ID + different hash
    → Updated

Known ID + identical hash
    → Unchanged
```

Normalization should remove meaningless formatting differences without hiding meaningful content changes.

## Repository

SQLite is the initial persistence mechanism.

The repository owns persistence concerns.

Other modules should not depend directly on SQL queries or SQLite-specific behavior.

Conceptually:

```rust
trait JobRepository {
    async fn find(...);
    async fn insert(...);
    async fn update(...);
}
```

Do not create abstractions merely to support hypothetical databases.

The repository boundary exists primarily to isolate persistence behavior and enable testing.

## Notification

Notification delivery is independent from job sources.

Conceptually:

```rust
#[async_trait]
pub trait Notifier {
    async fn notify(
        &self,
        changes: &[JobChange],
    ) -> Result<(), NotificationError>;
}
```

The MVP should implement only one notification channel.

## Scheduling

Scheduling is maintained by the running application.

```text
In-process scheduler
      │
      ├── 07:00
      └── 17:00
             │
             ▼
       Synchronization cycle
             │
             ▼
       Sleep until next run
```

The service performs an immediate check after startup, which also makes manual
and deployment verification straightforward.

## Failure Strategy

Failures must be visible.

A synchronization cycle should return a non-zero exit status when the overall operation fails.

Failures should include contextual logs.

External-source failures must not corrupt previously stored data.

Notifications should only describe successfully persisted state changes.

## Architectural Principles

Prefer:

* explicit boundaries
* small modules
* testable pure logic
* platform-independent domain models
* deterministic change detection
* minimal infrastructure

Avoid:

* speculative abstractions
* excessive traits
* unnecessary services
* premature optimization
* external-service assumptions leaking into domain code

## Architecture Decision Rule

Before introducing a new architectural abstraction, answer:

> What concrete current problem does this abstraction solve?

If there is no concrete answer, do not introduce it.
