🔒 Merge conflict
This pull request and the `{target_branch}` branch have diverged in a way
that cannot be automatically merged. Please rebase your branch ontop of the
latest `{target_branch}` branch and let the reviewer approve again.

Attempted merge from {head_sha} into {base_sha}

<details><summary>How do I rebase?</summary>

1. `git checkout {source_branch}` *(Switch to your branch)*
2. `git fetch upstream {target_branch}` *(Fetch the latest changes from the
   upstream)*
3. `git rebase upstream/{target_branch} -p` *(Rebase your branch onto the
   upstream branch)*
4. Follow the prompts to resolve any conflicts (use `git status` if you get
   lost).
5. `git push self {source_branch} --force-with-lease` *(Update this PR)*`

You may also read
 [*Git Rebasing to Resolve Conflicts* by Drew Blessing](http://blessing.io/git/git-rebase/open-source/2015/08/23/git-rebasing-to-resolve-conflicts.html)
 for a short tutorial.

Please avoid the ["**Resolve conflicts**" button](https://help.github.com/articles/resolving-a-merge-conflict-on-github/) on GitHub.
 It uses `git merge` instead of `git rebase` which makes the PR commit
history more difficult to read.

Sometimes step 4 will complete without asking for resolution. This is usually
due to difference between how `Cargo.lock` conflict is handled during merge
and rebase. This is normal, and you should still perform step 5 to update
this PR.

</details>
