use chrono::{DateTime, Utc};
use octocrab::models::pulls::{MergeableState, ReviewState};
use octocrab::models::{IssueState, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub login: String,
    pub id: UserId,
}

impl Default for User {
    fn default() -> Self {
        Self {
            login: "".to_string(),
            id: UserId(0),
        }
    }
}

impl From<octocrab::models::UserProfile> for User {
    fn from(value: octocrab::models::UserProfile) -> Self {
        Self {
            login: value.login,
            id: value.id,
        }
    }
}

impl From<octocrab::models::Author> for User {
    fn from(value: octocrab::models::Author) -> Self {
        Self {
            login: value.login,
            id: value.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Label {
    pub name: String,
}

impl From<octocrab::models::Label> for Label {
    fn from(value: octocrab::models::Label) -> Self {
        Self { name: value.name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrBranch {
    pub label: Option<String>,
    pub ref_field: String,
    pub sha: String,
}

impl From<octocrab::models::pulls::Head> for PrBranch {
    fn from(value: octocrab::models::pulls::Head) -> Self {
        Self {
            label: value.label,
            ref_field: value.ref_field,
            sha: value.sha,
        }
    }
}

impl From<octocrab::models::pulls::Base> for PrBranch {
    fn from(value: octocrab::models::pulls::Base) -> Self {
        Self {
            label: value.label,
            ref_field: value.ref_field,
            sha: value.sha,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: Option<IssueState>,
    pub mergeable_state: Option<MergeableState>,
    pub merge_commit_sha: Option<String>,
    pub assignees: Vec<User>,
    pub requested_reviewers: Vec<User>,
    pub user: Option<User>,
    pub labels: Vec<Label>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub draft: Option<bool>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub head: PrBranch,
    pub base: PrBranch,
}

impl From<octocrab::models::pulls::PullRequest> for PullRequest {
    fn from(value: octocrab::models::pulls::PullRequest) -> Self {
        Self {
            number: value.number,
            title: value.title.unwrap_or_default(),
            body: value.body.unwrap_or_default(),
            state: value.state,
            mergeable_state: value.mergeable_state,
            merge_commit_sha: value.merge_commit_sha,
            assignees: value.assignees.into_iter().flatten().map(|a| a.into()).collect(),
            requested_reviewers: value.requested_reviewers.into_iter().flatten().map(|a| a.into()).collect(),
            user: value.user.map(|u| (*u).into()),
            labels: value.labels.into_iter().flatten().map(|l| l.into()).collect(),
            created_at: value.created_at,
            updated_at: value.updated_at,
            draft: value.draft,
            closed_at: value.closed_at,
            merged_at: value.merged_at,
            head: (*value.head).into(),
            base: (*value.base).into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Review {
    pub user: Option<User>,
    pub state: Option<ReviewState>,
}

impl From<octocrab::models::pulls::Review> for Review {
    fn from(value: octocrab::models::pulls::Review) -> Self {
        Self {
            user: value.user.map(|u| u.into()),
            state: value.state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Commit {
    pub sha: String,
    pub tree: Option<String>,
}

impl From<octocrab::models::repos::RepoCommit> for Commit {
    fn from(value: octocrab::models::repos::RepoCommit) -> Self {
        Self {
            sha: value.sha,
            tree: None,
        }
    }
}

impl From<octocrab::models::commits::Commit> for Commit {
    fn from(value: octocrab::models::commits::Commit) -> Self {
        Self {
            sha: value.sha,
            tree: Some(value.commit.tree.sha),
        }
    }
}

impl From<octocrab::models::commits::GitCommitObject> for Commit {
    fn from(value: octocrab::models::commits::GitCommitObject) -> Self {
        Self {
            sha: value.sha,
            tree: Some(value.tree.sha),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    #[default]
    Queued,
    InProgress,
    Completed,
    Waiting,
    Requested,
    Pending,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunConclusion {
    Success,
    Failure,
    #[default]
    Neutral,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq, Default)]
pub struct CheckRunEvent {
    pub id: i64,
    pub name: String,
    pub head_sha: String,
    pub html_url: Option<String>,
    pub details_url: Option<String>,
    pub url: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: CheckRunStatus,
    pub conclusion: Option<CheckRunConclusion>,
}
