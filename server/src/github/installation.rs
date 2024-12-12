use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use moka::future::Cache;
use octocrab::models::pulls::PullRequest;
use octocrab::models::{Installation, InstallationRepositories, Repository, RepositoryId, UserId, UserProfile};
use octocrab::Octocrab;
use parking_lot::Mutex;

#[derive(Debug)]
pub struct InstallationClient {
	client: Octocrab,
	installation: Mutex<Installation>,
	repositories: Mutex<HashMap<RepositoryId, Repository>>,
	repositories_by_name: Mutex<HashMap<String, RepositoryId>>,

	pulls: Cache<(RepositoryId, u64), PullRequest>,

	// This is a cache of user profiles that we load due to this installation.
	users: Cache<UserId, UserProfile>,
	users_by_name: Cache<String, UserId>,
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
			.try_get_with::<_, octocrab::Error>(name.to_lowercase(), async {
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
		self.set_repository(repo);
		Ok(())
	}

	pub fn set_repository(&self, repo: Repository) {
		tracing::info!("added repository: {}/{}", repo.owner.as_ref().unwrap().login, repo.name);
		self.repositories_by_name.lock().insert(repo.name.to_lowercase(), repo.id);
		self.repositories.lock().insert(repo.id, repo);
	}

	pub fn remove_repository(&self, repo_id: RepositoryId) {
		let repo = self.repositories.lock().remove(&repo_id);
		if let Some(repo) = repo {
			self.repositories_by_name.lock().remove(&repo.name.to_lowercase());
			tracing::info!("removed repository: {}/{}", repo.owner.unwrap().login, repo.name);
		}
	}

	pub fn installation(&self) -> Installation {
		self.installation.lock().clone()
	}

	pub fn update_installation(&self, installation: Installation) {
		tracing::info!("updated installation: {} ({})", installation.account.login, installation.id);
		*self.installation.lock() = installation;
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

	pub async fn set_pull_request(&self, pull_request: PullRequest) {
		self.installation
			.pulls
			.insert((self.repo_id, pull_request.number), pull_request)
			.await;
	}
}
