#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueMessage {
    CommitApproved(String),
    Error(String),
    MergeConflict(String),
    TestsFailed(String),
    TestsPassMerge(String),
    TestsPass(String),
    TestsTimeout(String),
    TestsStart(String),
    Pong(String),
}

impl AsRef<str> for IssueMessage {
    fn as_ref(&self) -> &str {
        match self {
            IssueMessage::CommitApproved(s) => s.as_ref(),
            IssueMessage::Error(s) => s.as_ref(),
            IssueMessage::MergeConflict(s) => s.as_ref(),
            IssueMessage::TestsFailed(s) => s.as_ref(),
            IssueMessage::TestsPassMerge(s) => s.as_ref(),
            IssueMessage::TestsPass(s) => s.as_ref(),
            IssueMessage::TestsTimeout(s) => s.as_ref(),
            IssueMessage::TestsStart(s) => s.as_ref(),
            IssueMessage::Pong(s) => s.as_ref(),
        }
    }
}

impl std::fmt::Display for IssueMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_ref())
    }
}

pub fn commit_approved(
    commit_link: impl std::fmt::Display,
    requested_by: impl std::fmt::Display,
    approved_by: impl std::fmt::Display,
) -> IssueMessage {
    IssueMessage::CommitApproved(format!(
        include_str!("commit_approved.md"),
        commit_link = commit_link,
        requested_by = requested_by,
        approved_by = approved_by,
    ))
}

#[derive(Debug, Clone)]
pub struct CommitMessage(String);

impl AsRef<str> for CommitMessage {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_ref())
    }
}

pub fn commit_message(
    issue_link: impl std::fmt::Display,
    branch: impl std::fmt::Display,
    reviewers: impl std::fmt::Display,
    title: impl std::fmt::Display,
    body: impl std::fmt::Display,
    reviewed_by: impl std::fmt::Display,
) -> CommitMessage {
    CommitMessage(format!(
        include_str!("commit_message.md"),
        issue_link = issue_link,
        branch = branch,
        reviewers = reviewers,
        title = title,
        body = body,
        reviewed_by = reviewed_by
    ))
}

pub fn error(title: impl std::fmt::Display, error: impl std::fmt::Display) -> IssueMessage {
    IssueMessage::Error(format!(include_str!("error.md"), title = title, error = error,))
}

pub fn error_no_body(title: impl std::fmt::Display) -> IssueMessage {
    IssueMessage::Error(format!(include_str!("error_no_body.md"), title = title,))
}

pub fn merge_conflict(
    source_branch: impl std::fmt::Display,
    target_branch: impl std::fmt::Display,
    head_sha_link: impl std::fmt::Display,
    base_sha_link: impl std::fmt::Display,
) -> IssueMessage {
    IssueMessage::MergeConflict(format!(
        include_str!("merge_conflict.md"),
        source_branch = source_branch,
        target_branch = target_branch,
        head_sha = head_sha_link,
        base_sha = base_sha_link,
    ))
}

pub fn tests_failed(check: impl std::fmt::Display, check_link: impl std::fmt::Display) -> IssueMessage {
    IssueMessage::TestsFailed(format!(
        include_str!("tests_failed.md"),
        check = check,
        check_link = check_link,
    ))
}

pub fn tests_pass_merge(
    duration: impl std::fmt::Display,
    checks_message: impl std::fmt::Display,
    reviewers: impl std::fmt::Display,
    commit_link: impl std::fmt::Display,
    branch: impl std::fmt::Display,
) -> IssueMessage {
    IssueMessage::TestsPassMerge(format!(
        include_str!("tests_pass_merge.md"),
        duration = duration,
        checks_message = checks_message,
        reviewers = reviewers,
        commit_link = commit_link,
        branch = branch,
    ))
}

pub fn tests_pass(
    duration: impl std::fmt::Display,
    checks_message: impl std::fmt::Display,
    requested_by: impl std::fmt::Display,
    commit_link: impl std::fmt::Display,
    commit_sha: impl std::fmt::Display,
) -> IssueMessage {
    IssueMessage::TestsPass(format!(
        include_str!("tests_pass.md"),
        duration = duration,
        checks_message = checks_message,
        requested_by = requested_by,
        commit_link = commit_link,
        commit_sha = commit_sha,
    ))
}

pub fn tests_timeout(timeout: impl std::fmt::Display, missing_checks: impl std::fmt::Display) -> IssueMessage {
    IssueMessage::TestsTimeout(format!(
        include_str!("tests_timeout.md"),
        timeout = timeout,
        missing_checks = missing_checks,
    ))
}

pub fn tests_start(head_sha_link: impl std::fmt::Display, base_sha_link: impl std::fmt::Display) -> IssueMessage {
    IssueMessage::TestsStart(format!(
        include_str!("tests_start.md"),
        head_sha_link = head_sha_link,
        base_sha_link = base_sha_link,
    ))
}

pub fn pong(username: impl std::fmt::Display, status: impl std::fmt::Display) -> IssueMessage {
    IssueMessage::Pong(format!(include_str!("pong.md"), username = username, status = status,))
}

pub fn format_fn<F>(f: F) -> FormatFn<F>
where
    F: Fn(&mut std::fmt::Formatter) -> std::fmt::Result,
{
    FormatFn(f)
}

pub struct FormatFn<F>(F);

impl<F> std::fmt::Display for FormatFn<F>
where
    F: Fn(&mut std::fmt::Formatter) -> std::fmt::Result,
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0(f)
    }
}
