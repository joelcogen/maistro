# Implementation status and limitations

Maistro has the local orchestration pipeline in place: board polling, isolated
implementation, independent review, test and CI gates, squash merging, and restart
recovery. That doesn't establish that unattended operation works with a particular
AppFlowy board or GitHub repository. Live integration still needs validation.

This is the implementation reference. [README.md](README.md) covers installation,
configuration and commands. The old specification, architecture, acceptance,
operations and pilot documents have been consolidated here; their proposed work
and one-time setup notes aren't current deployment status.

## What's implemented

- Multi-project TOML configuration, strict agent JSON contracts and board scans.
  Rust rejects incomplete scans, ambiguous boards and identifiers without MCP
  evidence. Requirements fingerprints exclude status and progress metadata.
- SQLite claims, one active run per story, repository locks and a daemon singleton
  lock. Scheduling rotates between projects and respects priority and worker
  limits. Each repository is limited to one worker; independent repositories can
  run concurrently.
- One worktree per execution, bounded implementation and repair attempts, a fresh
  reviewer session, deterministic tests, branch pushes and linked pull requests.
  Rust owns Git operations and GitHub credentials.
- Merge gates check current authorization, unchanged requirements, PR identity,
  independent review, local tests and required CI checks for the exact head SHA.
  Protected paths require human handling. A merge uses squash and is only complete
  after Rust confirms the returned commit on the configured base branch.
- Pause/resume, cancellation, retry, configuration reload and restart recovery.
  The CLI and user systemd unit are included. Runtime paths default to the service
  user's `~/.local/state/maistro/`; explicit overrides remain supported.

The original milestones were read-only integration (M1), single-worker execution
(M2), verification and merging (M3), and multi-project operations (M4). Their code
and tests are present. The later naming and per-user storage changes are also in
place. There is no deployment step, web UI, notification system, monetary usage
accounting or atomic change across repositories.

## Authorization and isolation

Only Ready authorizes new work; the label is configurable. Missing dependencies
hold the run until they become Done. Unclear requirements become Blocked. Changed
requirements or revoked authorization prevent stale work from merging.

The coordinator uses the existing AppFlowy MCP configuration with a read
allowlist. It has no shell or delegation. Status and comment writes go through a
Rust stdio MCP guard tied to the committed run and row. The guard permits exact
status, metadata and comment values, rejects requirement edits and other writes,
and requires receipts before acknowledging an outgoing update.

Workers get their assigned source, selected read-only tools and caches, private
output, and read-only Codex file authentication. Git metadata, other repositories,
daemon state and GitHub credentials aren't exposed. Reviewers get read-only
source. Tests have no network access or model credentials.

Codex runs inside mandatory Bubblewrap confinement. A network namespace and Rust
CONNECT proxy restrict worker egress to `chatgpt.com`, `api.openai.com`,
`auth.openai.com` and `ab.chatgpt.com` on port 443. Tests have no proxy. If the host
can't provide the sandbox, execution fails; there's no unsandboxed fallback.
See [sandbox.rs](src/sandbox.rs), [network.rs](src/network.rs) and
[mcp_guard.rs](src/mcp_guard.rs) for these boundaries.

## Recovery and operational limits

SQLite uses WAL with FULL synchronization. State transitions and pending AppFlowy
updates commit together. Recovery looks for existing PRs before creating one and
records merge intent before sending the request. An uncertain merge stays in
reconciliation until remote identity and Git ancestry resolve it. AppFlowy updates
retry separately, so an outage after a merge doesn't rerun implementation.

Pause holds new dispatch and merges while active local work and review can finish.
Cancellation stops local work and retains the branch and PR. Reload drains tasks
and validates the new configuration; invalid changes keep the old configuration.
Project identity and runtime paths can't change through reload.

Process records include boot and start identity. A timeout or expired lease isn't
proof that a process has stopped. Unconfirmed termination holds the run, and
recovery doesn't kill an unrelated process just because its PID matches a record.
Operational retries use backoff capped at one hour and a configured attempt
budget. Polls and pending board updates keep retrying with bounded backoff.

Restart with the same configuration, database and worktree root. Don't delete the
database to retry a failed update. For failed tests, review or CI, inspect the
retained worktree and logs, fix the cause, return an unchanged story to Ready and
use `maistro retry RUN_ID`. Changed requirements create a separate revision.

Runtime files are private (umask 0077). Logs include session JSONL, stderr,
structured output and process evidence; output is capped at 16 MiB per stream.
Treat logs and SQLite snapshots as sensitive project data. Use a local filesystem
with reliable SQLite locking. Back up through SQLite's online backup API, or stop
the service and copy DB/WAL/SHM consistently. Copying only the database during WAL
writes can lose state. Worktrees are retained by default; optional cleanup removes
only clean, completed worktrees, leaving branches, PRs and logs.

Nothing runs while the host sleeps. On resume, wall-clock deadlines still apply
and board scans reconcile missed changes. Authentication and quota failures become
recorded errors and retries. Refresh Codex authentication as the service user.

## Test coverage and evidence

The automated suites use disposable repositories and controlled external responses.
The engine fixtures exercise real SQLite, Git, process supervision and, when run
explicitly on a suitable host, Bubblewrap. They don't verify live GitHub or
AppFlowy behavior.

| Coverage | Tests |
| --- | --- |
| Configuration, runtime paths, strict contracts, requirement revisions, cancellation and argument handling | [tests/m1.rs](tests/m1.rs), [src/config.rs](src/config.rs) |
| Ready-only claims, restart deduplication, dependency blocking, projection retries, worktree recovery and isolation | [tests/m2.rs](tests/m2.rs) |
| Protected files, exact-head CI, changed/cancelled stories, independent review evidence and durable merge intent | [tests/m3.rs](tests/m3.rs) |
| Fair scheduling, pause/resume, repository slots, backoff, configuration identity, concurrent isolation and process termination | [tests/m4.rs](tests/m4.rs) |
| Independent board polling, bounded reviewer rejection, CI repair, stale authorization, paused merging, lost merge response and post-merge AppFlowy outage | [src/acceptance.rs](src/acceptance.rs) |
| Model-only egress, MCP write guard and GitHub PR reconciliation | [src/network.rs](src/network.rs), [src/mcp_guard.rs](src/mcp_guard.rs), [src/github.rs](src/github.rs) |

The supplied deterministic checks are:

```sh
cargo test --locked --offline
cargo clippy --locked --offline -- -D warnings
```

HTTP fixtures listen on `127.0.0.1`. If the host uses an HTTP proxy, include
`127.0.0.1,localhost` in `NO_PROXY` so fixture requests stay local. An empty
`NO_PROXY` caused HTTP 403 responses in this worktree; the full default suite
passed with that local routing correction (34 passed, four ignored).

The default test command skips four namespace-dependent tests. The README has the
full host-check commands, including `--include-ignored` and a helper binary build.
[CI](.github/workflows/ci.yml) runs those fixtures, formatting, all-target Clippy
and a release build on Ubuntu 22.04 with Rust 1.93.1. Its check name is `test`.
[worker_probe.rs](examples/worker_probe.rs) is a separate live model probe.

Earlier implementation notes recorded 38 passing host tests on 2026-11-11,
including the namespace fixtures, plus formatting, Clippy and release compilation.
They also recorded a live Luna AppFlowy read, an isolated Astra file-writing probe,
CLI smoke checks and systemd unit validation. These are historical results, not a
fresh check of the current host or deployment.

## Remaining integration limits

- Supported deployment is Linux, GitHub.com and existing stdio AppFlowy MCP.
  Workers require Codex `auth.json`; keyring-only authentication isn't supported.
- Automatic merge defaults to off. It requires strict classic branch protection
  and inspection of active rulesets; ruleset-only protection isn't enough.
  Repository administrators are rejected as daemon identities. The README covers
  the dedicated account and token requirements.
- The retained implementation notes don't establish live GitHub permission and
  protection reads, PR creation, CI, squash merging or remote commit confirmation.
  They also don't establish real guarded AppFlowy status/comment writes. Validate
  these on a disposable project before relying on unattended automatic merging.
- AppFlowy reads can be eventually consistent. Fingerprints and authorization
  checks reduce stale execution risk but can't make AppFlowy and GitHub one
  transaction. Cancellation can't undo a merge GitHub has already accepted.

## Existing pilot reference

The earlier pilot setup recorded these identifiers and paths. They are retained
so the board can be found, not as evidence that its service is running or ready:

- Parent page: `f7b2406b-c5de-4f36-b5d1-c4935627b150`
- [Board](https://appflowy.com/app/f757bab8-f38b-49a2-9338-6a524d0646e2/8983b67b-9d14-42c0-a0fa-b9cdad15c501)
- Database: `7606135a-e2a1-4426-8984-3a0d9c6e177c`
- Repository and ignored local configuration: `/home/jole/dev/maistro` and its
  `config.toml`
- Runtime root: `/home/jole/.local/state/maistro`
- Preserved provisional database: `preflight-unconfigured.db` under that root

The old notes used `unconfigured/maistro` as a placeholder GitHub repository and
reported a missing token and a stopped service. Those observations aren't current
status. Check the installed configuration and remote integration using the README
before operating the pilot. The systemd service reads the installed configuration,
not the repository copy.
