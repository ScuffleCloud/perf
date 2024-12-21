use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::http;
use futures::TryStreamExt;
use moka::future::Cache;
use octocrab::models::{Installation, InstallationRepositories, Repository, RepositoryId, UserId};
use octocrab::{GitHubError, Octocrab};
use parking_lot::Mutex;

use super::config::{GitHubBrawlRepoConfig, Role};
use super::models::{PullRequest, User};
use super::repo::{GitHubRepoClient, RepoClient};

#[derive(Debug)]
pub struct InstallationClient {
    pub(super) client: Octocrab,
    installation: Mutex<Installation>,
    repositories: Mutex<HashSet<RepositoryId>>,

    pub(super) repos: Cache<RepositoryId, Arc<(Repository, GitHubBrawlRepoConfig)>>,
    pub(super) pulls: Cache<(RepositoryId, u64), Arc<PullRequest>>,

    // This is a cache of user profiles that we load due to this installation.
    pub(super) users: Cache<UserId, Arc<User>>,
    pub(super) users_by_name: Cache<String, UserId>,

    pub(super) teams: Cache<String, Vec<UserId>>,
    pub(super) team_users: Cache<(RepositoryId, Role), Vec<UserId>>,
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

    fn get_repository(
        &self,
        repo_id: RepositoryId,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<Self::RepoClient>>> + Send;

    fn fetch_repositories(&self) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn repositories(&self) -> Vec<RepositoryId>;

    fn has_repository(&self, repo_id: RepositoryId) -> bool;

    fn set_repository(&self, repo: Repository) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn remove_repository(&self, repo_id: RepositoryId);

    fn fetch_repository(&self, id: RepositoryId) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn get_user(&self, user_id: UserId) -> impl std::future::Future<Output = anyhow::Result<Arc<User>>> + Send;

    fn get_user_by_name(&self, name: &str) -> impl std::future::Future<Output = anyhow::Result<Arc<User>>> + Send;

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
            repositories: Mutex::new(HashSet::new()),
            repos: Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(60 * 5)) // 5 minutes
                .build(),
            pulls: Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(30)) // 30 seconds
                .build(),
            users: moka::future::Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(60 * 5)) // 5 minutes
                .build(),
            users_by_name: moka::future::Cache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(60 * 5)) // 5 minutes
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

        let mut repos = HashSet::new();
        for repo in repositories {
            repos.insert(repo.id);
        }

        *self.repositories.lock() = repos;

        Ok(())
    }

    fn repositories(&self) -> Vec<RepositoryId> {
        self.repositories.lock().iter().cloned().collect()
    }

    fn has_repository(&self, repo_id: RepositoryId) -> bool {
        self.repositories.lock().contains(&repo_id)
    }

    async fn get_user(&self, user_id: UserId) -> anyhow::Result<Arc<User>> {
        self.users
            .try_get_with::<_, octocrab::Error>(user_id, async {
                let user = self.client.users_by_id(user_id).profile().await?;
                self.users_by_name.insert(user.login.to_lowercase(), user_id).await;
                Ok(Arc::new(user.into()))
            })
            .await
            .context("get user profile")
    }

    async fn get_user_by_name(&self, name: &str) -> anyhow::Result<Arc<User>> {
        let user_id = self
            .users_by_name
            .try_get_with::<_, octocrab::Error>(name.trim_start_matches('@').to_lowercase(), async {
                let user = self.client.users(name).profile().await?;
                let user_id = user.id;
                self.users.insert(user_id, Arc::new(user.into())).await;
                Ok(user_id)
            })
            .await
            .context("get user by name")?;

        self.get_user(user_id).await
    }

    async fn get_repository(&self, repo_id: RepositoryId) -> anyhow::Result<Option<RepoClient>> {
        if !self.repositories.lock().contains(&repo_id) {
            return Ok(None);
        }

        self.repos
            .try_get_with::<_, anyhow::Error>(repo_id, async {
                let repo = self.client.repos_by_id(repo_id).get().await?;
                let config = self.get_repo_config(repo_id).await?;
                Ok(Arc::new((repo, config)))
            })
            .await
            .map(|repo| Some(RepoClient::new(repo.clone(), self.clone())))
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn fetch_repository(&self, id: RepositoryId) -> anyhow::Result<()> {
        let repo = self.client.repos_by_id(id).get().await.context("get repository")?;
        self.set_repository(repo).await.context("set repository")?;
        Ok(())
    }

    async fn set_repository(&self, repo: Repository) -> anyhow::Result<()> {
        let config = self.get_repo_config(repo.id).await.context("get repo config")?;
        self.repos.insert(repo.id, Arc::new((repo, config))).await;
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
