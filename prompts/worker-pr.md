PR MODE:
- Use the supplied stable branch name for this story. Reuse it when recovering.
- For new work, start from the configured remote/base branch after fetching it;
  do not base a new story on another story's unmerged branch.
- Implement and test, commit with the stable story marker in the commit message,
  push the story branch to the configured remote, then create or update its PR
  targeting the configured base branch. Use gh or the repository's normal PR tool.
- Include the stable story marker, summary, and actual test results in the PR.
  Confirm the push and PR creation succeeded, then return completed with commit
  and pr_url. Do not wait for CI, enable auto-merge, or merge anything.
