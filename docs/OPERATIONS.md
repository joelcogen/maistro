# Operations

By default, SQLite state is `~/.local/state/maistro/state.db`, with `logs/` and
`worktrees/` alongside it. These paths use the home directory of the service user.
Explicit runtime-path overrides in the configuration take precedence.

Runtime files are private (CLI umask 0077). Treat transcripts as sensitive project
data; never commit runtime data, auth files, environment files or SQLite snapshots.
Agent outputs are bounded to 16 MiB per stream; partial output survives timeout.
Logs include per-run/per-session JSONL, stderr, structured output and PID evidence.
PIDs are diagnostic only: the daemon never blindly kills a recorded PID after a
restart. Worker PID namespaces die with their parent; systemd kills the complete
service cgroup. Failed/interrupted attempts consume a bounded repair budget.

`status` shows execution states and last errors. SQLite `events` is append-only
with transition/tool-stage explanations. `leases` carries owner/expiry heartbeats;
expiration alone never permits stealing a live repository lock. Operational
failures use exponential backoff, capped at one hour, and become Blocked after the
configured attempt budget. Polls and durable status updates continue retrying with
bounded backoff because external service recovery should not require reimplementing
completed code. A potentially successful merge retains its reconciliation state.

For an unavailable AppFlowy service, wait for recovery and inspect the scan/error
and pending projection counts. Do not delete the database to retry. After a merge,
Done projections retry independently of implementation. For failed tests/review/CI,
inspect the retained worktree, logs and PR; fix missing environment dependencies or
clarify the story, put an unchanged story in Ready, then `maistro retry RUN_ID`.
Changed requirements form a distinct revision after stale work is cancelled.

For interrupted service operation, restart using the same config, SQLite file and
worktree root. Existing branch/PR identities are reconciled. Never reuse a project
ID for another repository or board. Database paths on network filesystems are not
supported; use a local filesystem with reliable SQLite locking. Back up with
SQLite's online backup API or stop the service and copy DB/WAL/SHM consistently.
Do not copy only the DB file while WAL writes are active.

The host may sleep: elapsed wall-clock deadlines remain meaningful on resume;
startup and periodic full board scans reconcile missed events. Nothing executes
while the server is asleep. Model quota/authentication failures become recorded
errors and bounded retries. Refresh Codex authentication as the service user when
needed. Isolated workers mount authentication read-only; the regular coordinator
uses the original Codex home and maintains normal CLI authentication.

Before enabling auto-merge, test a low-risk disposable project with real AppFlowy,
GitHub CI and branch protection. Confirm exact required-check names, read access
to protection/rulesets, identity of the dedicated non-admin account, and review
rejection/cancellation behavior. Then edit `auto_merge=true`, run `validate`, and
request `reload`. This application never weakens protection settings or enables
GitHub deployment. No real merge was performed during implementation.
