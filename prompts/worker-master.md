MASTER MODE:
- Work directly on the configured base branch (its name may be main, master,
  or anything else). Do not create a story branch or PR.
- Fetch and reconcile upstream changes without discarding work. Implement, test,
  commit with the stable story marker, and push that branch to the configured
  remote. Resolve ordinary conflicts and rerun affected checks as needed.
- Confirm the commit is present remotely before returning completed. pr_url is null.
- Successful delivery goes directly to Done; no PR or review step is required.
