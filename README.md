# Maistro

Maistro turns AppFlowy stories into tested pull requests. Move a story to Ready,
and it uses Luna to read the requirements, Astra to write the code, and a separate
Astra session to review it. Rust runs the tests, checks GitHub CI and decides
whether the change can merge. It runs on your server without an open chat.

Each project connects one AppFlowy board to one local Git repository. Work happens
in separate Git worktrees, and SQLite keeps track of it across restarts.
Automatic merging is off by default.

## Install

You need Linux, Git, Bubblewrap, Rust 1.89 or newer, and a logged-in Codex CLI with
AppFlowy MCP configured. The host must allow Bubblewrap's user, process and network
namespaces. Development used Codex 0.167.0; CI uses Rust 1.93.1. Keep Codex's helper
executables beside its main binary.

```sh
cd ~/dev/maistro
cargo build --release --locked
install -Dm755 target/release/maistro "$HOME/.local/bin/maistro"
mkdir -p "$HOME/.config/maistro"
chmod 700 "$HOME/.config/maistro"
cp --update=none config.example.toml "$HOME/.config/maistro/config.toml"
chmod 600 "$HOME/.config/maistro/config.toml"
```

For the existing Maistro pilot, use the local `config.toml` instead of the example.
The daemon will read the installed copy in `~/.config/maistro/`.

## Configure a project

Edit `~/.config/maistro/config.toml`. For each project, set:

- A stable `id` and an absolute `repo` path. The repository needs a clean working
  tree, an initial commit, an existing base branch and a GitHub `origin`.
- `github_repo` as `owner/repository`, matching that remote.
- One `appflowy_url` or an exact `appflowy_page` name. Use the URL if names overlap.
- `test_commands`, `required_checks` and `enabled = true` when setup is complete.

Projects must use different repositories. Runtime data defaults to
`~/.local/state/maistro/`: `state.db`, `worktrees/` and `logs/`. This location belongs
to the operating-system user running Maistro and is shared by that daemon's
configured projects. Omit `state_db`, `worktree_root` and `log_root` to use these
defaults, or override them individually. Runtime paths beginning with `~/` expand
using `HOME`; other relative paths resolve from the configuration file's directory.
Keep runtime data outside managed repositories. Changing paths does not migrate
an existing installation. Stop the daemon before moving data, preserve the
database, and reconcile any recorded worktree paths before restarting.

Test commands are argument arrays, such as `["cargo", "test", "--locked",
"--offline"]`. Tests run without network access, so prepare dependencies first.
They must exit successfully without changing source files; ignore build outputs
in Git.

When a check fails, Maistro sends the command and its stdout/stderr to the coding
agent, which diagnoses the failure, repairs the code and reruns the checks.
Repairs remain bounded by `max_attempts`. Rust projects also get a compiler and
linker check inside the sandbox before stories are dispatched.

Use `toolchain_paths` to expose specific toolchains and caches read-only. For
Cargo, mount a registry directory such as `/home/jole/.cargo/registry`. Maistro
also exposes it as the worker's Cargo registry. A mounted directory with a `bin`
child is added to `PATH`. Keep credentials out of these directories.

Maistro reads the existing `appflowy` MCP entry from `$CODEX_HOME/config.toml`, or
`~/.codex/config.toml` by default. Guarded AppFlowy writes currently support the
stdio transport. Workers need Codex's `auth.json`; keyring-only authentication
won't work. Set `agents.codex` to an absolute path if the service can't find Codex.
Validation checks the installed CLI and configured models.

## Create the GitHub token

Use a **personal access token (classic)** for this version. Maistro reads the
Checks API, which GitHub doesn't currently support with fine-grained personal
access tokens.

The account needs Write access to the managed repositories. You can use your
repository-owner account while `github.auto_merge = false`. Automatic merging
requires a non-administrator account with no protection-bypass role. This check
uses the account's repository role, not the token's `admin:*` scope checkboxes.

1. Sign in as that account and open [Create a classic personal access token](https://github.com/settings/tokens/new).
2. Set **Note** to `Maistro` and choose an expiration, for example 30 days.
3. Under **Select scopes**, check **repo**. Leave its included child scopes selected.
4. Leave every other top-level scope unchecked, including `workflow`, `admin:org`,
   `delete_repo`, and package scopes.
5. Click **Generate token** and save it in the environment file below. If your
   organization uses SAML SSO, authorize the token through **Configure SSO** on
   [Tokens (classic)](https://github.com/settings/tokens).

The `repo` scope covers Git pushes, pull requests, merges and private-repository
CI reads. A classic token can access all repositories available to its account;
it has no repository picker. Limit the dedicated account's access accordingly.
If your organization blocks classic tokens, this setup is blocked by that policy.

Create the local credential file:

```sh
umask 077
mkdir -p "$HOME/.config/maistro"
chmod 700 "$HOME/.config/maistro"
touch "$HOME/.config/maistro/environment"
chmod 600 "$HOME/.config/maistro/environment"
${EDITOR:-vi} "$HOME/.config/maistro/environment"
```

Add one line, replacing the placeholder with the generated token:

```dotenv
MAISTRO_GITHUB_TOKEN=PASTE_TOKEN_HERE
```

Keep the token in this file, outside Git and TOML. Maistro reads it locally; the
GitHub Actions workflow doesn't need it as a secret. When you replace an expired
token, update this file and restart the service so it picks up the new value.

## Prepare the board and CI

Use these board statuses: Backlog, Ready, In Progress, Review, Done, Blocked and
Cancelled. The `Status` select field must match `projects.statuses`. You can map
Ready to an existing label such as To Do.

Only Ready authorizes work. Describe the outcome you want; observable requirements
in prose are enough. The agent chooses ordinary implementation and design details
using repository conventions and reasonable defaults. Examples, mockups and exact
output formats are optional. Luna blocks a story only when a concrete obstacle
prevents meaningful progress, and explains why the agent cannot resolve it.

Dependencies refer to row IDs on the same board. Maistro waits for them to become
Done and explains the wait in a comment.

Stories and human comments may reference other AppFlowy pages. Luna reads linked
documents and resolves named guides through the workspace folder. Rust includes
the fetched text in the worker's context and revision checks. Only stories on the
configured board authorize work; reference documents do not create extra tasks.

Optional fields are `Execution ID`, `PR URL`, `Last error` and `Retry count`.
Maistro fills these in; underscore spellings also work. See the
[pilot setup](docs/PILOT.md) for the existing board and local paths.

The included [GitHub Actions workflow](.github/workflows/ci.yml) runs formatting,
Clippy, all automated tests and a release build. Its check is named `test`, so the
pilot uses `required_checks = ["test"]`. Verify its first GitHub run after pushing
the repository. Other projects need their own check names.

Keep `github.auto_merge = false` for the first trial. Maistro can open and review
PRs while you handle merging. When the base branch advances, it updates the PR
branch and asks the coding agent to resolve text conflicts within the repair
budget. Clean base updates do not consume repair attempts or invoke the coding
agent. It reruns local checks and independent review before pushing to the same
PR, then checks CI for the new commit. This also works with automatic merging
disabled and never uses a force push. Conflicts in protected files or files the
agent cannot safely edit require human handling.

To enable automatic squash merges, it also needs
read access to branch protection and rulesets, strict up-to-date required checks,
protection enforced for administrators, and no force pushes, deletions or bypass
actors. Token scopes don't grant access the account itself lacks. If protection
reads fail, automatic merging stays blocked.

Maistro currently requires classic branch protection, even when rulesets exist.
It doesn't change GitHub protection settings. Test the full flow on a disposable
project before enabling automatic merge.

## Validate and start

Load the token and check the configuration:

```sh
set -a
. "$HOME/.config/maistro/environment"
set +a
"$HOME/.local/bin/maistro" -c "$HOME/.config/maistro/config.toml" validate
```

Validation reads GitHub and AppFlowy, checks the repository, models and sandbox,
and writes local diagnostics. Fix any reported errors before starting.

Install and start the user service:

```sh
cd ~/dev/maistro
install -Dm644 systemd/maistro.service "$HOME/.config/systemd/user/maistro.service"
systemctl --user daemon-reload
systemctl --user enable --now maistro.service
```

Check the service, jobs and logs:

```sh
systemctl --user status maistro.service --no-pager
"$HOME/.local/bin/maistro" -c "$HOME/.config/maistro/config.toml" status
journalctl --user -u maistro.service -n 100 -f
```

Stop it with `systemctl --user stop maistro.service`. To keep it running after you
log out, an administrator can run `sudo loginctl enable-linger "$USER"`.

For a foreground run, use `maistro -c PATH run`. Polling defaults to 60 seconds.
`run --once` performs one cycle and can write code, push branches and open PRs.
Use `validate` when you only want to check the setup.

## Commands

All commands accept `-c PATH`. Without it, Maistro reads `config.toml` from the
current directory. The systemd unit explicitly uses `~/.config/maistro/config.toml`.

| Command | What it does |
| --- | --- |
| `run [--once]` | Poll boards and process authorized work |
| `validate` | Check setup and read boards |
| `projects` | Show configured projects |
| `status` | Show runs, errors and pending updates |
| `config` | Open the selected config file in `$EDITOR` |
| `project add` | Add a project interactively and validate it |
| `project pause ID` / `project resume ID` | Pause or resume one project |
| `pause` / `resume` | Pause or resume all projects |
| `retry RUN_OR_STORY_ID` | Retry a terminal run with a fresh attempt budget |
| `cancel RUN_OR_STORY_ID` | Stop work and keep the branch and PR for inspection |
| `reload` | Validate configuration changes after active work drains |

Output is human-readable by default. For example, `maistro projects` shows:

```text
Projects (1)

demo — Demo (enabled)
  Repository: /home/alice/demo
  Board: Demo board
  GitHub: alice/demo
  Base branch: main
```

`status` shows dispatch state, event and pending-update counts, project pause and
last-scan information, and runs with full IDs, story titles, state, attempts,
branches, worktrees, PR links and errors. An empty run list says `No runs recorded.`
Last-scan timestamps are labeled Unix seconds. `validate` prints one success line
per enabled project, such as `demo: valid (board BOARD_ID, 3 stories)`.
Control commands confirm recorded changes or requests; a request confirmation does
not mean the daemon has finished processing it.

For scripts, use `maistro --json projects`, `maistro status --json`, or
`maistro --json validate`. This preserves the previous projects array, status
object, and one JSON object per validated project. Control commands remain silent
in JSON mode. Logs go to stderr (JSON with `--json`, plain text otherwise).
`project add` remains interactive, with prompts on stderr and a JSON result on
stdout when `--json` is set. Internal bridge and MCP stdout protocols are unchanged.

Run `EDITOR=nano maistro config` or `EDITOR='code --wait' maistro -c PATH config`.
The command waits for the editor and reports a failure if it exits unsuccessfully.
`EDITOR` is required and interpreted as a shell command, supporting arguments and
quoted executable paths; the config path is passed as a separate literal argument.
It opens the same path selected by `-c` (default: `./config.toml`) without parsing
or validating it, allowing broken files to be repaired and missing files to be
created by the editor. Parent directories must already exist. Editing does not
reload a running daemon; use `reload` separately. `--json` does not change editor I/O.

Pause lets active local work and review finish, but holds new jobs and merges.
Cancellation stops local work; it can't undo a merge GitHub has already accepted.
Changed requirements or revoked authorization prevent stale work from merging.

Retry requires the same requirements revision and Ready status. Use the run ID
when a story has several revisions. Reload keeps runtime paths and project
identity fixed; use a new project ID for a different board, repository or base
branch. Invalid reloads keep the previous configuration. Keep a project enabled
while an uncertain `merging` run is being reconciled.

## Test and recover

Run the full checks on a Linux host that permits namespaces and local listeners:

```sh
cargo build --locked --bins
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --include-ignored
```

The ignored tests use disposable repositories and fake external services. They
need host isolation features, which is why the command includes them explicitly.
`cargo run --example worker_probe` is a separate live Astra call that writes and
checks a file in a temporary sandbox.

Workers can access their assigned source and selected tools. Reviewers get
read-only source. Tests have no network or model credentials. Rust owns GitHub
credentials, checks test and review evidence, and enforces merge rules. An agent's
success report can't approve a merge. Changes to protected files, including CI,
policy, authentication, deployment and migrations, require human handling.

Restart with the same configuration and SQLite database to recover interrupted
work. Maistro checks existing branches, PRs and merge results before repeating
remote writes. AppFlowy updates retry separately, so an outage after a merge
doesn't rerun implementation. Worktrees are retained by default. Setting
`retain_worktrees = false` removes only clean, completed worktrees; branches, PRs
and logs remain.

See [architecture](docs/ARCHITECTURE.md) for isolation and recovery details,
[operations](docs/OPERATIONS.md) for retries and backups, and
[acceptance evidence](docs/ACCEPTANCE.md) for passed tests and unverified live
integrations. Maistro currently supports Linux, GitHub.com and stdio AppFlowy MCP.
It doesn't deploy the changes it merges.
