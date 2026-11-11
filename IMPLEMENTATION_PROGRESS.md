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

Live verification subsequently confirmed matching body/criteria text across
consecutive scans, durable pickup of the Ready stories, and successful guarded
AppFlowy status/comment writes. Projection instructions now specify exact metadata
values (including the full progress message and string retry count); guard errors
identify the expected committed value without relaxing validation. All 43 tests,
formatting, strict Clippy and debug/release builds passed again.

The Install script run reached Git preparation but SSH authentication failed.
Diagnosis found that the user service manager's SSH agent had no usable identities,
while the current session uses a different, reachable agent with identities.
Nicer CLI was blocked requesting an output example; Cleanup doc was blocked on
the unavailable writing guide and unclear completion criteria. These are live
results, not fixture assumptions. No PR or merge occurred during these checks.

The SSH read succeeded with the current session agent; refreshed systemd's stale
`SSH_AUTH_SOCK`. All AppFlowy status projections then drained successfully. A later
scan nevertheless reproduced a false revision change: Luna used the Markdown
renderer, whose paragraph whitespace differs from row/detail's plain text. The
prompt-only extraction fix was insufficient and is superseded by Rust source
canonicalization. Rust now requires authoritative row/body and complete comment
reads, reconstructs title/body/criteria/comments from their MCP results, and
rejects missing or inconsistent source reads. Model transcription no longer
determines revision-bearing text. Existing content hashing still detects actual
body, criteria and human-comment edits; progress metadata remains excluded.

All 45 tests passed, including new canonicalization regressions and updated
end-to-end MCP fixtures. Formatting, strict Clippy and debug compilation passed.
The installer run hit its retry limit during diagnosis and will be retried after
restoring its original Ready authorization. Its history is preserved. The SSH
agent refresh is session-dependent; unattended operation needs a durable agent
or another configured Git credential mechanism.

Final live verification: release compilation passed and the installed binary was
checksum-verified. The fresh canonical scan matched the installer's original
requirements exactly, so the existing blocked run was retried through the CLI.
Git fetched successfully, the isolated worktree was prepared at base
`00e74f0540a6b533ac151091c00456dbb33ef2cc`, and the worker emitted completed command
events. The service is active with zero restarts and the projection outbox is
empty. AppFlowy shows Install script In Progress, with Nicer CLI and Cleanup doc
Blocked for their recorded clarification requests. `auto_merge = false` was
rechecked. Story completion, real PR CI and merging are not claimed by this check.

## Delegated implementation choices — 2026-11-11

Changed Luna's readiness policy to respect the user's delegation of ordinary
engineering and design choices. Understandable outcomes default to implementation;
missing examples, exact CLI formatting or an explicit criteria checklist do not
require clarification. Genuine blockers must explain why repository inspection
or a reasonable default cannot resolve them. Source requirements remain verbatim,
and Rust's authorization, verification and merge gates are unchanged.

Updated the README to describe this policy. Formatting, strict Clippy, debug and
release builds and all 45 existing tests passed. Live reclassification of Nicer
CLI will be checked after installing the build; automatic merging remains off.

Installed the checksum-verified release and restarted the daemon between coding
worker invocations. A fresh live Luna scan returned `decision=implement` for the
unchanged Nicer CLI story: its intended behavior is understandable and ordinary
design choices are delegated. Restored Ready and used the CLI to retry the
existing blocked run after verifying identical requirements. SQLite confirms it
is queued. The service is active with zero restarts and `auto_merge = false`.
During shutdown, an aborted poll was logged as a task panic; the service restarted
successfully, but graceful-shutdown reporting needs a separate correction.

## Referenced context and actionable repair feedback — 2026-11-11

Diagnosed all live blocked stories. Cleanup doc was rejected by an unintended
board-only read instruction, despite its human comment linking an accessible
AppFlowy writing guide. Both implementation stories exhausted three repair
attempts on the same missing `cc` linker: `/usr/bin/cc` points through
`/etc/alternatives`, which the test sandbox did not expose. The repair prompt
contained only a host log path that the worker could not read.

Luna now reads referenced AppFlowy documents, including named guides resolved
through the folder. Rust attaches exact fetched contents to the story context
and hashes them with the requirements. Missing reads block the affected story;
reference documents cannot authorize extra tasks. The sandbox exposes the
read-only alternatives directory, and Rust-project preflight compiles a minimal
program inside the sandbox before dispatch. Failed tests now supply the command
and bounded stdout/stderr directly to the repair agent, including partial output
on execution failure. The worker is instructed to diagnose, repair and rerun
those checks. The repair budget and deterministic merge gates remain intact.
Expected aborted polling/projection tasks no longer produce shutdown panics.

Formatting, strict Clippy, debug/release compilation and all 47 tests passed.
Regressions cover referenced-page content and revision changes, C/Rust linking
inside the sandbox, and an end-to-end failed build repaired using actual stderr
before independent review and CI verification. The two existing implementation
worktrees also passed their configured `cargo test --locked --offline` and
strict Clippy commands in the repaired sandbox, without manual source changes.
Evidence is retained under `/tmp/maistro-verify-6_9e60jb/logs/`. All three stories
were restored to Ready for recovery. Automatic merging remains disabled.

Live recovery verification: Luna accepted all three Ready stories and read the
linked writing guide. Cleanup doc's persisted task context contains its full
contents and one grounded page reference. The installed release passed compiler
preflight and the daemon is active with zero restarts. Cleanup doc was dispatched;
the existing installer and CLI runs were explicitly retried after confirming
unchanged requirements and are queued behind it. Their worktrees and histories
were retained. Actual story review/CI completion has not been claimed.

## Merge conflict recovery in manual mode — 2026-11-11

PR maintenance previously ran only after the automatic-merge gate and green CI.
With automatic merging disabled, an outdated conflicting PR could wait forever;
an actual Git merge conflict also escaped the repair loop as an error.

Maistro now integrates an advanced base before waiting for CI, in both manual
and automatic merge modes. Rust preserves the pending merge and gives the coding
agent the conflicted paths and repair instructions. Rust completes the merge,
reruns deterministic checks and independent review, and pushes to the existing
PR without force. Pending merges recover after interruption. Conflict markers
prevent a commit and feed the bounded repair loop. Protected, binary and unsafe
conflicts fail closed. Upstream changes to protected files are distinguished from
worker edits. Uncertain remote merge attempts are not modified. Story comments
and moving a story back to Ready retain their existing behavior.

Formatting, strict Clippy, debug/release builds and all 49 automated tests passed,
including the host namespace tests. New regressions use disposable repositories
to exercise text and modify/delete conflicts, crash recovery, marker rejection,
protected upstream changes, clean merges and ordinary non-force pushes. A full
pipeline test proves repair with automatic merging disabled, fresh tests and
review, CI tied to the new head, reuse of the same PR and repeat reconciliation
without another attempt. Production automatic merging remains disabled.

Installed the checksum-verified release and restarted the service; it is active
with zero restarts and automatic merging disabled. The previous Cleanup doc run
had already been cancelled by a board scan at 12:56 UTC, before this restart,
so it was not revived. Live GitHub conflict repair is not claimed; its complete
flow was verified with real disposable Git repositories and fixture agents/API.

## Hosted CI compiler fixture — 2026-11-11

GitHub Actions run 43026692241 failed only
`sandbox_compiles_and_runs_c_and_rust`: `rustc: not found`. The fixture used no
toolchain paths, so it passed with cosmo's system Rust but could not see the
runner's rustup installation inside the filesystem boundary. The fixture now
discovers the active compiler's sysroot and mounts that specific toolchain
read-only through the existing project configuration. It also asserts that the
sandbox selects that compiler, preventing a system installation from masking
the configuration mistake. No isolation rules or CI checks were relaxed.

All 49 tests, including host isolation fixtures, passed locally, along with
formatting, strict all-target Clippy, debug build and release build. A new hosted
Actions run remains to be verified after pushing this fix. Production code and
the running daemon are unchanged.

## Clean base updates and repair accounting — 2026-11-11

Install script was falsely blocked after passing tests and review: the conflict
recovery change routed even a clean base merge through the coding repair budget.
Repeated base advances exhausted its three attempts despite no conflicted files.
Clean integrations now transition directly from WaitingCi to Reviewing without
another coding agent or attempt. The review obligation is persisted before Git
commit; recovery refreshes the committed head and reruns checks and independent
review. Real conflicts, failed checks and rejected reviews remain bounded.

All 50 tests passed, including the full host isolation suite. The new pipeline
regression covers clean integration at the attempt limit and interruptions both
before the merge commit and after commit but before SQLite records its head.
Each case requires fresh tests and independent review, updates the existing PR,
checks its new head in CI and remains idempotent. Formatting, strict all-target
Clippy and the debug build passed. Automatic merging remains disabled.

Release build passed and the checksum-verified binary is installed. With the
daemon stopped, backed up SQLite and restored only Install script's false block
to WaitingCi after checking unchanged requirements, matching HEAD/MERGE_HEAD and
an empty conflict list. The attempt count was retained at three and prior test
and review evidence cleared. AppFlowy was restored to Review. After restart,
Maistro completed the clean merge and fresh tests and independent review at head
`7584cc784469408f12d50e024cd0510169622cd1`, still on attempt three. The daemon is
active; automatic merging is disabled. Remote push/CI follow this verification.
