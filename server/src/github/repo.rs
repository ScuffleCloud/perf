use std::sync::Arc;

use anyhow::Context;
use axum::http;
use octocrab::models::repos::{Object, Ref};
use octocrab::models::{Repository, RepositoryId, UserId};
use octocrab::params::repos::Reference;
use octocrab::params::{self};
use octocrab::GitHubError;

use super::config::{GitHubBrawlRepoConfig, Permission, Role};
use super::installation::{GitHubInstallationClient, InstallationClient};
use super::merge_workflow::{DefaultMergeWorkflow, GitHubMergeWorkflow};
use super::messages::{CommitMessage, IssueMessage};
use super::models::{Commit, PullRequest, Review, User};

pub struct RepoClient {
    repo: Arc<(Repository, GitHubBrawlRepoConfig)>,
    installation: Arc<InstallationClient>,
}

#[derive(Debug, Clone)]
pub enum MergeResult {
    Success(Commit),
    Conflict,
}

pub trait GitHubRepoClient: Send + Sync {
    type MergeWorkflow<'a>: GitHubMergeWorkflow
    where
        Self: 'a;

    /// The ID of the repository
    fn id(&self) -> RepositoryId;

    /// The repository configuration
    fn config(&self) -> &GitHubBrawlRepoConfig;

    /// The owner of the repository
    fn owner(&self) -> &str;

    /// The name of the repository
    fn name(&self) -> &str;

    /// The merge workflow for this client
    fn merge_workflow(&self) -> Self::MergeWorkflow<'_>;

    /// A link to a pull request in the repository
    fn pr_link(&self, pr_number: u64) -> String {
        format!(
            "https://github.com/{owner}/{repo}/pull/{pr_number}",
            owner = self.owner(),
            repo = self.name(),
            pr_number = pr_number,
        )
    }

    /// A link to a commit in the repository
    fn commit_link(&self, sha: &str) -> String {
        format!(
            "https://github.com/{owner}/{repo}/commit/{sha}",
            owner = self.owner(),
            repo = self.name(),
            sha = sha,
        )
    }

    /// Get a user by their ID
    fn get_user(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<Arc<User>>> + Send;

    /// Get a user by their username
    fn get_user_by_name(&self, name: &str) -> impl std::future::Future<Output = anyhow::Result<Arc<User>>> + Send;

    /// Get a pull request by its number
    fn get_pull_request(&self, number: u64) -> impl std::future::Future<Output = anyhow::Result<Arc<PullRequest>>> + Send;

    /// Set a pull request
    fn set_pull_request(&self, pull_request: PullRequest) -> impl std::future::Future<Output = ()> + Send;

    /// Get the members of a role
    fn get_role_members(&self, role: Role) -> impl std::future::Future<Output = anyhow::Result<Vec<UserId>>> + Send;

    /// Get the reviewers of a pull request
    fn get_reviewers(&self, pr_number: u64) -> impl std::future::Future<Output = anyhow::Result<Vec<Review>>> + Send;

    /// Send a message to a pull request
    fn send_message(
        &self,
        issue_number: u64,
        message: &IssueMessage,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    /// Get a commit by its SHA
    fn get_commit(&self, sha: &str) -> impl std::future::Future<Output = anyhow::Result<Commit>> + Send;

    /// Create a merge commit
    fn create_merge(
        &self,
        message: &CommitMessage,
        base_sha: &str,
        head_sha: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<MergeResult>> + Send;

    /// Create a commit
    fn create_commit(
        &self,
        message: String,
        parents: Vec<String>,
        tree: String,
    ) -> impl std::future::Future<Output = anyhow::Result<Commit>> + Send;

    /// Get a commit by its SHA
    fn get_commit_by_sha(&self, sha: &str) -> impl std::future::Future<Output = anyhow::Result<Option<Commit>>> + Send;

    /// Get a reference by its name
    fn get_ref_latest_commit(
        &self,
        gh_ref: &params::repos::Reference,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<Commit>>> + Send;

    /// Push a branch to the repository
    fn push_branch(
        &self,
        branch: &str,
        sha: &str,
        force: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    /// Delete a branch from the repository
    fn delete_branch(&self, branch: &str) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    /// Check if a user has a permission
    fn has_permission(
        &self,
        user_id: UserId,
        permissions: &[Permission],
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;

    /// Check if a user can merge
    fn can_merge(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        self.has_permission(user_id, &self.config().merge_permissions)
    }

    /// Check if a user can try
    fn can_try(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        self.has_permission(user_id, self.config().try_permissions())
    }

    /// Check if a user can review
    fn can_review(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        self.has_permission(user_id, self.config().reviewer_permissions())
    }
}

impl RepoClient {
    pub(super) fn new(repo: Arc<(Repository, GitHubBrawlRepoConfig)>, installation: Arc<InstallationClient>) -> Self {
        Self { repo, installation }
    }
}

impl GitHubRepoClient for RepoClient {
    type MergeWorkflow<'a> = DefaultMergeWorkflow;

    fn id(&self) -> RepositoryId {
        self.repo.0.id
    }

    fn merge_workflow(&self) -> Self::MergeWorkflow<'_> {
        DefaultMergeWorkflow
    }

    fn config(&self) -> &GitHubBrawlRepoConfig {
        &self.repo.1
    }

    fn name(&self) -> &str {
        &self.repo.0.name
    }

    fn owner(&self) -> &str {
        &self.repo.0.owner.as_ref().expect("repository has no owner").login
    }

    async fn get_user(&self, user_id: UserId) -> anyhow::Result<Arc<User>> {
        self.installation.get_user(user_id).await
    }

    async fn get_user_by_name(&self, name: &str) -> anyhow::Result<Arc<User>> {
        self.installation.get_user_by_name(name).await
    }

    async fn get_pull_request(&self, number: u64) -> anyhow::Result<Arc<PullRequest>> {
        self.installation
            .pulls
            .try_get_with::<_, octocrab::Error>((self.id(), number), async {
                let pull_request = self.installation.client.pulls(self.owner(), self.name()).get(number).await?;
                Ok(Arc::new(pull_request.into()))
            })
            .await
            .context("get pull request")
    }

    async fn set_pull_request(&self, pull_request: PullRequest) {
        self.installation
            .pulls
            .insert((self.id(), pull_request.number), Arc::new(pull_request))
            .await;
    }

    async fn get_role_members(&self, role: Role) -> anyhow::Result<Vec<UserId>> {
        self.installation
            .team_users
            .try_get_with::<_, octocrab::Error>((self.id(), role), async {
                let users = self
                    .installation
                    .client
                    .repos_by_id(self.id())
                    .list_collaborators()
                    .permission(role.into())
                    .send()
                    .await?;

                Ok(users.items.into_iter().map(|c| c.author.id).collect())
            })
            .await
            .context("get role members")
    }

    async fn send_message(&self, issue_number: u64, message: &IssueMessage) -> anyhow::Result<()> {
        self.installation
            .client
            .issues_by_id(self.id())
            .create_comment(issue_number, message)
            .await
            .context("send message")?;
        Ok(())
    }

    async fn get_commit(&self, sha: &str) -> anyhow::Result<Commit> {
        self.installation
            .client
            .commits(self.owner(), self.name())
            .get(sha)
            .await
            .context("get commit")
            .map(|c| c.into())
    }

    async fn create_merge(&self, message: &CommitMessage, base_sha: &str, head_sha: &str) -> anyhow::Result<MergeResult> {
        let tmp_branch = self.config().temp_branch();

        self.push_branch(&tmp_branch, base_sha, true)
            .await
            .context("push tmp branch")?;

        let commit = match self
            .installation
            .client
            .post::<_, octocrab::models::commits::Commit>(
                format!("/repos/{owner}/{repo}/merges", owner = self.owner(), repo = self.name()),
                Some(&serde_json::json!({
                    "base": tmp_branch,
                    "head": head_sha,
                    "commit_message": message.as_ref(),
                })),
            )
            .await
        {
            Ok(c) => Ok(MergeResult::Success(c.into())),
            Err(octocrab::Error::GitHub {
                source:
                    GitHubError {
                        status_code: http::StatusCode::CONFLICT,
                        ..
                    },
                ..
            }) => Ok(MergeResult::Conflict),
            Err(e) => Err(e).context("create merge"),
        };

        if let Err(e) = self.delete_branch(&tmp_branch).await {
            tracing::error!("failed to delete tmp branch: {:#}", e);
        }

        commit
    }

    async fn create_commit(&self, message: String, parents: Vec<String>, tree: String) -> anyhow::Result<Commit> {
        self.installation
            .client
            .repos_by_id(self.id())
            .create_git_commit_object(message, tree)
            .parents(parents)
            .send()
            .await
            .context("create commit")
            .map(|c| c.into())
    }

    async fn push_branch(&self, branch: &str, sha: &str, force: bool) -> anyhow::Result<()> {
        let branch_ref = Reference::Branch(branch.to_owned());

        let current_ref = match self.installation.client.repos_by_id(self.id()).get_ref(&branch_ref).await {
            Ok(r) if is_object_sha(&r.object, sha) => return Ok(()),
            Ok(r) => Some(r),
            Err(octocrab::Error::GitHub {
                source:
                    GitHubError {
                        status_code: http::StatusCode::NOT_FOUND,
                        ..
                    },
                ..
            }) => None,
            Err(e) => return Err(e).context("get ref"),
        };

        if current_ref.is_none() {
            self.installation
                .client
                .repos_by_id(self.id())
                .create_ref(&branch_ref, sha)
                .await
                .context("create ref")?;

            return Ok(());
        }

        self.installation
            .client
            .patch::<Ref, _, _>(
                format!(
                    "/repos/{owner}/{repo}/git/refs/heads/{branch}",
                    owner = self.owner(),
                    repo = self.name(),
                    branch = branch
                ),
                Some(&serde_json::json!({
                    "sha": sha,
                    "force": force,
                })),
            )
            .await
            .context("update ref")?;

        Ok(())
    }

    async fn delete_branch(&self, branch: &str) -> anyhow::Result<()> {
        self.installation
            .client
            .repos_by_id(self.id())
            .delete_ref(&Reference::Branch(branch.to_owned()))
            .await
            .context("delete branch")
    }

    async fn get_commit_by_sha(&self, sha: &str) -> anyhow::Result<Option<Commit>> {
        match self
            .installation
            .client
            .get::<octocrab::models::commits::Commit, _, _>(
                format!(
                    "/repos/{owner}/{repo}/commits/{sha}",
                    owner = self.owner(),
                    repo = self.name(),
                    sha = sha,
                ),
                None::<&()>,
            )
            .await
        {
            Ok(commit) => Ok(Some(commit.into())),
            Err(octocrab::Error::GitHub {
                source:
                    GitHubError {
                        status_code: http::StatusCode::NOT_FOUND,
                        ..
                    },
                ..
            }) => Ok(None),
            Err(e) => Err(e).context("get commit by sha"),
        }
    }

    async fn get_ref_latest_commit(&self, gh_ref: &params::repos::Reference) -> anyhow::Result<Option<Commit>> {
        match self.installation.client.repos_by_id(self.id()).get_ref(gh_ref).await {
            Ok(r) => Ok(Some(match r.object {
                Object::Commit { sha, .. } => Commit { sha, tree: None },
                Object::Tag { sha, .. } => Commit { sha, tree: None },
                _ => return Err(anyhow::anyhow!("invalid object type")),
            })),
            Err(octocrab::Error::GitHub {
                source:
                    GitHubError {
                        status_code: http::StatusCode::NOT_FOUND,
                        ..
                    },
                ..
            }) => Ok(None),
            Err(e) => Err(e).context("get ref"),
        }
    }

    async fn has_permission(&self, user_id: UserId, permissions: &[Permission]) -> anyhow::Result<bool> {
        for permission in permissions {
            match permission {
                Permission::Role(role) => {
                    let users = self.get_role_members(*role).await?;
                    if users.contains(&user_id) {
                        return Ok(true);
                    }
                }
                Permission::Team(team) => {
                    let users = self.installation.get_team_users(team).await?;
                    if users.contains(&user_id) {
                        return Ok(true);
                    }
                }
                Permission::User(user) => {
                    let user = self.installation.get_user_by_name(user).await?;
                    if user.id == user_id {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    async fn get_reviewers(&self, pr_number: u64) -> anyhow::Result<Vec<Review>> {
        let pr = self
            .installation
            .client
            .pulls(self.owner(), self.name())
            .list_reviews(pr_number)
            .per_page(100)
            .send()
            .await?;
        let mut reviewers = pr.items.into_iter().map(|r| r.into()).collect::<Vec<_>>();
        let mut next = pr.next;
        while let Some(page) = self
            .installation
            .client
            .get_page::<octocrab::models::pulls::Review>(&next)
            .await?
        {
            reviewers.extend(page.items.into_iter().map(|r| r.into()));
            if page.next.is_none() {
                break;
            }

            next = page.next;
        }
        Ok(reviewers)
    }
}

fn is_object_sha(object: &Object, sha: &str) -> bool {
    match object {
        Object::Commit { sha: o, .. } => o == sha,
        Object::Tag { sha: o, .. } => o == sha,
        _ => false,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod test_utils {
    use super::*;

    pub struct MockRepoClient<T: GitHubMergeWorkflow> {
        pub id: RepositoryId,
        pub owner: String,
        pub name: String,
        pub config: GitHubBrawlRepoConfig,
        pub actions: tokio::sync::mpsc::Sender<MockRepoAction>,
        pub merge_workflow: T,
    }

    impl<T: GitHubMergeWorkflow> MockRepoClient<T> {
        pub fn new(merge_workflow: T) -> (Self, tokio::sync::mpsc::Receiver<MockRepoAction>) {
            let (tx, rx) = tokio::sync::mpsc::channel(100);
            (
                Self {
                    id: RepositoryId(1),
                    owner: "owner".to_owned(),
                    name: "repo".to_owned(),
                    config: GitHubBrawlRepoConfig::default(),
                    actions: tx,
                    merge_workflow,
                },
                rx,
            )
        }

        pub fn with_id(self, id: RepositoryId) -> Self {
            Self { id, ..self }
        }

        pub fn with_config(self, config: GitHubBrawlRepoConfig) -> Self {
            Self { config, ..self }
        }

        pub fn with_owner(self, owner: String) -> Self {
            Self { owner, ..self }
        }

        pub fn with_name(self, name: String) -> Self {
            Self { name, ..self }
        }
    }

    #[derive(Debug)]
    pub enum MockRepoAction {
        GetUser {
            user_id: UserId,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Arc<User>>>,
        },
        GetUserByName {
            name: String,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Arc<User>>>,
        },
        GetPullRequest {
            number: u64,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Arc<PullRequest>>>,
        },
        SetPullRequest {
            pull_request: Box<PullRequest>,
        },
        GetRoleMembers {
            role: Role,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Vec<UserId>>>,
        },
        GetReviewers {
            pr_number: u64,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Vec<Review>>>,
        },
        SendMessage {
            issue_number: u64,
            message: IssueMessage,
            result: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
        },
        GetCommit {
            sha: String,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Commit>>,
        },
        CreateMerge {
            message: CommitMessage,
            base_sha: String,
            head_sha: String,
            result: tokio::sync::oneshot::Sender<anyhow::Result<MergeResult>>,
        },
        CreateCommit {
            message: String,
            parents: Vec<String>,
            tree: String,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Commit>>,
        },
        GetCommitBySha {
            sha: String,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Option<Commit>>>,
        },
        GetRefLatestCommit {
            gh_ref: params::repos::Reference,
            result: tokio::sync::oneshot::Sender<anyhow::Result<Option<Commit>>>,
        },
        PushBranch {
            branch: String,
            sha: String,
            force: bool,
            result: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
        },
        DeleteBranch {
            branch: String,
            result: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
        },
        HasPermission {
            user_id: UserId,
            permissions: Vec<Permission>,
            result: tokio::sync::oneshot::Sender<anyhow::Result<bool>>,
        },
    }

    impl<T: GitHubMergeWorkflow> GitHubRepoClient for MockRepoClient<T> {
        type MergeWorkflow<'a>
            = &'a T
        where
            T: 'a;

        fn merge_workflow(&self) -> Self::MergeWorkflow<'_> {
            &self.merge_workflow
        }

        fn id(&self) -> RepositoryId {
            self.id
        }

        fn config(&self) -> &GitHubBrawlRepoConfig {
            &self.config
        }

        fn owner(&self) -> &str {
            &self.owner
        }

        fn name(&self) -> &str {
            &self.name
        }

        async fn get_user(&self, user_id: UserId) -> anyhow::Result<Arc<User>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetUser { user_id, result: tx })
                .await
                .expect("send get user");
            rx.await.expect("recv get user")
        }

        async fn get_user_by_name(&self, name: &str) -> anyhow::Result<Arc<User>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetUserByName {
                    name: name.to_string(),
                    result: tx,
                })
                .await
                .expect("send get user by name");
            rx.await.expect("recv get user by name")
        }

        async fn get_pull_request(&self, number: u64) -> anyhow::Result<Arc<PullRequest>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetPullRequest { number, result: tx })
                .await
                .expect("send get pull request");
            rx.await.expect("recv get pull request")
        }

        async fn set_pull_request(&self, pull_request: PullRequest) {
            self.actions
                .send(MockRepoAction::SetPullRequest {
                    pull_request: Box::new(pull_request),
                })
                .await
                .expect("send set pull request");
        }

        async fn get_role_members(&self, role: Role) -> anyhow::Result<Vec<UserId>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetRoleMembers { role, result: tx })
                .await
                .expect("send get role members");
            rx.await.expect("recv get role members")
        }

        async fn get_reviewers(&self, pr_number: u64) -> anyhow::Result<Vec<Review>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetReviewers { pr_number, result: tx })
                .await
                .expect("send get reviewers");
            rx.await.expect("recv get reviewers")
        }

        async fn send_message(&self, issue_number: u64, message: &IssueMessage) -> anyhow::Result<()> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::SendMessage {
                    issue_number,
                    message: message.clone(),
                    result: tx,
                })
                .await
                .expect("send send message");
            rx.await.expect("recv send message")
        }

        async fn get_commit(&self, sha: &str) -> anyhow::Result<Commit> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetCommit {
                    sha: sha.to_string(),
                    result: tx,
                })
                .await
                .expect("send get commit");
            rx.await.expect("recv get commit")
        }

        async fn create_merge(
            &self,
            message: &CommitMessage,
            base_sha: &str,
            head_sha: &str,
        ) -> anyhow::Result<MergeResult> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::CreateMerge {
                    message: message.clone(),
                    base_sha: base_sha.to_string(),
                    head_sha: head_sha.to_string(),
                    result: tx,
                })
                .await
                .expect("send create merge");
            rx.await.expect("recv create merge")
        }

        async fn create_commit(&self, message: String, parents: Vec<String>, tree: String) -> anyhow::Result<Commit> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::CreateCommit {
                    message,
                    parents,
                    tree,
                    result: tx,
                })
                .await
                .expect("send create commit");
            rx.await.expect("recv create commit")
        }

        async fn get_commit_by_sha(&self, sha: &str) -> anyhow::Result<Option<Commit>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetCommitBySha {
                    sha: sha.to_string(),
                    result: tx,
                })
                .await
                .expect("send get commit by sha");
            rx.await.expect("recv get commit by sha")
        }

        async fn get_ref_latest_commit(&self, gh_ref: &params::repos::Reference) -> anyhow::Result<Option<Commit>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::GetRefLatestCommit {
                    gh_ref: gh_ref.clone(),
                    result: tx,
                })
                .await
                .expect("send get ref latest commit");
            rx.await.expect("recv get ref latest commit")
        }

        async fn push_branch(&self, branch: &str, sha: &str, force: bool) -> anyhow::Result<()> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::PushBranch {
                    branch: branch.to_string(),
                    sha: sha.to_string(),
                    force,
                    result: tx,
                })
                .await
                .expect("send push branch");
            rx.await.expect("recv push branch")
        }

        async fn delete_branch(&self, branch: &str) -> anyhow::Result<()> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::DeleteBranch {
                    branch: branch.to_string(),
                    result: tx,
                })
                .await
                .expect("send delete branch");
            rx.await.expect("recv delete branch")
        }

        async fn has_permission(&self, user_id: UserId, permissions: &[Permission]) -> anyhow::Result<bool> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.actions
                .send(MockRepoAction::HasPermission {
                    user_id,
                    permissions: permissions.to_vec(),
                    result: tx,
                })
                .await
                .expect("send has permission");
            rx.await.expect("recv has permission")
        }
    }
}
