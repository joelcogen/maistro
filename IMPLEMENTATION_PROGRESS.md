# Implementation progress — 2026-11-10

Specification v1.0 was read in full and retained as `maistro-spec.md`.
The initial attachment-access blocker was resolved. M1 → M2 → M3 → M4 were
implemented in order, with build/test/static-check gates between milestones.

## Milestone results

| Milestone | Result | Verification at milestone gate |
|---|---|---|
| M1 — read-only integration | Complete | Build, six tests, formatting and strict Clippy passed; real Luna/existing AppFlowy MCP read passed |
| M2 — single worker | Complete | Build, twelve tests plus explicit host isolation test passed; SQLite claims/outbox/reopen and real disposable worktrees verified; real GitHub writes unavailable |
| M3 — verification/merge | Complete | Build, formatting, strict Clippy and 22 tests passed, including disposable end-to-end lost-response merge recovery and post-merge projection outage |
| M4 — operations | Complete | Concurrent polling/fair scheduling, repository locks, cancellation, pause/resume/reload/retry, dependency reconsideration, process recovery, CLI/docs/systemd implemented; expanded acceptance suite passed |

## Implementation

Rust owns scheduling, SQLite state, transactional claims, repository and singleton
locks, subprocess groups/PID namespaces, test execution, Git writes, PR identity,
review evidence, CI verification and squash merge policy. Multi-project TOML and
agent JSON are strict and typed. Existing Codex AppFlowy MCP configuration is
reused without changing user configuration.

Workers receive one isolated source tree, hidden Git metadata, selected read-only
toolchains, private output and read-only Codex authentication. Reviewers receive
read-only source. Tests receive no network or model credential. The host rejects
nested Bubblewrap namespaces, so Codex runs in mandatory external Bubblewrap
confinement with a network namespace and Rust model-host-only CONNECT proxy;
there is no host-unsandboxed fallback. Companion Codex executables are mounted.
A live Astra fixed-file smoke test passed through this boundary.

AppFlowy writes use a Rust stdio MCP guard, scoped to the committed run/row,
exact permitted status/metadata values and exact progress comment. The guard
rejects superseded state, requirement-field writes and other mutation tools.
Durable status/comment receipts are required; model-reported success alone is
insufficient. The production write transport reuses the existing stdio server.

SQLite uses WAL/FULL durability, immediate transactions, a partial active-story
uniqueness index, audited state changes, leases and a durable projection outbox.
Remote PR lookup precedes creation. Merge intent commits before the HTTP mutation;
uncertain responses reconcile against remote identity and actual Git ancestry.
Interrupted process records contain boot/start identity; verified termination is
recorded separately. A process whose exit cannot be confirmed holds its run.

Ready is the default authorization label, configurable to To Do. Dependency waits
preserve Ready authorization and automatically reconsider the same run when
prerequisites become Done. Ambiguous requirements become Blocked. Revision changes
and cancellation stop stale work. Pause holds new dispatch and merges while local
work can finish. Reload drains tasks, validates and preserves project identity;
invalid reloads retain the prior configuration.

## Verification

- Optimized release build produced `target/release/maistro`.
- Debug build, `cargo fmt --all --check`, and strict Clippy passed.
- All 12 acceptance scenarios are mapped to tests in `docs/ACCEPTANCE.md`.
- Full tests are run on the host with `--include-ignored`: the coding-session
  sandbox disallows localhost listeners/nested namespaces. Host fixture tests use
  disposable Git repositories, fake model responses and fake GitHub HTTP services.
- Live Luna + AppFlowy read and isolated Astra implementation passed.
- Release CLI smoke checks passed for help, projects, status, global/project pause
  and resume, and reload using disposable configuration/state.
- User systemd unit validation passed with the built executable path substituted.
  A NoNewPrivileges namespace probe also passed. Service not installed/enabled.
- Final regression: **36 passed, 0 failed, 0 ignored** with
  `cargo test --offline --locked -- --include-ignored` on the Linux host
  (9 library, 6 M1, 8 M2, 5 M3 and 8 M4 tests).
- Final `cargo fmt --all --check`,
  `cargo clippy --offline --locked --all-targets -- -D warnings` and
  `cargo build --offline --locked --release` all exited successfully.
- Implementation work is complete. Remaining deployment work is the external
  configuration and live integration validation described below; no unattended
  service or real automatic merging was enabled during implementation.

## External prerequisites and limitations

No daemon GitHub token or real project mappings were supplied. Real GitHub
protection/PR/CI/merge operations and real AppFlowy guarded writes remain
unverified. Examples keep auto_merge=false. Deployment requires configuring board
mappings, offline test environments and required checks, supplying a protected
least-privilege GitHub credential, and running `maistro validate` plus a disposable
real-project trial before enabling unattended auto-merge.

Linux/GitHub.com and existing stdio AppFlowy MCP are supported. Isolated Codex
requires file authentication. Auto-merge conservatively requires strict classic
branch protection as well as inspection of active rulesets; ruleset-only setups
fail closed. No production deployment, web UI, notifications, monetary accounting
or cross-repository atomic changes were added (optional/non-goals in the spec).
AppFlowy/GitHub do not offer a shared transaction; eventual-read and in-flight
merge timing limits are documented in the acceptance report.

Documentation: README.md, config.example.toml, docs/ARCHITECTURE.md,
docs/OPERATIONS.md, docs/ACCEPTANCE.md, systemd/maistro.service.

No unrelated repository was changed. No credentials were printed. This project's
Git repository was initialized locally with no remote, commits, merges or deploys.

## Pilot CI — 2026-11-11

Added `.github/workflows/ci.yml`: read-only GitHub Actions on branch pushes,
pull requests and manual dispatch, with a stable `test` check matching the pilot's
`required_checks`. It checks formatting, strict Clippy, all tests including host
isolation fixtures, and release compilation. Checkout is pinned and its release
SHA was verified. Rust is pinned to 1.93.1. No deployment or merge step is present.

Actionlint 1.7.12, YAML/shell validation, check-name alignment, formatting, Clippy,
debug/release builds and all 36 host tests passed. No remote workflow run was
possible yet. Repository destination and daemon credential remain pending; the
pilot is disabled and automatic merge remains off. No daemon was started.

## Maistro naming — 2026-11-11

Renamed the Rust package and CLI to `maistro`, the unit to `maistro.service`,
installed configuration paths to `~/.config/maistro/`, and the credential variable
to `MAISTRO_GITHUB_TOKEN`. Updated source, tests, schemas/references, CI comments,
Git metadata labels, documentation and `maistro-spec.md`. The pilot uses
`/home/jole/dev/maistro` and `/home/jole/.local/state/maistro`; the empty provisional
database identity was migrated and integrity checked. The AppFlowy parent page,
Kanban container and setup text now use Maistro while retaining their stable IDs.

Formatting, strict Clippy, debug/release builds and all 36 host tests passed.
The renamed systemd unit validated against the built binary. The source/config
audit found no stale prior-name references. No installed daemon/configuration
existed to migrate; installation paths and instructions were updated. The service
remains uninstalled and stopped, and automatic merge remains off.

## Per-user runtime storage — 2026-11-11

Runtime defaults now resolve to `~/.local/state/maistro/` using the current user's
absolute `HOME`: `state.db`, `logs/` and `worktrees/`. Both configuration loading
and interactive project creation resolve these paths. Explicit absolute and
configuration-relative overrides remain supported. Missing or invalid HOME is
an error when a home-relative runtime path needs resolution.

Moved this pilot's runtime directory to `/home/jole/.local/state/maistro`, with all
five files checksum-verified. The directory is mode 0700 and the database 0600.
There were no running daemon processes or execution runs to migrate. Updated the
pilot/example configuration, README, operations/pilot documentation, specification
example and AppFlowy setup text. The example omits path keys to use the defaults.

Formatting, strict Clippy, debug/release compilation and all 38 host tests passed,
including default-path loading, interactive setup path resolution, override
preservation and invalid HOME handling. The service remains stopped.

## Installed pilot startup fixes — 2026-11-11

Repository access validation now permits an owner account when automatic merging
is disabled. Automatic merge protection checks still reject administrator
accounts; push access and squash support remain required. The `admin` repository
role reported by GitHub is independent of the classic token's `admin:*` scopes.

Sandbox preflight now hides a main checkout's `.git` directory using a read-only
empty mount, while retaining the file mount for linked worktrees. A host test
verifies both read-only and writable sandbox modes without changing host metadata.

The first live Luna scan read all three Ready stories but returned the story count
as the board match count and confused the board view with its database. Clarified
both fields in the prompt and JSON schema; validation also rejects identical
database and view IDs. Ambiguous scans continue to fail closed.

Formatting, strict Clippy, debug/release builds and all 42 tests passed, including
host isolation, owner-account validation, identifier validation and merge gates.
The installed pilot retains `auto_merge = false`. Live scan and status projection
verification follows installation of this build; the earlier rejected scan did
not create execution runs.

The next live scan successfully persisted all three stories and dispatched one
run, but two further integration issues appeared. Codex requested interactive
approval for MCP writes despite the daemon's noninteractive policy. Only the two
tools behind Rust's committed-projection guard now receive explicit per-tool
approval; a process fixture verifies that ordinary scans remain read-only and
that writes use the guard and scope. Luna also partitioned the same source text
differently between scans, causing safe cancellation before any PR. The scan
contract now preserves the complete body and uses it verbatim as the criteria
when no separate criteria section exists. This needs repeat live-scan verification;
it is a model extraction contract, not a claim of deterministic source parsing.

All 43 tests passed after these changes, with formatting, strict Clippy and debug
build checks. Backed up the stopped pilot database and removed exactly two pending
erroneous cancellation projections, preserving the cancelled run history and
recording recovery events. Automatic merge remains disabled.
