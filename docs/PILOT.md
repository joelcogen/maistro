# Maistro self-pilot

The pilot Kanban was created and verified using AppFlowy MCP on 2026-11-10.

- Parent: https://appflowy.com/app/f757bab8-f38b-49a2-9338-6a524d0646e2/f7b2406b-c5de-4f36-b5d1-c4935627b150
- Board: https://appflowy.com/app/f757bab8-f38b-49a2-9338-6a524d0646e2/8983b67b-9d14-42c0-a0fa-b9cdad15c501
- Database: `7606135a-e2a1-4426-8984-3a0d9c6e177c`
- Local configuration: `/home/jole/dev/maistro/config.toml` (Git-ignored)
- Repository: `/home/jole/dev/maistro`
- Runtime root: `/home/jole/.local/state/maistro` (outside the managed repository)

Statuses: Backlog, Ready, In Progress, Review, Done, Blocked, Cancelled.
Fields: Title, Status, Description, Acceptance criteria, Priority, Dependencies,
Execution ID, PR URL, Last error, Retry count. Grouping is by Status.
The three automatically generated empty sample cards were removed. No executable
story was added. Story-writing instructions are on the parent AppFlowy page.

The board accepts draft stories now. Automated execution is **not ready**:

1. The local repository has no initial commit or GitHub origin. Confirm the
   destination and establish a committed, clean `main` matching that remote.
2. Set `github_repo` to the actual destination; `unconfigured/maistro` is an
   explicit placeholder, not an existing repository claim.
3. Supply `MAISTRO_GITHUB_TOKEN` through a protected environment file. GitHub Actions
   is configured in `.github/workflows/ci.yml`, and `required_checks = ["test"]`
   matches its job name. Verify the first remote run after publishing the repository.
4. Validate the project with `enabled = true`, exercise the real integration,
   then start the daemon. Keep `auto_merge = false` for the initial pilot.

`maistro -c config.toml projects` parsed and displayed the mapping successfully.
`maistro -c config.toml validate` failed because `MAISTRO_GITHUB_TOKEN` is absent.
Its empty provisional SQLite database was preserved as
`/home/jole/.local/state/maistro/preflight-unconfigured.db`, so it does not pin
the placeholder repository identity in the eventual operational database.

No remote was created or pushed, no branch protection was changed, and no service
was started. See README.md for the credential, validation and service commands.

## GitHub Actions

The `test` check runs formatting, strict Clippy, all automated tests including
host-isolation fixtures, and a release build on Ubuntu 22.04 with Rust 1.93.1.
CI uses read-only repository permissions, persists no checkout credentials, and
requires no AppFlowy, Codex or daemon GitHub secrets. Live model probes are excluded.
Push events cover every branch so the daemon can inspect checks on the exact PR
head commit; pull requests and manual dispatch also run CI. No GitHub-hosted run
has been performed yet. Configure the repository's required status check as `test`
when setting up branch protection; this change does not alter protection settings.

Local verification on 2026-11-11 passed: Actionlint 1.7.12, YAML and shell syntax,
configured check-name alignment, formatting, strict Clippy, debug/release builds,
and all 36 tests including the host fixtures.

## Start and inspect

After the repository destination and token are configured, set `enabled = true`
in the pilot configuration. Leave automatic merge off during initial verification.
The protected `~/.config/maistro/environment` file must contain
`MAISTRO_GITHUB_TOKEN=...`. Then install and validate:

```sh
cd /home/jole/dev/maistro
install -Dm755 target/release/maistro "$HOME/.local/bin/maistro"
install -Dm600 config.toml "$HOME/.config/maistro/config.toml"
install -Dm644 systemd/maistro.service "$HOME/.config/systemd/user/maistro.service"
set -a
. "$HOME/.config/maistro/environment"
set +a
"$HOME/.local/bin/maistro" -c "$HOME/.config/maistro/config.toml" validate
```

Start only after validation succeeds:

```sh
systemctl --user daemon-reload
systemctl --user enable --now maistro.service
systemctl --user status maistro.service --no-pager
"$HOME/.local/bin/maistro" -c "$HOME/.config/maistro/config.toml" status
journalctl --user -u maistro.service -n 100 -f
```

The service uses the installed configuration copy, not the repository copy.
After installing later configuration changes, request `maistro -c
~/.config/maistro/config.toml reload`. Stop with
`systemctl --user stop maistro.service`.
