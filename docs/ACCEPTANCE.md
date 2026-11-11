# Acceptance evidence

All 12 specification scenarios are covered by passing automated tests. External
responses are fixtures except where explicitly identified as live. End-to-end
fixtures execute the production engine with real SQLite, Git repositories,
worktrees, process supervision and Bubblewrap; Codex responses and GitHub HTTP
responses are controlled test doubles. They do not claim real GitHub verification.

| # | Scenario | Passing evidence |
|---|---|---|
| 1 | Independent project/board validation and polling | `two_projects_validate_and_poll_independent_boards`: two disposable Git roots, distinct board IDs, per-project GitHub validation calls, repeated coordinator scans |
| 2 | Backlog ignored; Ready starts exactly once | `backlog_ready_claim_restart_deduplicate`: no Backlog run, one Ready claim |
| 3 | Duplicate scans/restarts do not duplicate jobs/PRs | SQLite reopen test; `competing_claims_are_transactionally_exclusive`; HTTP `uncertain_create_is_reconciled_without_duplicate_pr`; lost-merge-response end-to-end recovery |
| 4 | Missing dependencies/requirements block and explain | `missing_dependencies_and_requirements_project_blocked`: durable reason projections; guarded exact-row updates/comments; dependency completion reconsideration test |
| 5 | Assigned worktree only; agents cannot merge | Real host namespace test hides Git metadata, home and other repos; `proxy_rejects_github`; live isolated Astra fixed-file probe |
| 6 | Review/CI repair is bounded | `reviewer_rejection_exhausts_pipeline_budget`: three actual fixture implementation/review iterations then Blocked; end-to-end CI failure repairs once; repair-budget exhaustion unit test |
| 7 | Changed/cancelled requirements cannot merge stale work | End-to-end modifies coordinator requirements, then cancels story: both reject with zero merge mutations; direct revision/status gate tests |
| 8 | Protected files/failed checks block merge | Default protected-file tests; exact-head/latest-run/missing/skipped/failing CI cases; self-report cannot replace test/review evidence |
| 9 | Green eligible PR merges once, then Done | End-to-end verifies one PR, one SHA-pinned squash mutation, actual disposable Git merge commit ancestry, persisted Completed |
| 10 | AppFlowy outage after merge does not rerun code | End-to-end fails projection, retries after recovery, verifies unchanged implementation/review session count and one merge |
| 11 | Independent projects execute concurrently | Two simultaneous real network-isolated sandboxes write only their respective roots; global/per-repo limits and round-robin priority tests |
| 12 | Pause stops new dispatch with defined active-job policy | Durable global/project pause claim tests; end-to-end paused active work finishes verification but makes zero merge requests until resumed |

Additional checks cover cancellation, timeout/partial logs, verified process-group
termination, unfinished-process recovery records, explicit state transitions,
singleton locking, Git worktree recovery, immutable project identity, bounded
backoff, strict JSON contracts, ambiguous/ungrounded board identifiers and
argument-injection handling.

## Live and host checks

- Local Codex 0.167.0 recognized existing ChatGPT authentication.
- A real noninteractive `gpt-6-luna` session called the existing configured
  AppFlowy MCP `get_workspaces` successfully and returned structured JSON.
- A real `gpt-6-astra` session created `probe.txt` inside the externally isolated
  worker; Rust verified exact bytes. It used the restricted model-only proxy.
- Real host Bubblewrap confinement and concurrent-project tests passed.
- `NoNewPrivileges` plus the required namespaces passed a host probe.
- Release CLI help/projects/status/pause/resume/project controls/reload passed
  using a disposable configuration and database.
- `systemd-analyze --user verify` passed with ExecStart pointed at the built
  binary. The service was not installed or started.

## Not exercised against real services

No daemon GitHub token or production project mapping was supplied. Real GitHub
permission/protection/ruleset reads, PR creation, CI execution, ready-for-review,
squash merge and remote commit confirmation remain deployment checks. Real
AppFlowy status/comment writes through the guard were not performed. No real
repository was pushed, merged or deployed. Automatic merge is disabled in the
example configuration.

AppFlowy row-detail reads are eventually consistent according to the installed
MCP documentation. Full reads, requirement fingerprints, current authorization
checks and fail-closed errors reduce stale execution risk; they do not create an
atomic transaction spanning AppFlowy and GitHub. A cancellation after GitHub has
accepted a merge cannot undo it. Validate this timing behavior on a disposable
real project before unattended auto-merge.
