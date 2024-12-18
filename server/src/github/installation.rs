use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use axum::http;
use futures::TryStreamExt;
use moka::future::Cache;
use octocrab::models::commits::{Commit, GitCommitObject};
use octocrab::models::issues::Issue;
use octocrab::models::pulls::PullRequest;
use octocrab::models::repos::{Object, Ref, RepoCommit};
use octocrab::models::{Installation, InstallationRepositories, Repository, RepositoryId, UserId, UserProfile};
use octocrab::params::repos::Reference;
use octocrab::{params, GitHubError, Octocrab};
use parking_lot::Mutex;

use super::config::{GitHubBrawlRepoConfig, Permission, Role};

#[derive(Debug)]
pub struct InstallationClient {
	client: Octocrab,
	installation: Mutex<Installation>,
	repositories: Mutex<HashMap<RepositoryId, Repository>>,
	repositories_by_name: Mutex<HashMap<String, RepositoryId>>,

	pulls: Cache<(RepositoryId, u64), PullRequest>,

	repo_configs: Cache<RepositoryId, GitHubBrawlRepoConfig>,

	// This is a cache of user profiles that we load due to this installation.
	users: Cache<UserId, UserProfile>,
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

impl InstallationClient {
	pub async fn new(client: Octocrab, installation: Installation) -> anyhow::Result<Self> {
		Ok(Self {
			client,
			installation: Mutex::new(installation),
			repositories: Mutex::new(HashMap::new()),
			repositories_by_name: Mutex::new(HashMap::new()),
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
			repo_configs: moka::future::Cache::builder()
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

	pub async fn fetch_repositories(&self) -> anyhow::Result<()> {
		let mut repositories = HashMap::new();
		let mut page = 1;
		loop {
			let resp: InstallationRepositories = self
				.client
				.get(format!("/installation/repositories?per_page=100&page={page}"), None::<&()>)
				.await
				.context("get installation repositories")?;

			repositories.extend(resp.repositories.into_iter().map(|repo| (repo.id, repo)));

			if repositories.len() >= resp.total_count as usize {
				break;
			}

			page += 1;
		}

		let mut repositories_by_name = HashMap::new();
		for (repo_id, repo) in &repositories {
			repositories_by_name.insert(repo.name.to_lowercase(), *repo_id);
		}

		*self.repositories.lock() = repositories;
		*self.repositories_by_name.lock() = repositories_by_name;

		Ok(())
	}

	pub fn repositories(&self) -> HashMap<RepositoryId, RepoClient> {
		self.repositories
			.lock()
			.keys()
			.copied()
			.map(|id| (id, RepoClient::new(id, self)))
			.collect()
	}

	pub fn has_repository(&self, repo_id: RepositoryId) -> bool {
		self.repositories.lock().contains_key(&repo_id)
	}

	pub async fn get_user(&self, user_id: UserId) -> anyhow::Result<UserProfile> {
		self.users
			.try_get_with::<_, octocrab::Error>(user_id, async {
				let user = self.client.users_by_id(user_id).profile().await?;
				self.users_by_name.insert(user.login.to_lowercase(), user_id).await;
				Ok(user)
			})
			.await
			.context("get user profile")
	}

	pub async fn get_user_by_name(&self, name: &str) -> anyhow::Result<UserProfile> {
		let user_id = self
			.users_by_name
			.try_get_with::<_, octocrab::Error>(name.trim_start_matches('@').to_lowercase(), async {
				let user = self.client.users(name).profile().await?;
				let user_id = user.id;
				self.users.insert(user_id, user).await;
				Ok(user_id)
			})
			.await
			.context("get user by name")?;

		self.get_user(user_id).await
	}

	pub fn get_repository(&self, repo_id: RepositoryId) -> Option<RepoClient> {
		if self.repositories.lock().contains_key(&repo_id) {
			Some(RepoClient::new(repo_id, self))
		} else {
			None
		}
	}

	pub fn get_repository_by_name(&self, name: &str) -> Option<RepoClient> {
		if let Some(repo_id) = self.repositories_by_name.lock().get(name) {
			self.get_repository(*repo_id)
		} else {
			None
		}
	}

	pub async fn fetch_repository(&self, id: RepositoryId) -> anyhow::Result<()> {
		let repo = self.client.repos_by_id(id).get().await?;
		self.set_repository(repo).await;
		Ok(())
	}

	pub async fn set_repository(&self, repo: Repository) {
		let name = repo.name.to_lowercase();
		let repo_id = repo.id;
		let old = self.repositories.lock().insert(repo_id, repo);
		if let Some(old) = old {
			tracing::info!("updated repository: {}/{}", self.name(), name);
			let old_name = old.name.to_lowercase();
			if old_name != name {
				self.repositories_by_name.lock().remove(&old_name);
			}
		} else {
			self.repositories_by_name.lock().insert(name.clone(), repo_id);
			tracing::info!("added repository: {}/{}", self.name(), name);
		}
		self.repo_configs.remove(&repo_id).await;
	}

	pub async fn remove_repository(&self, repo_id: RepositoryId) {
		let repo = self.repositories.lock().remove(&repo_id);
		if let Some(repo) = repo {
			self.repositories_by_name.lock().remove(&repo.name.to_lowercase());
			tracing::info!("removed repository: {}/{}", repo.owner.unwrap().login, repo.name);
		}
		self.repo_configs.remove(&repo_id).await;
	}

	pub fn installation(&self) -> Installation {
		self.installation.lock().clone()
	}

	pub fn name(&self) -> String {
		self.installation.lock().account.login.clone()
	}

	pub fn update_installation(&self, installation: Installation) {
		tracing::info!("updated installation: {} ({})", installation.account.login, installation.id);
		*self.installation.lock() = installation;
	}

	pub async fn get_repo_config(&self, repo_id: RepositoryId) -> anyhow::Result<GitHubBrawlRepoConfig> {
		self.repo_configs
			.try_get_with::<_, GitHubBrawlRepoConfigError>(repo_id, async {
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
						source: GitHubError {
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
			})
			.await
			.context("get repo config")
	}

	pub async fn get_team_users(&self, team: &str) -> anyhow::Result<Vec<UserId>> {
		self.teams
			.try_get_with_by_ref::<_, octocrab::Error, _>(team, async {
				let team = self.client.teams(self.name()).members(team).per_page(100).send().await?;

				let users = team.into_stream(&self.client).try_collect::<Vec<_>>().await?;
				Ok(users.into_iter().map(|u| u.id).collect())
			})
			.await
			.context("get team users")
	}

	pub fn client(&self) -> &Octocrab {
		&self.client
	}
}

pub struct RepoClient<'a> {
	repo_id: RepositoryId,
	installation: &'a InstallationClient,
}

impl<'a> RepoClient<'a> {
	fn new(repo_id: RepositoryId, installation: &'a InstallationClient) -> Self {
		Self { repo_id, installation }
	}

	pub fn get(&self) -> anyhow::Result<Repository> {
		self.installation
			.repositories
			.lock()
			.get(&self.repo_id)
			.cloned()
			.context("repository not found")
	}

	pub async fn get_pull_request(&self, number: u64) -> anyhow::Result<PullRequest> {
		let repo = self.get()?;

		let owner = self.installation.installation().account.login;

		self.installation
			.pulls
			.try_get_with::<_, octocrab::Error>((self.repo_id, number), async {
				self.installation.client.pulls(owner, repo.name).get(number).await
			})
			.await
			.context("get pull request")
	}

	pub async fn get_issue(&self, number: u64) -> anyhow::Result<Issue> {
		self.installation
			.client()
			.issues_by_id(self.repo_id)
			.get(number)
			.await
			.context("get issue")
	}

	pub async fn set_pull_request(&self, pull_request: PullRequest) {
		self.installation
			.pulls
			.insert((self.repo_id, pull_request.number), pull_request)
			.await;
	}

	pub async fn get_role_members(&self, role: Role) -> anyhow::Result<Vec<UserId>> {
		self.installation
			.team_users
			.try_get_with::<_, octocrab::Error>((self.repo_id, role), async {
				let users = self
					.installation
					.client
					.repos_by_id(self.repo_id)
					.list_collaborators()
					.permission(role.into())
					.send()
					.await?;

				Ok(users.items.into_iter().map(|c| c.author.id).collect())
			})
			.await
			.context("get role members")
	}

	pub async fn send_message(&self, issue_number: u64, message: impl AsRef<str>) -> anyhow::Result<()> {
		self.installation
			.client()
			.issues_by_id(self.repo_id)
			.create_comment(issue_number, message)
			.await
			.context("send message")?;
		Ok(())
	}

	pub async fn get_commit(&self, sha: &str) -> anyhow::Result<RepoCommit> {
		let repo = self.get()?;

		self.installation
			.client()
			.commits(repo.owner.unwrap().login, repo.name)
			.get(sha)
			.await
			.context("get commit")
	}

	pub async fn create_merge(
		&self,
		message: &str,
		tmp_branch_prefix: &str,
		base_sha: &str,
		head_sha: &str,
	) -> anyhow::Result<Commit> {
		let repo = self.get()?;

		let tmp_branch = format!("{}/{}", tmp_branch_prefix.trim_end_matches('/'), uuid::Uuid::new_v4());

		self.push_branch(&tmp_branch, base_sha, true)
			.await
			.context("push tmp branch")?;

		let commit = self
			.installation
			.client()
			.post::<_, Commit>(
				format!("/repos/{}/{}/merges", repo.owner.unwrap().login, repo.name),
				Some(&serde_json::json!({
					"base": tmp_branch,
					"head": head_sha,
					"commit_message": message,
				})),
			)
			.await
			.context("create commit");

		if let Err(e) = self.delete_branch(&tmp_branch).await {
			tracing::error!("failed to delete tmp branch: {:#}", e);
		}

		commit
	}

	pub async fn create_commit(
		&self,
		message: String,
		parents: Vec<String>,
		tree: String,
	) -> anyhow::Result<GitCommitObject> {
		self.installation
			.client()
			.repos_by_id(self.repo_id)
			.create_git_commit_object(message, tree)
			.parents(parents)
			.send()
			.await
			.context("create commit")
	}

	pub async fn push_branch(&self, branch: &str, sha: &str, force: bool) -> anyhow::Result<()> {
		let repo = self.get()?;

		let branch_ref = Reference::Branch(branch.to_owned());

		let current_ref = match self
			.installation
			.client()
			.repos_by_id(self.repo_id)
			.get_ref(&branch_ref)
			.await
		{
			Ok(r) if is_object_sha(&r.object, sha) => return Ok(()),
			Ok(r) => Some(r),
			Err(octocrab::Error::GitHub {
				source: GitHubError {
					status_code: http::StatusCode::NOT_FOUND,
					..
				},
				..
			}) => None,
			Err(e) => return Err(e).context("get ref"),
		};

		if current_ref.is_none() {
			self.installation
				.client()
				.repos_by_id(self.repo_id)
				.create_ref(&branch_ref, sha)
				.await
				.context("create ref")?;

			return Ok(());
		}

		self.installation
			.client()
			.patch::<Ref, _, _>(
				format!("/repos/{}/{}/git/refs/heads/{}", repo.owner.unwrap().login, repo.name, branch),
				Some(&serde_json::json!({
					"sha": sha,
					"force": force,
				})),
			)
			.await
			.context("update ref")?;

		Ok(())
	}

	pub async fn delete_branch(&self, branch: &str) -> anyhow::Result<()> {
		self.installation
			.client()
			.repos_by_id(self.repo_id)
			.delete_ref(&Reference::Branch(branch.to_owned()))
			.await
			.context("delete branch")
	}

	pub async fn get_commit_by_sha(&self, sha: &str) -> anyhow::Result<Option<Commit>> {
		let repo = self.get()?;

		match self
			.installation
			.client()
			.get::<Commit, _, _>(
				format!("/repos/{}/{}/commits/{}", repo.owner.unwrap().login, repo.name, sha),
				None::<&()>,
			)
			.await
		{
			Ok(commit) => Ok(Some(commit)),
			Err(octocrab::Error::GitHub {
				source: GitHubError {
					status_code: http::StatusCode::NOT_FOUND,
					..
				},
				..
			}) => Ok(None),
			Err(e) => Err(e).context("get commit by sha"),
		}
	}

	pub async fn get_ref(&self, gh_ref: &params::repos::Reference) -> anyhow::Result<Option<Ref>> {
		match self.installation.client().repos_by_id(self.repo_id).get_ref(gh_ref).await {
			Ok(r) => Ok(Some(r)),
			Err(octocrab::Error::GitHub {
				source: GitHubError {
					status_code: http::StatusCode::NOT_FOUND,
					..
				},
				..
			}) => Ok(None),
			Err(e) => Err(e).context("get ref"),
		}
	}

	pub async fn has_permission(&self, user_id: UserId, permissions: &[Permission]) -> anyhow::Result<bool> {
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
