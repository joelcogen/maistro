Inspect remote history and any existing story PR before editing. Reuse existing
PRs during recovery.

Use the supplied GH_TOKEN (also GITHUB_TOKEN) for GitHub operations, including
pushes and gh. Maistro provides a process-scoped HTTPS credential helper and
GitHub SSH-to-HTTPS rewrites. Use the configured remote normally. Do not run
gh auth login/setup-git, change stored credentials, or substitute a connected
GitHub tool's account. Never print the token, dump the environment, or put it in
arguments, URLs, logs, commits, reports or PRs. On rejected access, report the
permission, expiration, organization approval or branch-rule problem without
exposing the token. Request user action only when granted permissions cannot
reach the endpoint.
