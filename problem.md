make github bot that handles merge requests + can rollup PRs.

## Wants

### 2 stage CI pipeline

We want to have a 2 stage CI pipeline.

1. The first stage is the CI pipeline that runs on every PR doing the basic checks.
- linting
- formatting
- basic tests

2. The second stage is the CI pipeline that runs on a merge to main.
- additional longer running tests

### Performance benchmarks

We do not want to run performance benchmarks on every PR.

So we want a bot that can trigger a workflow to run the performance benchmarks and then update the PR with the results.

### Some way to rollup multiple PRs

We want to be able to rollup multiple PRs into a single workflow run.

## Commands

`@brawl r+`

`@brawl r+ rollup=<state> p=<priority> r=<username>`

    - Denotes you approve this PR & queues it for merge.
    - `rollup=<state>` can be:
        - "deny" - this PR should never be rolled up
        - "allow" - this PR should always be rolled up (if you don't provide a state this is the default)
    - `p=<priority>` is an integer denoting the priority of this PR (higher is more important)
    - `r=<username>` is the username of the person who is approving this PR (they must have previously approved this PR via `@brawl r+`)
    - queues this PR for merge (commits after the approval will not be tested, and therefore will not be merged)

`@brawl r-` 

    - stops whatever is running or queued for this PR
    - remove the label from this PR

`@brawl try`

`@brawl try commit=<sha>`

    - adds a label saying this PR is being tested (if you provide a commit sha, we will trigger the run on that specific commit, otherwise the PR will be tested on the latest commit pushed @ the time of the command)
    - runs the 2nd stage CI for this PR (will report the results to the PR)

## Rolling up PRs

We should have a website that lets you select multiple PRs and then have it create a new PR with the changes from all of the selected PRs.

Similar to how rust homu works.

The PR title will look like this:

```
Rollup of <number_of_prs> pull requests

Successful merges:

- #<pr_number_1> - (<pr_title_1>)
- #<pr_number_2> - (<pr_title_2>)
- #<pr_number_3> - (<pr_title_3>)
```

## Merging

When the bot runs a merge, we will create a temporary branch and then merge that PR into that branch.

This will create a merge commit with the following message:

```
Auto merge of #<pr_number> - <branch_name>, r=<github_username>

<pr_title>

<pr_body>
```

1. CI/CD will then run on this branch.
2. Report results to the PR
3. If successful & we are not trying; push the commit to the target branch of the PR.

## Some stuff to be aware of

This will always use merge commits, meaning non-linear history on the target branch.

However we must require that PRs themselves are linear.

