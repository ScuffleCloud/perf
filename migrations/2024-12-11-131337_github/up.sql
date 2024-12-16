CREATE TYPE github_ci_run_status AS ENUM (
    'queued',
    'running',
    'success',
    'failure',
    'timed_out'
);

CREATE TABLE github_ci_runs (
    id SERIAL PRIMARY KEY,
    github_repo_id BIGINT NOT NULL,
    -- On GitHub issue numbers are also PR numbers, the difference is that PRs have a commit SHA / HEAD ref
    -- and issues don't.
    github_issue_number INT NOT NULL,
    status github_ci_run_status NOT NULL,
    -- The SHA of the base commit (the commit we are merging into)
    base_commit_sha TEXT NOT NULL,
    -- The SHA of the head commit (the commit we are merging from)
    head_commit_sha TEXT NOT NULL,
    -- A concurrency group only allows one CI run to be active at a time.
    ci_branch TEXT NOT NULL,
    -- The priority of the CI run (higher priority runs are run first)
    priority INT NOT NULL,
    -- The ID of the user who requested the CI run (on GitHub)
    requested_by_id BIGINT NOT NULL,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- This index enforces the concurrency group constraint, ensuring that only one CI run
-- can be running at a time on a given repository concurrency group.
CREATE UNIQUE INDEX github_ci_runs_ci_branch_idx ON github_ci_runs (github_repo_id, ci_branch) WHERE status = 'running';


CREATE TYPE github_pr_status AS ENUM (
    'open',
    'closed',
    'draft'
);

CREATE TYPE github_pr_merge_status AS ENUM (
    -- Not ready to merge
    'not_ready',
    -- Can be merged
    'ready',
    -- Has a merge conflict
    'conflict',
    -- CI Checks failed prior to merging
    'check_failure',
    -- Merge failed during the run
    'merge_failure',
    -- Merged
    'merged'
);

CREATE TABLE github_pr_merge_queue (
    github_repo_id BIGINT NOT NULL,
    github_pr_number INT NOT NULL,

    -- The title of the PR (on GitHub)
    title TEXT NOT NULL,
    -- The body of the PR (on GitHub)
    body TEXT NOT NULL,

    -- The merge status of the PR (on GitHub)
    merge_status github_pr_merge_status NOT NULL,
    -- The ID of the user who created the PR (on GitHub)
    author_id BIGINT NOT NULL,
    -- The IDs of the users who reviewed the PR (via the Brawl command) (max 10 - no nulls)
    reviewer_ids BIGINT[] NOT NULL CHECK (array_length(reviewer_ids, 1) <= 10 AND array_position(reviewer_ids, NULL) IS NULL),
    -- The IDs of the users who are assigned to the PR (on GitHub) (max 10 - no nulls)
    assigned_ids BIGINT[] NOT NULL CHECK (array_length(assigned_ids, 1) <= 10 AND array_position(assigned_ids, NULL) IS NULL),
    -- The status of the PR (on GitHub)
    status github_pr_status NOT NULL,
    -- The ID of the CI run that merged this PR (if it was merged)
    merge_ci_run_id INT,
    -- The SHA of the merge commit (if the PR was merged)
    merge_commit_sha TEXT,
    -- The target branch of the PR
    target_branch TEXT NOT NULL,
    -- The source branch of the PR
    source_branch TEXT NOT NULL,
    -- The SHA of the latest commit on the PR
    latest_commit_sha TEXT NOT NULL,
    -- The time the PR was created
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- The time the PR was last updated
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- The default priority of the PR
    default_priority INT,

    FOREIGN KEY (merge_ci_run_id) REFERENCES github_ci_runs(id) ON DELETE SET NULL,
    PRIMARY KEY (github_repo_id, github_pr_number)
);
