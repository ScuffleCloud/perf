use std::borrow::Cow;
use std::sync::Arc;

use anyhow::Context;
use axum::http;
use diesel::prelude::{Insertable, Queryable};
use diesel::query_dsl::methods::{FilterDsl, LimitDsl, OrderDsl, SelectDsl};
use diesel::{BoolExpressionMethods, ExpressionMethods, OptionalExtension, Selectable, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::pulls::PullRequest;
use octocrab::models::{RepositoryId, UserId};
use octocrab::params::repos::Reference;
use octocrab::GitHubError;

use crate::github::config::GitHubBrawlRepoConfig;
use crate::github::installation::InstallationClient;
use crate::pr::Pr;
use crate::schema_enums::GithubCiRunStatus;
use crate::utils::{commit_link, issue_link};

#[derive(Debug, Clone)]
pub struct Head<'a> {
	sha: Cow<'a, str>,
}

impl<'a> Head<'a> {
	pub fn from_sha(sha: &'a str) -> Self {
		Self { sha: Cow::Borrowed(sha) }
	}

	pub fn from_pr(pr: &'a PullRequest) -> Self {
		Self {
			sha: Cow::Borrowed(&pr.head.sha),
		}
	}

	pub fn sha(&self) -> &str {
		&self.sha
	}
}

#[derive(Debug, Clone)]
pub enum Base<'a> {
	Commit(Cow<'a, str>),
	Branch(Cow<'a, str>),
}

impl<'a> Base<'a> {
	pub fn from_pr(pr: &'a PullRequest) -> Self {
		Self::Branch(Cow::Borrowed(pr.base.ref_field.as_str()))
	}

	pub const fn from_sha(sha: &'a str) -> Self {
		Self::Commit(Cow::Borrowed(sha))
	}

	pub fn from_string(s: &'a str) -> Option<Self> {
		if let Some(sha) = s.strip_prefix("commit:") {
			return Some(Self::Commit(Cow::Borrowed(sha)));
		}

		if let Some(branch) = s.strip_prefix("branch:") {
			return Some(Self::Branch(Cow::Borrowed(branch)));
		}

		None
	}
}

impl std::fmt::Display for Base<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Base::Commit(sha) => write!(f, "commit:{sha}"),
			Base::Branch(branch) => write!(f, "branch:{branch}"),
		}
	}
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::github_ci_runs)]
pub struct InsertCiRun<'a> {
	pub github_repo_id: i64,
	pub github_pr_number: i32,
	pub base_ref: &'a str,
	pub head_commit_sha: &'a str,
	pub run_commit_sha: Option<&'a str>,
	pub ci_branch: &'a str,
	pub priority: i32,
	pub requested_by_id: i64,
	pub is_dry_run: bool,
}

impl<'a> InsertCiRun<'a> {
	pub async fn insert(
		self,
		conn: &mut AsyncPgConnection,
		client: &Arc<InstallationClient>,
		config: &GitHubBrawlRepoConfig,
		pr: &Pr<'_>,
	) -> anyhow::Result<i32> {
		let run_id = diesel::insert_into(crate::schema::github_ci_runs::dsl::github_ci_runs)
			.values(&self)
			.returning(crate::schema::github_ci_runs::id)
			.get_result(conn)
			.await
			.context("insert")?;

		if self.is_dry_run {
			start_ci_run(conn, run_id, client, config, pr).await?;
		} else {
			let repo_client = client
				.get_repository(RepositoryId(self.github_repo_id as u64))
				.context("get repository")?;

			let repo = repo_client.get()?;
			let repo_owner = repo.owner.context("repo owner")?;

			let mut reviewers = Vec::new();
			for id in &pr.reviewer_ids {
				let user = client.get_user(UserId(*id as u64)).await?;
				reviewers.push(user.login);
			}

			repo_client
				.send_message(
					self.github_pr_number as u64,
					&format!(
						"📌 Commit {} has been approved by `{reviewers}`, added to the merge queue.",
						commit_link(&repo_owner.login, &repo.name, &self.head_commit_sha),
						reviewers = reviewers.join(", ")
					),
				)
				.await?;
		}

		Ok(run_id)
	}
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::github_ci_runs)]
pub struct CiRun {
	pub id: i32,
	pub github_repo_id: i64,
	pub github_pr_number: i32,
	pub status: GithubCiRunStatus,
	pub base_ref: String,
	pub head_commit_sha: String,
	pub run_commit_sha: Option<String>,
	pub ci_branch: String,
	pub priority: i32,
	pub requested_by_id: i64,
	pub is_dry_run: bool,
	pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
	pub started_at: Option<chrono::DateTime<chrono::Utc>>,
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl CiRun {
	pub async fn start(
		&self,
		conn: &mut AsyncPgConnection,
		client: &Arc<InstallationClient>,
		config: &GitHubBrawlRepoConfig,
		pr: &Pr<'_>,
	) -> anyhow::Result<bool> {
		start_ci_run(conn, self.id, client, config, pr).await
	}

	pub async fn cancel(&self, conn: &mut AsyncPgConnection, client: &Arc<InstallationClient>) -> anyhow::Result<()> {
		cancel_ci_run(conn, self.id, client).await
	}

	pub async fn get_active(
		conn: &mut AsyncPgConnection,
		repo_id: RepositoryId,
		pr_number: i64,
	) -> anyhow::Result<Option<Self>> {
		crate::schema::github_ci_runs::dsl::github_ci_runs
			.select(CiRun::as_select())
			.filter(
				crate::schema::github_ci_runs::github_repo_id
					.eq(repo_id.0 as i64)
					.and(crate::schema::github_ci_runs::github_pr_number.eq(pr_number as i32))
					.and(crate::schema::github_ci_runs::completed_at.is_null()),
			)
			.get_result::<CiRun>(conn)
			.await
			.optional()
			.context("select")
	}

	pub async fn get_latest(
		conn: &mut AsyncPgConnection,
		repo_id: RepositoryId,
		pr_number: i64,
	) -> anyhow::Result<Option<Self>> {
		crate::schema::github_ci_runs::dsl::github_ci_runs
			.select(CiRun::as_select())
			.filter(
				crate::schema::github_ci_runs::github_repo_id
					.eq(repo_id.0 as i64)
					.and(crate::schema::github_ci_runs::github_pr_number.eq(pr_number as i32)),
			)
			.order(crate::schema::github_ci_runs::created_at.desc())
			.limit(1)
			.get_result::<CiRun>(conn)
			.await
			.optional()
			.context("select")
	}
}

pub async fn start_ci_run(
	conn: &mut AsyncPgConnection,
	run_id: i32,
	client: &Arc<InstallationClient>,
	config: &GitHubBrawlRepoConfig,
	pr: &Pr<'_>,
) -> anyhow::Result<bool> {
	#[derive(Queryable, Selectable)]
	#[diesel(table_name = crate::schema::github_ci_runs)]
	struct UpdateResult {
		github_repo_id: i64,
		github_pr_number: i32,
		base_ref: String,
		head_commit_sha: String,
		ci_branch: String,
		requested_by_id: i64,
		is_dry_run: bool,
	}

	let update = diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
		.filter(
			crate::schema::github_ci_runs::id
				.eq(run_id)
				.and(crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::Queued)),
		)
		.set(crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::Pending))
		.returning(UpdateResult::as_select())
		.get_result::<UpdateResult>(conn)
		.await
		.optional()
		.context("update")?;

	let Some(ci_run) = update else {
		return Ok(false);
	};

	let repo_client = client
		.get_repository(RepositoryId(ci_run.github_repo_id as u64))
		.context("get repository")?;

	let repo = repo_client.get()?;
	let repo_owner = repo.owner.context("repo owner")?;

	let base_sha = match Base::from_string(&ci_run.base_ref) {
		Some(Base::Commit(sha)) => sha,
		Some(Base::Branch(branch)) => {
			let Some(branch) = repo_client.get_ref(&Reference::Branch(branch.to_string())).await? else {
				diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
					.filter(crate::schema::github_ci_runs::id.eq(run_id))
					.set(crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::Failure))
					.execute(conn)
					.await
					.context("update")?;

				repo_client
					.send_message(
						ci_run.github_pr_number as u64,
						format!("🚨 Failed to find base branch `{branch}`"),
					)
					.await
					.context("send message")?;

				return Ok(false);
			};

			match branch.object {
				octocrab::models::repos::Object::Commit { sha, .. } => Cow::Owned(sha),
				octocrab::models::repos::Object::Tag { sha, .. } => Cow::Owned(sha),
				_ => anyhow::bail!("invalid base"),
			}
		}
		None => anyhow::bail!("invalid base"),
	};

	let mut reviewers = Vec::new();
	if ci_run.is_dry_run {
		let user = client.get_user(UserId(ci_run.requested_by_id as u64)).await?;
		reviewers.push(user.login);
	} else {
		for id in &pr.reviewer_ids {
			let user = client.get_user(UserId(*id as u64)).await?;
			reviewers.push(user.login);
		}
	}

	let commit_message = format!(
		"Auto merge of {issue} - {branch}, r={reviewers}\n\n{title}\n{body}",
		issue = issue_link(&repo_owner.login, &repo.name, ci_run.github_pr_number as u64),
		branch = pr.source_branch,
		reviewers = reviewers.join(", "),
		title = pr.title,
		body = pr.body,
	);

	let commit = match repo_client
		.create_merge(
			&commit_message,
			&config.temp_branch_prefix,
			&base_sha,
			&ci_run.head_commit_sha,
		)
		.await
	{
		Ok(commit) => commit,
		Err(e) => {
			if let Some(octocrab::Error::GitHub {
				source: GitHubError {
					status_code: http::StatusCode::CONFLICT,
					..
				},
				..
			}) = e.downcast_ref::<octocrab::Error>()
			{
				repo_client
							.send_message(
								ci_run.github_pr_number as u64,
								format!(r#"🔒 Merge conflict
This pull request and the `{target_branch}` branch have diverged in a way that cannot be automatically merged.
Please rebase your branch ontop of the latest `{target_branch}` branch and let the reviewer approve again.

Attempted merge from {head_sha} into {base_sha}

<details><summary>How do I rebase?</summary>

1. `git checkout {source_branch}` *(Switch to your branch)*
2. `git fetch upstream {target_branch}` *(Fetch the latest changes from the upstream)*
3. `git rebase upstream/{target_branch} -p` *(Rebase your branch onto the upstream branch)*
4. Follow the prompts to resolve any conflicts (use `git status` if you get lost).
5. `git push self {source_branch} --force-with-lease` *(Update this PR)*`

You may also read
 [*Git Rebasing to Resolve Conflicts* by Drew Blessing](http://blessing.io/git/git-rebase/open-source/2015/08/23/git-rebasing-to-resolve-conflicts.html)
 for a short tutorial.

Please avoid the ["**Resolve conflicts**" button](https://help.github.com/articles/resolving-a-merge-conflict-on-github/) on GitHub.
 It uses `git merge` instead of `git rebase` which makes the PR commit history more difficult to read.

Sometimes step 4 will complete without asking for resolution. This is usually due to difference between how `Cargo.lock` conflict is
handled during merge and rebase. This is normal, and you should still perform step 5 to update this PR.

</details>
"#,
								head_sha = commit_link(&repo_owner.login, &repo.name, &ci_run.head_commit_sha),
								base_sha = commit_link(&repo_owner.login, &repo.name, &base_sha),
								source_branch = pr.source_branch,
								target_branch = pr.target_branch,
							))
							.await
							.context("send message")?;

				return Ok(false);
			}

			repo_client
				.send_message(
					ci_run.github_pr_number as u64,
					format!(
						r#"🚨 Failed to start CI run
<details>
<summary>Error</summary>

{error:#}

</details>
"#,
						error = e
					),
				)
				.await
				.context("send message")?;

			return Ok(false);
		}
	};

	repo_client
		.push_branch(&ci_run.ci_branch, &commit.sha, true)
		.await
		.context("push branch")?;

	diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
		.filter(crate::schema::github_ci_runs::id.eq(run_id))
		.set(crate::schema::github_ci_runs::run_commit_sha.eq(&commit.sha))
		.execute(conn)
		.await
		.context("update")?;

	repo_client
		.send_message(
			ci_run.github_pr_number as u64,
			format!(
				"⌛ Trying commit {} with merge {}...",
				commit_link(&repo_owner.login, &repo.name, &ci_run.head_commit_sha),
				commit_link(&repo_owner.login, &repo.name, &commit.sha)
			),
		)
		.await?;

	Ok(true)
}

pub async fn cancel_ci_run(
	conn: &mut AsyncPgConnection,
	run_id: i32,
	client: &Arc<InstallationClient>,
) -> anyhow::Result<()> {
	let response = diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
		.filter(
			crate::schema::github_ci_runs::id
				.eq(run_id)
				.and(crate::schema::github_ci_runs::completed_at.is_null()),
		)
		.set((
			crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::Cancelled),
			crate::schema::github_ci_runs::completed_at.eq(chrono::Utc::now()),
		))
		.returning((
			crate::schema::github_ci_runs::run_commit_sha,
			crate::schema::github_ci_runs::github_repo_id,
		))
		.get_result::<(Option<String>, i64)>(conn)
		.await
		.optional()
		.context("update")?;

	if let Some((Some(run_commit_sha), repo_id)) = response {
		// TODO: cancel all checks on this commit on GitHub
		tracing::info!("cancelled ci run {run_id} for {repo_id} on {run_commit_sha}");
		let _ = client;
	}

	Ok(())
}
