use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::http;
use futures::TryStreamExt;
use moka::future::Cache;
use octocrab::models::commits::{Commit, GitCommitObject};
use octocrab::models::pulls::PullRequest;
use octocrab::models::repos::{Object, Ref, RepoCommit};
use octocrab::models::{Installation, InstallationRepositories, Repository, RepositoryId, UserId, UserProfile};
use octocrab::params::repos::Reference;
use octocrab::{params, GitHubError, Octocrab};
use parking_lot::Mutex;

use super::config::{GitHubBrawlRepoConfig, Permission, Role};
use super::messages::{CommitMessage, IssueMessage};

#[derive(Debug)]
pub struct InstallationClient {
    client: Octocrab,
    installation: Mutex<Installation>,
    repositories: Mutex<HashMap<RepositoryId, Arc<(Repository, GitHubBrawlRepoConfig)>>>,

    pulls: Cache<(RepositoryId, u64), Arc<PullRequest>>,

    // This is a cache of user profiles that we load due to this installation.
    users: Cache<UserId, Arc<UserProfile>>,
    users_by_name: Cache<String, UserId>,

    teams: Cache<String, Vec<UserId>>,
    team_users: Cache<(RepositoryId, Role), Vec<UserId>>,
}

#[derive(Debug, thiserror::Error)]
pub enum GitHubBrawlRepoConfigError {
    #[error("expected 1 file, got {0}")]
    ExpectedOneFile(usize),
    #[error("github error: {0}")]
    GitHub(#[from] octocrab::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("missing content")]
    MissingContent,
}

pub trait GitHubInstallationClient: Send + Sync {
    type RepoClient: GitHubRepoClient;

    fn get_repository(&self, repo_id: RepositoryId) -> Option<Self::RepoClient>;

    fn get_repository_by_name(&self, name: &str) -> Option<Self::RepoClient>;

    fn fetch_repositories(&self) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn repositories(&self) -> Vec<RepositoryId>;

    fn set_repository(&self, repo: Repository) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn remove_repository(&self, repo_id: RepositoryId);

    fn fetch_repository(&self, id: RepositoryId) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn get_user(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<Arc<UserProfile>>> + Send;

    fn get_user_by_name(&self, name: &str) -> impl std::future::Future<Output = anyhow::Result<Arc<UserProfile>>> + Send;

    fn get_team_users(&self, team: &str) -> impl std::future::Future<Output = anyhow::Result<Vec<UserId>>> + Send;

    fn installation(&self) -> Installation;

    fn owner(&self) -> String;

    fn update_installation(&self, installation: Installation);
}

impl InstallationClient {
    pub async fn new(client: Octocrab, installation: Installation) -> anyhow::Result<Self> {
        Ok(Self {
            client,
            installation: Mutex::new(installation),
            repositories: Mutex::new(HashMap::new()),
            pulls: Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(60 * 10)) // 10 minutes
                .build(),
            users: moka::future::Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(60 * 10)) // 10 minutes
                .build(),
            users_by_name: moka::future::Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(60 * 10)) // 10 minutes
                .build(),
            teams: moka::future::Cache::builder()
                .max_capacity(50)
                .time_to_live(Duration::from_secs(60 * 5)) // 5 minutes
                .build(),
            team_users: moka::future::Cache::builder()
                .max_capacity(50)
                .time_to_live(Duration::from_secs(60 * 5)) // 5 minutes
                .build(),
        })
    }

    async fn get_repo_config(&self, repo_id: RepositoryId) -> Result<GitHubBrawlRepoConfig, GitHubBrawlRepoConfigError> {
        let file = match self
            .client
            .repos_by_id(repo_id)
            .get_content()
            .path(".github/brawl.toml")
            .send()
            .await
        {
            Ok(file) => file,
            Err(octocrab::Error::GitHub {
                source:
                    GitHubError {
                        status_code: http::StatusCode::NOT_FOUND,
                        ..
                    },
                ..
            }) => {
                return Ok(GitHubBrawlRepoConfig::missing());
            }
            Err(e) => return Err(e.into()),
        };

        if file.items.is_empty() {
            return Ok(GitHubBrawlRepoConfig::missing());
        }

        if file.items.len() != 1 {
            return Err(GitHubBrawlRepoConfigError::ExpectedOneFile(file.items.len()));
        }

        let config = toml::from_str(
            &file.items[0]
                .decoded_content()
                .ok_or(GitHubBrawlRepoConfigError::MissingContent)?,
        )?;

        Ok(config)
    }
}

impl GitHubInstallationClient for Arc<InstallationClient> {
    type RepoClient = RepoClient;

    async fn fetch_repositories(&self) -> anyhow::Result<()> {
        let mut repositories = Vec::new();
        let mut page = 1;
        loop {
            let resp: InstallationRepositories = self
                .client
                .get(format!("/installation/repositories?per_page=100&page={page}"), None::<&()>)
                .await
                .context("get installation repositories")?;

            repositories.extend(resp.repositories);

            if repositories.len() >= resp.total_count as usize {
                break;
            }

            page += 1;
        }

        let mut repo_map = HashMap::new();
        for repo in repositories {
            let config = self.get_repo_config(repo.id).await?;
            repo_map.insert(repo.id, Arc::new((repo, config)));
        }

        *self.repositories.lock() = repo_map;

        Ok(())
    }

    fn repositories(&self) -> Vec<RepositoryId> {
        self.repositories.lock().keys().cloned().collect()
    }

    async fn get_user(&self, user_id: UserId) -> anyhow::Result<Arc<UserProfile>> {
        self.users
            .try_get_with::<_, octocrab::Error>(user_id, async {
                let user = self.client.users_by_id(user_id).profile().await?;
                self.users_by_name.insert(user.login.to_lowercase(), user_id).await;
                Ok(Arc::new(user))
            })
            .await
            .context("get user profile")
    }

    async fn get_user_by_name(&self, name: &str) -> anyhow::Result<Arc<UserProfile>> {
        let user_id = self
            .users_by_name
            .try_get_with::<_, octocrab::Error>(name.trim_start_matches('@').to_lowercase(), async {
                let user = self.client.users(name).profile().await?;
                let user_id = user.id;
                self.users.insert(user_id, Arc::new(user)).await;
                Ok(user_id)
            })
            .await
            .context("get user by name")?;

        self.get_user(user_id).await
    }

    fn get_repository(&self, repo_id: RepositoryId) -> Option<RepoClient> {
        self.repositories
            .lock()
            .get(&repo_id)
            .map(|repo| RepoClient::new(repo.clone(), self.clone()))
    }

    fn get_repository_by_name(&self, name: &str) -> Option<RepoClient> {
        for repo in self.repositories.lock().values() {
            if repo.0.name.eq_ignore_ascii_case(name) {
                return Some(RepoClient::new(repo.clone(), self.clone()));
            }
        }
        None
    }

    async fn fetch_repository(&self, id: RepositoryId) -> anyhow::Result<()> {
        let repo = self.client.repos_by_id(id).get().await.context("get repository")?;
        self.set_repository(repo).await.context("set repository")?;
        Ok(())
    }

    async fn set_repository(&self, repo: Repository) -> anyhow::Result<()> {
        let config = self.get_repo_config(repo.id).await.context("get repo config")?;
        self.repositories.lock().insert(repo.id, Arc::new((repo, config)));
        Ok(())
    }

    fn remove_repository(&self, repo_id: RepositoryId) {
        self.repositories.lock().remove(&repo_id);
    }

    fn installation(&self) -> Installation {
        self.installation.lock().clone()
    }

    fn owner(&self) -> String {
        self.installation.lock().account.login.clone()
    }

    fn update_installation(&self, installation: Installation) {
        tracing::info!("updated installation: {} ({})", installation.account.login, installation.id);
        *self.installation.lock() = installation;
    }

    async fn get_team_users(&self, team: &str) -> anyhow::Result<Vec<UserId>> {
        self.teams
            .try_get_with_by_ref::<_, octocrab::Error, _>(team, async {
                let team = self.client.teams(self.owner()).members(team).per_page(100).send().await?;

                let users = team.into_stream(&self.client).try_collect::<Vec<_>>().await?;
                Ok(users.into_iter().map(|u| u.id).collect())
            })
            .await
            .context("get team users")
    }
}

pub struct RepoClient {
    repo: Arc<(Repository, GitHubBrawlRepoConfig)>,
    installation: Arc<InstallationClient>,
}

pub trait GitHubRepoClient: Send + Sync {
    fn id(&self) -> RepositoryId;
    fn get(&self) -> &Repository;
    fn config(&self) -> &GitHubBrawlRepoConfig;
    fn owner(&self) -> &str;
    fn name(&self) -> &str;

    fn pr_link(&self, pr_number: u64) -> String {
        format!(
            "https://github.com/{owner}/{repo}/pull/{pr_number}",
            owner = self.owner(),
            repo = self.name(),
            pr_number = pr_number,
        )
    }

    fn commit_link(&self, sha: &str) -> String {
        format!(
            "https://github.com/{owner}/{repo}/commit/{sha}",
            owner = self.owner(),
            repo = self.name(),
            sha = sha,
        )
    }

    fn get_user(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<Arc<UserProfile>>> + Send;

    fn get_user_by_name(&self, name: &str) -> impl std::future::Future<Output = anyhow::Result<Arc<UserProfile>>> + Send;

    fn get_pull_request(&self, number: u64) -> impl std::future::Future<Output = anyhow::Result<Arc<PullRequest>>> + Send;

    fn set_pull_request(&self, pull_request: PullRequest) -> impl std::future::Future<Output = ()> + Send;

    fn get_role_members(&self, role: Role) -> impl std::future::Future<Output = anyhow::Result<Vec<UserId>>> + Send;

    fn send_message(
        &self,
        issue_number: u64,
        message: &IssueMessage,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn get_commit(&self, sha: &str) -> impl std::future::Future<Output = anyhow::Result<RepoCommit>> + Send;

    fn create_merge(
        &self,
        message: &CommitMessage,
        base_sha: &str,
        head_sha: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<Commit>> + Send;

    fn create_commit(
        &self,
        message: String,
        parents: Vec<String>,
        tree: String,
    ) -> impl std::future::Future<Output = anyhow::Result<GitCommitObject>> + Send;

    fn get_commit_by_sha(&self, sha: &str) -> impl std::future::Future<Output = anyhow::Result<Option<Commit>>> + Send;

    fn get_ref(
        &self,
        gh_ref: &params::repos::Reference,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<Ref>>> + Send;

    fn push_branch(
        &self,
        branch: &str,
        sha: &str,
        force: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn delete_branch(&self, branch: &str) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn has_permission(
        &self,
        user_id: UserId,
        permissions: &[Permission],
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;

    fn can_merge(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        self.has_permission(user_id, &self.config().merge_permissions)
    }

    fn can_try(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        self.has_permission(user_id, self.config().try_permissions())
    }
}

impl RepoClient {
    fn new(repo: Arc<(Repository, GitHubBrawlRepoConfig)>, installation: Arc<InstallationClient>) -> Self {
        Self { repo, installation }
    }
}

impl GitHubRepoClient for RepoClient {
    fn id(&self) -> RepositoryId {
        self.repo.0.id
    }

    fn get(&self) -> &Repository {
        &self.repo.0
    }

    fn config(&self) -> &GitHubBrawlRepoConfig {
        &self.repo.1
    }

    fn name(&self) -> &str {
        &self.get().name
    }

    fn owner(&self) -> &str {
        &self.repo.0.owner.as_ref().expect("repository has no owner").login
    }

    async fn get_user(&self, user_id: UserId) -> anyhow::Result<Arc<UserProfile>> {
        self.installation.get_user(user_id).await
    }

    async fn get_user_by_name(&self, name: &str) -> anyhow::Result<Arc<UserProfile>> {
        self.installation.get_user_by_name(name).await
    }

    async fn get_pull_request(&self, number: u64) -> anyhow::Result<Arc<PullRequest>> {
        self.installation
            .pulls
            .try_get_with::<_, octocrab::Error>((self.id(), number), async {
                let pull_request = self.installation.client.pulls(self.owner(), self.name()).get(number).await?;
                Ok(Arc::new(pull_request))
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

    async fn get_commit(&self, sha: &str) -> anyhow::Result<RepoCommit> {
        self.installation
            .client
            .commits(self.owner(), self.name())
            .get(sha)
            .await
            .context("get commit")
    }

    async fn create_merge(&self, message: &CommitMessage, base_sha: &str, head_sha: &str) -> anyhow::Result<Commit> {
        let tmp_branch = format!(
            "{prefix}/{id}",
            prefix = self.config().temp_branch_prefix.trim_end_matches('/'),
            id = uuid::Uuid::new_v4(),
        );

        self.push_branch(&tmp_branch, base_sha, true)
            .await
            .context("push tmp branch")?;

        let commit = self
            .installation
            .client
            .post::<_, Commit>(
                format!("/repos/{owner}/{repo}/merges", owner = self.owner(), repo = self.name()),
                Some(&serde_json::json!({
                    "base": tmp_branch,
                    "head": head_sha,
                    "commit_message": message.as_ref(),
                })),
            )
            .await
            .context("create commit");

        if let Err(e) = self.delete_branch(&tmp_branch).await {
            tracing::error!("failed to delete tmp branch: {:#}", e);
        }

        commit
    }

    async fn create_commit(&self, message: String, parents: Vec<String>, tree: String) -> anyhow::Result<GitCommitObject> {
        self.installation
            .client
            .repos_by_id(self.id())
            .create_git_commit_object(message, tree)
            .parents(parents)
            .send()
            .await
            .context("create commit")
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
            .get::<Commit, _, _>(
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
            Ok(commit) => Ok(Some(commit)),
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

    async fn get_ref(&self, gh_ref: &params::repos::Reference) -> anyhow::Result<Option<Ref>> {
        match self.installation.client.repos_by_id(self.id()).get_ref(gh_ref).await {
            Ok(r) => Ok(Some(r)),
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
}

fn is_object_sha(object: &Object, sha: &str) -> bool {
    match object {
        Object::Commit { sha: o, .. } => o == sha,
        Object::Tag { sha: o, .. } => o == sha,
        _ => false,
    }
}
