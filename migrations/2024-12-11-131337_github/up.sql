CREATE TYPE github_pr_status AS ENUM (
    -- PR is in a pending state, waiting for a new action from the user
    'pending_update',
    -- PR is in a pending state, waiting for a new action from the reviewer
    'pending_review',
    -- PR is in the queue to be merged
    'in_queue',
    -- PR is being merged
    'in_progress',
    -- PR was merged
    'merged',
    -- PR was rolled up by another PR
    'rolled_up',
    -- PR was closed without being merged
    'closed'
);

CREATE TYPE github_pr_rollup_eligibility AS ENUM (
    -- PR is allowed to be rolled up
    'can',
    -- PR is not allowed to be rolled up
    'cannot',
    -- The rollup status was not specified
    'unknown',
    -- The PR itself is a rollup of other PRs (we cannot rollup prs that are rollups)
    'is_rollup'
);

-- A table specifically for specifying a merge queue for a GitHub repository
CREATE TABLE github_pr_merge_queue (
    -- The ID of the GitHub repository
    github_repo_id BIGINT NOT NULL,
    -- The ID of the PR on GitHubq
    github_pr_id BIGINT NOT NULL,
    -- If the PR is mergable
    is_mergable BOOLEAN NOT NULL,
    -- If this is a dry run and we don't want to actually merge the PR.
    is_dry_run BOOLEAN NOT NULL,
    -- The ID of the user that created the PR
    author_id BIGINT NOT NULL,
    -- The IDs of the user who was assigned to review the PR
    reviewer_ids BIGINT[] NOT NULL DEFAULT '{}',
    -- The IDs of the users who approved the PR
    approver_ids BIGINT[] NOT NULL DEFAULT '{}',
    -- The status of the PR
    status github_pr_status NOT NULL,
    -- The priority of the PR
    priority INTEGER NOT NULL DEFAULT 5,
    -- The rollup eligibility of the PR
    rollup_eligibility github_pr_rollup_eligibility NOT NULL DEFAULT 'unknown',
    -- Sometimes a PR might have been rolled up by another PR, this would denote the
    -- PR that rolled it up.
    rolled_up_github_pr_id BIGINT,
    -- The SHA of the latest commit of the PR
    head_commit_sha TEXT NOT NULL,
    -- The SHA of the latest commit of the target branch
    base_commit_sha TEXT NOT NULL,
    -- The branch of the PR
    head_ref TEXT NOT NULL,
    -- The target branch of the PR
    base_ref TEXT NOT NULL,
    -- The SHA of the commit that would be merged if the PR was merged
    merge_commit_sha TEXT,
    -- The time this entry was completed
    completed_at TIMESTAMPTZ,
    -- The time the PR was created
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- The time the PR was last updated
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (github_repo_id, github_pr_id),
    FOREIGN KEY (github_repo_id, rolled_up_github_pr_id) REFERENCES github_pr_merge_queue(github_repo_id, github_pr_id)
);

-- A base-ref of a github repo can only have one non-dry-run PR running at any given time.
CREATE UNIQUE INDEX
    github_pr_merge_queue_base_ref_idx
ON github_pr_merge_queue (github_repo_id, base_ref)
WHERE is_dry_run = FALSE AND status = 'in_progress';
