# Maistro — Implementation Specification

**Status:** Ready for MVP implementation
**Date:** 2026\-11\-10
**Version:** 1\.0

## 1\. Objective

Build an always\-on, local, multi\-project coding orchestrator\. A user manages stories in AppFlowy; the orchestrator detects eligible work, uses a lightweight Luna agent to interpret the board and select tasks, delegates implementation to Astra through local Codex CLI processes, verifies the result, and automatically merges eligible pull requests\. No ChatGPT conversation must be open or manually prompted\.

On this page is the prompt I currently use, that this project is meant to replace:

https://appflowy\.com/app/f757bab8\-f38b\-49a2\-9338\-6a524d0646e2/221a4642\-3341\-4ad8\-910c\-cda319bbfce4

### Success criterion

For a correctly configured project, moving a story to To Do in AppFlowy is sufficient to trigger unattended implementation, testing, review, PR creation, gated merge, and status reconciliation to **Done**\.

## 2\. Decisions and constraints

|Area                |Decision                                                      |
|--------------------|--------------------------------------------------------------|
|Implementation      |Rust daemon and CLI (`maistro`)                                |
|Host                |Always-on local development server                            |
|Board               |AppFlowy, accessed through an **already configured Codex MCP**|
|AppFlowy interaction|Option A: Luna calls the MCP directly                         |
|Coordinator         |GPT-6 Luna (configured model ID, verified at deployment)      |
|Implementer         |GPT-6 Astra through noninteractive Codex CLI                  |
|Reviewer            |Fresh, separate Astra session (replaceable model)             |
|Source repositories |Multiple existing local Git repositories                      |
|Work isolation      |One Git worktree per execution                                |
|State               |SQLite with transactional claims                              |
|Runtime             |Tokio; user-level systemd service                             |
|Merge               |Automatic squash merge only after enforced gates              |
|Deployment          |Out of scope; merging is not production deployment            |

The daemon must not depend on an interactive Codex/ChatGPT session\. Agent access, authentication, model identifiers and supported CLI flags must be validated against the local installed Codex version before enabling unattended runs\.

## 3\. Architecture

1. **Rust scheduler:** Loads project definitions, launches polls, manages durable queues and locks, supervises processes, and reconciles remote state\.
2. **Luna coordinator:** Uses existing AppFlowy MCP tools to inspect a configured board, interpret statuses, dependencies and story content, and return strictly structured data\. It may update user\-facing progress but is not authorized to merge or execute arbitrary shell actions\.
3. **Astra implementation worker:** Receives one fully scoped story and works in its own local Git worktree\. Implements changes and tests them, then reports results\. It does not choose the next story\.
4. **Independent reviewer:** Runs with fresh context and examines the implementation against acceptance criteria, repository rules and the diff\. Produces structured blocking/nonblocking findings; cannot approve its own implementation changes\.
5. **GitHub gate:** Source of truth for PR checks, mergeability and merge commit\. Rust owns the credentials and decision to merge\.
6. **AppFlowy:** Source of truth for business requirements and authorization to begin work\. SQLite is the source of truth for execution state\.

### Access separation

|Capability              |Luna       |Astra                              |Rust                       |
|------------------------|-----------|-----------------------------------|---------------------------|
|Read AppFlowy via MCP   |Yes        |No by default                      |Indirectly via Luna        |
|Interpret/select stories|Yes        |No                                 |Validate and schedule      |
|Edit repository         |No         |Assigned worktree                  |Worktree setup/cleanup     |
|Push feature branch     |Prefer Rust|Optional via restricted credentials|Yes                        |
|Review code             |No         |Separate reviewer session          |Enforce review outcome     |
|Merge PR                |No         |No                                 |Yes, via scoped credentials|
|Manage job state        |No         |No                                 |Yes                        |

Model outputs and AppFlowy content are untrusted inputs: do not allow them to override policy, paths, permissions, repository identity or executable arguments\.

## 4\. Multi\-project configuration

Configuration is TOML\. Each project is identified by a unique stable ID, an AppFlowy URL **or** page name, and an absolute path to an existing Git repository\. URL is preferred; names are allowed only when they resolve unambiguously\. A stable AppFlowy database/view ID is cached once resolved and revalidated periodically\.

```toml
[orchestrator]
poll_interval_seconds = 60
max_concurrent_workers = 2
state_db = "~/.local/state/maistro/state.db"

[agents]
coordinator_model = "gpt-6-luna"
developer_model = "gpt-6-astra"
reviewer_model = "gpt-6-astra"

[github]
auto_merge = true
merge_method = "squash"

[[projects]]
id = "nexus"
name = "Nexus"
appflowy_url = "https://example.invalid/appflowy/board"
repo = "/absolute/path/to/nexus"
base_branch = "main"
max_workers = 1
enabled = true

[[projects]]
id = "mg-companion"
name = "MG Companion"
appflowy_page = "MG Companion Backlog"
repo = "/absolute/path/to/mg-companion"
base_branch = "main"
max_workers = 1
enabled = true
```

Example URLs/paths and model identifiers are placeholders to validate\. Configuration may add per\-project status mappings, CI requirements, repository owner/name, budgets, timeouts and allowed paths\. Secrets must be supplied via OS credential facilities or protected environment files, never committed TOML\.

### Validation

`maistro validate` must verify: distinct IDs, valid absolute repo paths, clean and accessible Git metadata, configured remotes, base branches, local Codex authentication and MCP availability, resolvable AppFlowy boards, readable fields, and sufficient GitHub access\. It must fail closed when a page name is ambiguous or a board cannot be read\.

## 5\. AppFlowy schema and authorization

Recommended Kanban statuses: **Backlog → Ready → In Progress → Review → Done**, with **Blocked** and **Cancelled** side states\.

Recommended fields: title, description/page content, acceptance criteria, priority, repository &#40;optional for one\-board\-per\-repo&#41;, dependency references, status, execution ID, PR URL, last error and retry count\.

Rules:

- Only **Ready** means the user authorizes implementation\. Adding a Backlog story alone does not authorize code changes\.
- Dependencies must be completed/merged before execution\.
- Luna resolves page/board identifiers through MCP, collects story content and outputs normalized JSON\. Rust validates it against a schema\.
- AppFlowy status is a user\-facing projection; do not rely on status updates as transactional locks\.
- If requirements are insufficient, mark **Blocked** with a precise question rather than guessing materially consequential product decisions\.
- If an in\-flight story is edited, compare a stable content fingerprint/revision and stop or replan before merging\.
- If the user cancels or removes authorization, stop pending work and prevent merge\.

### Luna result contract &#40;illustrative&#41;

```json
{
  "project_id": "nexus",
  "board_id": "resolved-id",
  "stories": [
    {
      "story_id": "stable-appflowy-row-id",
      "title": "Add authentication",
      "status": "ready",
      "priority": 1,
      "dependencies": [],
      "acceptance_criteria": ["Users can log in", "Sessions persist"],
      "source_revision": "hash-of-relevant-story-content",
      "decision": "implement",
      "reason": "Requirements sufficiently scoped"
    }
  ]
}
```

All identifiers must be grounded in the MCP output\. Unknown fields and unsupported statuses are handled explicitly\. Ideally Luna reads but Rust executes all AppFlowy mutations through a narrow coordinator request; MVP may allow constrained Luna MCP writes after Rust has committed a state change\.

## 6\. Scheduler and triggers

The MVP uses periodic polling &#40;default 60 seconds&#41;, not an assumed AppFlowy webhook\. Run a full reconciliation on startup and periodically thereafter to recover missed changes\.

|Trigger                     |Action                                          |
|----------------------------|------------------------------------------------|
|Story moves to Ready        |Validate and enqueue once                       |
|Dependency merges           |Reconsider dependent stories                    |
|Worker finishes             |Start review or mark failure                    |
|Review has blocking findings|Retry implementation within budget              |
|CI fails                    |Bounded repair or Blocked                       |
|CI passes                   |Check every merge gate                          |
|PR merges                   |Confirm merge commit, mark Done                 |
|Story is cancelled/changed  |Suspend, cancel or replan; forbid stale merge   |
|Daemon starts/restarts      |Reconcile processes, worktrees, PRs and database|

Schedule fairly across enabled projects &#40;round\-robin at project level, priority within project&#41;, respecting global and per\-project concurrency\. MVP defaults to one worker per repository to avoid integration conflicts\. Use a single daemon instance or an explicit singleton lock\.

## 7\. Execution flow

1. Atomically claim `(project_id, story_id, source_revision)` in SQLite; ensure at most one active run per story\.
2. Fetch latest base branch and create a uniquely named feature branch and isolated Git worktree\. Verify that the configured path is inside an approved repo/worktree root\.
3. Build the implementation prompt from normalized story data, acceptance criteria, repository `AGENTS.md` and pertinent version\-controlled documentation\.
4. Spawn `codex exec` with selected Astra model and bounded write permissions, in the worktree, with structured logging\. Do not construct shell commands using unescaped story text; pass arguments via a process API\.
5. Astra edits code and runs local tests\. Record exit status, event stream, elapsed time, resulting commit/diff and any reported uncertainties\.
6. Spawn a **fresh reviewer session** with the spec and diff\. Record a machine\-parseable verdict and blocking findings\.
7. If defects are found, send findings to an implementation retry in the same task worktree, with a maximum attempt count &#40;default three&#41;\.
8. Push a uniquely named branch, open/update one linked PR, and await required GitHub checks\.
9. If CI fails, perform bounded repair\. If it passes, recheck authorization, requirements revision, base SHA, branch protection, review verdict and changed\-file policies before merge\.
10. Merge through GitHub using squash merge; confirm the resulting merge commit is present on the base branch\. Update SQLite, then AppFlowy to Done\.
11. Reclaim worktrees only after the outcome is durable and no debugging retention policy applies\.

Neither agent’s final prose nor exit code alone counts as proof of test success, review approval or merge completion\.

## 8\. State model and recovery

Internal states: `queued`, `running`, `reviewing`, `waiting_ci`, `merging`, `completed`, `blocked`, `failed`, `cancelled`\.

Allowed transitions are explicit, persisted transactionally and audited\. Review/CI failure transitions back to `running` only if retry budget remains\.

Minimum SQLite entities:

- `projects`: project config snapshot, resolved board ID, last successful scan\.
- `stories`: stable story ID, last seen revision/status, normalized metadata\.
- `runs`: unique run ID, project/story/revision, state, attempt, timestamps, heartbeat, branch, worktree, PR and agent session identifiers\.
- `events`: append\-only state changes, tool outcomes and error classifications\.
- `leases`: active project/repository locks with expiration and owner\.

Invariants:

- At most one active run per story, and no more than configured concurrent runs per repository\.
- Polls and process restarts cannot duplicate jobs or PRs\.
- A timed\-out process is not automatically considered dead; reconcile PID/process group and worktree first\.
- Before retrying, inspect for an existing branch, PR, CI run or merge commit\.
- An AppFlowy update failure after a successful merge is retried without reimplementing the story\.
- Cancellation and changed requirements prevent merging stale work\.
- Use explicit timeouts, process\-group termination, retry limits and backoff\.

## 9\. Merge policy and security

Automatic merge is permitted **only** when all are true:

1. Story remains authorized and source revision matches the reviewed task\.
2. PR targets the configured repository/base branch and corresponds to the recorded run\.
3. Required CI checks are successful for the exact head SHA to merge\.
4. Independent review has no unresolved blocking findings\.
5. GitHub reports the PR mergeable; base changes have been accounted for\.
6. No protected files or operations require human intervention\.

Protected by default: GitHub Actions/CI configuration, authorization and policy files, credentials, production deployment config, live\-data migrations and other high\-risk changes\. The policy engine—not an agent prompt—decides whether a change is protected\. GitHub branch protection/rulesets must backstop local decisions; no bypass token for the daemon\.

Use least\-privilege credentials, no production secrets in worktrees or agent environment, and constrained filesystem/network/tool permissions\. Prefer daemon\-owned branch pushes and PR merges\. Treat MCP text and GitHub comments as prompt\-injection surfaces\. Never auto\-deploy to production in MVP\.

## 10\. CLI and service

Proposed commands:

```text
maistro run
maistro validate
maistro projects
maistro project add
maistro project pause <project-id>
maistro project resume <project-id>
maistro status
maistro pause
maistro resume
maistro retry <run-or-story-id>
maistro reload
```

`project add` interactively requests project name, AppFlowy URL or page name, absolute repository path, base branch and worker limit\. Duplicate identifiers and ambiguous boards are rejected\.

Run via a user\-level systemd unit with restart\-on\-failure and journal logging\. Expose a readable status CLI first; web UI, notifications and metrics dashboard are later enhancements\.

Suggested Rust crates: `tokio`, `serde`, `toml`, `sqlx` &#40;SQLite&#41;, `tracing`, `reqwest`, `clap`, `thiserror`/`anyhow`\. Verify crate versions when implementing\. Use `std::process`/Tokio process APIs rather than shell interpolation\.

## 11\. Implementation milestones

### M1 — Read\-only integration

- Parse and validate multiple configured projects\.
- Invoke Luna through local Codex with the existing AppFlowy MCP\.
- Resolve boards from URLs or names and normalize stories to schema\.
- `maistro validate`, `maistro projects`, `maistro status`; no code writes\.

### M2 — Single\-worker implementation

- Persist jobs in SQLite and create worktrees\.
- Dispatch Astra noninteractively with logs/timeouts\.
- Implement one Ready story, push branch and open draft PR\.
- Update AppFlowy and recover from a daemon restart\.
- Keep automatic merge disabled\.

### M3 — Verification and automatic merge

- Independent reviewer, bounded repair loop and exact\-SHA CI checks\.
- Strict local policy and GitHub\-enforced merge gates\.
- Enable squash merge after adversarial/failure\-path testing\.
- Reconcile AppFlowy after merge without duplicate execution\.

### M4 — Multiple projects and operational resilience

- Fair scheduling, per\-repo locks, cancellation, reload and pause/resume\.
- Robust retry/backoff, crash recovery and diagnostics\.
- Optional notifications, cost budgets and richer planning\.

## 12\. Acceptance tests

1. Two projects with distinct AppFlowy boards and repo paths are validated and polled independently\.
2. A Backlog story does not run; moving it to Ready launches exactly one job without user chat interaction\.
3. Duplicate scans and a process restart do not duplicate active jobs or PRs\.
4. A missing dependency or ambiguous requirement blocks execution and explains why in AppFlowy\.
5. Astra changes only the assigned worktree and cannot merge directly\.
6. Reviewer rejection or CI failure triggers bounded repair; exhaustion yields Blocked, not an infinite loop\.
7. A changed/cancelled story cannot be merged using stale acceptance criteria\.
8. A protected\-file change or failed required check prevents auto\-merge\.
9. An eligible green PR merges once and the associated story becomes Done\.
10. If AppFlowy is temporarily unavailable after merge, reconciliation updates it later without rerunning code\.
11. Two independent projects can execute concurrently without modifying each other’s repositories\.
12. User pause prevents new dispatch; existing job behavior follows a documented pause/cancel policy\.

## 13\. Open implementation details

These are deliberately not hardcoded until verified on the dev server:

- Exact Luna/Astra Codex CLI model IDs, flags, authentication and unattended approvals\.
- Installed AppFlowy MCP tool names, returned identifiers, ability to read page bodies and write statuses\.
- Whether AppFlowy URLs resolve uniquely to databases or to containing pages; fallback to an explicit database ID when needed\.
- Git host and repository permissions &#40;GitHub assumed&#41;; required check names and merge rulesets\.
- CI/test commands supplied by each repository and expected build isolation\.
- Desired operating behavior when the dev server sleeps, the network disappears or usage budgets are exhausted\.

**MVP non\-goals:** self\-generating backlog stories, autonomous architectural redesign, production deployment, cross\-repo atomic changes, a web dashboard, and fully parallel workers within one repository\.
