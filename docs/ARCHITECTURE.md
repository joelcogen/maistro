# Architecture and trust boundaries

`maistro` is a Linux process. Each invocation reads a strict TOML configuration.
SQLite owns execution state; AppFlowy owns requirements and permission to start.
A status projection is never a lock. Model output never grants merge permission.

The coordinator receives only the configured AppFlowy MCP with a read allowlist.
It has no shell, plugins, other MCP servers, or delegation. A projection request
adds only row-update/comment tools. Reads must enumerate all rows and include
bodies and requirements comments. Rust rejects ambiguous board resolution,
partial scans, unknown fields/statuses and IDs absent from successful MCP output.
Rust computes the requirements fingerprint, excluding status/progress fields.

Codex runs noninteractively with `--json`, an output schema, an explicit model,
`--ephemeral`, and approval policy `never`. Schema checking is repeated in Rust.
The user's AppFlowy MCP server definition is loaded from the existing Codex
configuration; no credentials are copied to this repository. Agent transcripts
are private runtime artifacts and must not be committed or published.

The original AppFlowy prompt linked in the specification was inspected. Its
instructions to work on master are replaced by the specification's worktree and
PR requirements. Its server-upgrade instructions are unrelated to this project.

Rust is responsible for worktrees, tests, commits, pushes, PR identity, independent
review evidence, CI observations and merging. A reviewer gets a fresh context and
read-only source. Implementation and tests receive a minimal Bubblewrap filesystem;
no other project, daemon state, GitHub credential, or SSH agent is exposed.
An unavailable sandbox is a deployment failure; there is no unsandboxed fallback.

GitHub credentials belong to Rust. Branch protections remain unchanged. The merge
request includes the verified head SHA and uses squash only. A successful request
is not completion until the returned commit is verified on the configured base.
Remote uncertainty is resolved by reads before another mutation.

The acceptance matrix in `docs/ACCEPTANCE.md` records evidence and external gaps.

## Recovery contract

The scheduler holds one advisory singleton lock for its state directory. A
repository lease and a partial SQLite uniqueness index prevent duplicate active
stories. State changes and outgoing AppFlowy projections commit together. Remote
writes use stable run/branch identities; recovery queries existing PRs and merged
commits before creating anything. Uncertain remote outcomes retain their run.

The worker sandbox has its own PID namespace and dies with its supervisor.
Ordinary daemon shutdown cancels process groups and leaves durable runs available
for reconciliation. Process-group termination is bounded; a PID alone is never
used as proof that an interrupted run is safe to duplicate.

Pause stops new dispatch; running tasks may finish local work and review, but
merges are held until resume. Cancellation revokes authorization and terminates
active local work; it never force-deletes branches or closes user-owned PRs.

## Host compatibility and deterministic boundaries

The installed host AppArmor profile allows an initial Bubblewrap namespace but
rejects nested Bubblewrap. Worker Codex therefore uses its external-sandbox mode
inside mandatory Rust-owned Bubblewrap confinement. A new network namespace has
no host egress. A local bridge reaches a private UNIX socket; the Rust CONNECT
proxy permits only `chatgpt.com`, `api.openai.com`, `auth.openai.com` and
`ab.chatgpt.com` on port 443. GitHub and arbitrary hosts are rejected. This route
was exercised by a real Astra implementation probe. Tests have no proxy at all.

The projection MCP guard launches the original configured stdio server and checks
writes against a private committed scope and current SQLite state. It learns field
and option IDs from the target board's successful field response, allows exact
status/metadata values only, checks row/workspace identity, and refuses other write
tools. Exact previously observed progress comments are not appended again. Rust
requires a confirmed status update and a comment receipt before acknowledging the
outbox item. Projection records cannot override source requirements.

Process termination waits for group disappearance. Unconfirmed termination retains
an active, held run. Restart checks only unfinished process records, uses boot and
process-start identity, and never kills an unrelated recorded PID. Completed
processes have a termination receipt, avoiding PID-reuse false positives.
