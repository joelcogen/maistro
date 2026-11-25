LOCAL MASTER MODE:
- Switch to the existing local base branch in GIT.base_branch and work directly
  there. GIT.remote is false. Do not fetch, push, create a story branch, or use GitHub.
- Search the local base branch's history for the exact GIT.story_marker in commit
  messages. If the commit already delivers the story, verify it and reuse its SHA.
- Implement and test any remaining work and commit it with GIT.story_marker.
  Verify that the reported commit is reachable from the local base branch.
- Successful delivery goes directly to Done with the full commit SHA. Set pr_url
  to null.
