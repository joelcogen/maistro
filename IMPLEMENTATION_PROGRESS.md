# Implementation progress and limitations

Maistro implements the story-to-PR workflow: board polling, isolated coding,
local checks, independent review, PR maintenance and gated squash merging.
Automatic merging is off by default. Setup and CLI usage are in the
[README](README.md); this file describes the implementation and its limits.

## What works

- Multiple projects are polled independently. Scheduling rotates between projects,
  respects worker limits and prioritizes stories within each project. SQLite
  claims and repository locks prevent duplicate active work.
- Only Ready stories authorize new work. Dependencies are reconsidered when they
  finish. Ordinary design choices are delegated to the coding agent; a blocked
  story needs a concrete reason that repository inspection or a reasonable default
  can't resolve.
- Rust reconstructs requirements from row bodies and human comments returned by
  MCP. Referenced documents are included in the worker context and revision hash.
  Missing or inconsistent reads stop work. Progress fields don't change the
  requirements revision, and reference pages can't authorize additional tasks.
- Board cancellation belongs to the user. Internal cancellation stops a run
  without projecting Cancelled or leaving a stale status update queued. The MCP
  write guard rejects cancellation projections, and startup discards legacy
  cancellation writes. Rust reads status from the configured source Status cell;
  coordinator prose, comments and execution metadata cannot override it. Unknown
  or inconsistent source statuses fail closed.
- Workers receive one assigned worktree. Rust runs configured checks and gives
  failed commands and bounded stdout/stderr to the worker for repair. A fresh
  reviewer examines the result. Tests and approval must match the current commit;
  an agent's success report isn't verification.
- Rust creates and updates the PR. When the base advances, it integrates the
  changes, reruns checks and review, and pushes to the same PR without force.
  This also works with automatic merging disabled. Clean integrations don't use
  repair attempts; actual conflicts, failed checks and rejected reviews do.
  Protected, binary or unsafe conflicts need human handling.
- Automatic merge requires current authorization and requirements, matching PR
  identity, successful checks for the exact head, independent approval and GitHub
  mergeability. Protected-file changes block it. Completion requires confirming
  the squash commit on the configured base branch.
- The CLI supports human-readable and JSON output, configuration editing,
  project setup, pause/resume, cancellation, retry and reload. A user systemd
  service is included.

## Isolation and recovery

Rust owns Git writes, GitHub credentials, execution state and merge decisions.
The coordinator has scoped MCP access. Workers have hidden Git metadata, selected
read-only tools and file-based Codex authentication; reviewers have read-only
source. Bubblewrap isolates their filesystem and processes. Model traffic goes
through a proxy restricted to model endpoints. Tests have no network or model
credentials. There is no fallback that runs workers without the sandbox.

AppFlowy status and comment writes pass through a Rust MCP guard. It checks the
target row, permitted values and current committed state, and requires receipts
before acknowledging an update. It cannot change requirements. SQLite records
state changes and pending updates together, so a projection outage after a merge
retries the update without repeating implementation.

Recovery checks recorded branches, PRs and merge results before repeating remote
writes. Process records include boot and start identity; an unconfirmed exit holds
the run instead of starting duplicate work. Pause holds new work and merges while
local work and review can finish. Cancellation stops local work. Reload drains
active tasks and preserves project identity; invalid changes leave the previous
configuration in use.

Startup applies the current TOML project settings over saved configuration and
clears changed projects' cached scans. Identity changes require a project without
saved runs or pending updates; otherwise use a new project ID. Keep the same
database and worktrees when restarting. Don't delete the
database to retry a failed job. Keep it on a local filesystem with reliable SQLite
locking. Use SQLite's online backup API, or stop the service and copy the database,
WAL and SHM files consistently. Copying only the database during active WAL writes
can lose state. Logs, transcripts and database backups contain private project
information and belong outside the repository.

## Test coverage

The tests use disposable repositories and SQLite databases. Pipeline fixtures run
the production engine with controlled model and GitHub responses; they don't prove
that a live service or account is configured correctly.

The user-owned cancellation change passed all 55 tests, including host isolation
fixtures, plus formatting, strict all-target Clippy and debug/release builds.
Regressions cover authoritative status labels, cancellation without a board
write, rejection of cancellation writes and removal of legacy queued updates.

| Coverage | Tests |
| --- | --- |
| Configuration, source revisions, referenced pages and process supervision | [m1](tests/m1.rs), [source](src/source.rs) |
| Claims, restart recovery, dependencies and sandbox isolation | [m2](tests/m2.rs) |
| Protected paths, exact-commit checks, review evidence and merge intent | [m3](tests/m3.rs) |
| Scheduling, pause, backoff, concurrent projects and process termination | [m4](tests/m4.rs) |
| PR creation, bounded repair, cancellation, merge recovery and projection outages | [pipeline fixtures](src/acceptance.rs) |
| Conflicts, interrupted merges and ordinary pushes | [Git conflict fixtures](tests/git_conflicts.rs) |
| Human/JSON output and editor invocation | [CLI fixtures](tests/cli.rs) |

The deterministic checks configured in the example are:

```sh
cargo test --locked --offline
cargo clippy --locked --offline -- -D warnings
```

The default test command skips tests marked ignored, including namespace-dependent
pipeline and isolation fixtures. The [CI workflow](.github/workflows/ci.yml) also
checks formatting, lints all targets, builds the binary, runs tests with
`--include-ignored`, and builds a release. Those fixtures need a Linux host that
allows Bubblewrap namespaces and local listeners. See the README for the full
commands. The worker probe in `examples/worker_probe.rs` makes a live model call
and is separate from the fixture suite.

## Limits and remaining validation

- Supported integrations are Linux, GitHub.com and an existing stdio AppFlowy MCP
  server. Workers require Codex file authentication; keyring-only setups aren't
  supported. Offline tests need their dependencies and toolchains available inside
  the sandbox. Rust preflight checks compiler and linker availability.
- Automatic merging requires strict classic branch protection and inspection of
  active rulesets. Ruleset-only protection isn't sufficient. Maistro doesn't
  change protection settings or use an administrator account for automatic merges.
- Board reads and GitHub writes don't share a transaction. Revision and
  authorization checks reduce stale-work risk, but cancellation can't undo a merge
  GitHub has already accepted.
- Live checks confirmed board reads, guarded status/comment writes and an
  isolated coding probe. A clean base update also passed fresh tests and review
  and reached the existing PR without another coding attempt. A manually merged
  PR was verified on the base branch and its story moved to Done automatically.
  These observations don't verify unattended automatic merging. Live conflict
  repair and the hosted CI result after the compiler-fixture fix still need
  confirmation. Fixture coverage alone doesn't close these gaps.
- Existing runs can retain attempts consumed by the earlier clean-update bug.
  One affected run was restored with its worktree and PR intact, then explicitly
  retried with a fresh bounded budget after a later base update caused a real
  conflict. That retry wasn't evidence of successful conflict repair.
- Deployment, a web dashboard, notifications, monetary accounting and atomic
  changes across repositories aren't implemented. Work is serialized within a
  repository; concurrency is across independent projects.

Before unattended automatic merging, validate the intended configuration and run
an end-to-end trial on a disposable project with real CI and branch protection,
including review rejection and cancellation. Installation-specific board IDs,
credentials, paths and service state aren't part of this document.
