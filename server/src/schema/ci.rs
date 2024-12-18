use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use axum::http;
use chrono::Utc;
use diesel::prelude::{Insertable, Queryable};
use diesel::query_dsl::methods::{FilterDsl, LimitDsl, OrderDsl, SelectDsl};
use diesel::{BoolExpressionMethods, ExpressionMethods, OptionalExtension, Selectable, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::pulls::PullRequest;
use octocrab::models::{RepositoryId, UserId};
use octocrab::params::repos::Reference;
use octocrab::GitHubError;

use super::ci_checks::CiCheck;
use crate::github::config::GitHubBrawlRepoConfig;
use crate::github::installation::{InstallationClient, RepoClient};
use crate::schema::enums::{GithubCiRunStatus, GithubCiRunStatusCheckStatus};
use crate::schema::pr::Pr;
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

impl InsertCiRun<'_> {
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
						commit_link(&repo_owner.login, &repo.name, self.head_commit_sha),
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

	pub async fn refresh(
		&self,
		conn: &mut AsyncPgConnection,
		client: &Arc<InstallationClient>,
		config: &GitHubBrawlRepoConfig,
		pr: &Pr<'_>,
	) -> anyhow::Result<()> {
		if self.completed_at.is_some() {
			return Ok(());
		}

		let repo_client = client
			.get_repository(RepositoryId(self.github_repo_id as u64))
			.context("get repository")?;

		let Some(started_at) = self.started_at else {
			return Ok(());
		};

		let checks = CiCheck::get_for_run(conn, self.id).await?;

		let checks = checks
			.iter()
			.map(|c| (c.status_check_name.as_ref(), c))
			.collect::<HashMap<_, _>>();

		let mut success = true;
		let mut required_checks = Vec::new();
		let mut missing_checks = Vec::new();

		for check in &config.required_status_checks {
			let Some(check) = checks.get(check.as_str()).copied() else {
				success = false;
				missing_checks.push((check.as_str(), None));
				continue;
			};

			if check.status_check_status == GithubCiRunStatusCheckStatus::Failure {
				fail_run(
					conn,
					&repo_client,
					self.id,
					self.github_pr_number,
					&format!(
						"💔 Test failed - [{check}]({check_url})",
						check = check.status_check_name,
						check_url = check.url
					),
				)
				.await?;
				return Ok(());
			} else if check.status_check_status != GithubCiRunStatusCheckStatus::Success {
				success = false;
				missing_checks.push((check.status_check_name.as_ref(), Some(check)));
			}

			required_checks.push(check);
		}

		if success {
			success_run(conn, client, self, pr, required_checks.as_ref()).await?;
		} else if Utc::now().signed_duration_since(started_at) > chrono::Duration::minutes(config.timeout_minutes as i64) {
			fail_run(
				conn,
				&repo_client,
				self.id,
				self.github_pr_number,
				&format!(
					"💔 CI run timed out after {timeout} minutes\n{missing_checks}",
					timeout = config.timeout_minutes,
					missing_checks = {
						let mut missing_checks_string = String::new();
						for (name, check) in missing_checks {
							if let Some(check) = check {
								missing_checks_string.push_str(&format!(
									"- [{name}]({url}) (pending)\n",
									name = name,
									url = check.url
								));
							} else {
								missing_checks_string.push_str(&format!("- {name} (not started)\n", name = name));
							}
						}
						missing_checks_string
					},
				),
			)
			.await?;
		}

		Ok(())
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

	pub async fn find_by_run_commit_sha(conn: &mut AsyncPgConnection, run_commit_sha: &str) -> anyhow::Result<Option<Self>> {
		crate::schema::github_ci_runs::dsl::github_ci_runs
			.select(CiRun::as_select())
			.filter(crate::schema::github_ci_runs::run_commit_sha.eq(run_commit_sha))
			.get_result::<CiRun>(conn)
			.await
			.optional()
			.context("select")
	}
}

async fn fail_run(
	conn: &mut AsyncPgConnection,
	repo_client: &RepoClient<'_>,
	run_id: i32,
	pr_number: i32,
	message: &str,
) -> anyhow::Result<()> {
	diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
		.filter(crate::schema::github_ci_runs::id.eq(run_id))
		.set((
			crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::Failure),
			crate::schema::github_ci_runs::completed_at.eq(chrono::Utc::now()),
		))
		.execute(conn)
		.await
		.context("update")?;

	repo_client
		.send_message(pr_number as u64, message)
		.await
		.context("send message")?;

	Ok(())
}

async fn success_run(
	conn: &mut AsyncPgConnection,
	client: &Arc<InstallationClient>,
	run: &CiRun,
	pr: &Pr<'_>,
	checks: &[&CiCheck<'_>],
) -> anyhow::Result<()> {
	diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
		.filter(crate::schema::github_ci_runs::id.eq(run.id))
		.set((
			crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::Success),
			crate::schema::github_ci_runs::completed_at.eq(chrono::Utc::now()),
		))
		.execute(conn)
		.await
		.context("update")?;

	let mut checks_message = String::new();
	for check in checks {
		checks_message.push_str(&format!(
			"- [{name}]({url}) (in {duration}) \n",
			name = check.status_check_name,
			url = check.url,
			duration = {
				let duration = check.completed_at.unwrap_or(chrono::Utc::now()).signed_duration_since(check.started_at);
				let seconds = duration.num_seconds() % 60;
				let minutes = (duration.num_seconds() / 60) % 60;
				let hours = duration.num_seconds() / 60 / 60;
				let mut format_string = String::new();
				if hours > 0 {
					format_string.push_str(&format!("{:0>2}:", hours));
					format_string.push_str(&format!("{:0>2}:", minutes));
					format_string.push_str(&format!("{:0>2}", seconds));
				} else if minutes > 0 {
					format_string.push_str(&format!("{:0>2}:", minutes));
					format_string.push_str(&format!("{:0>2}", seconds));
				} else {
					format_string.push_str(&format!("{:0>2}s", seconds));
				}

				format_string
			},
		));
	}

	let Some(run_commit_sha) = run.run_commit_sha.as_ref() else {
		anyhow::bail!("run commit sha is null");
	};

	let repo_client = client
		.get_repository(RepositoryId(run.github_repo_id as u64))
		.context("get repository")?;

	let repo = repo_client.get()?;
	let repo_owner = repo.owner.context("repo owner")?;

	if run.is_dry_run {
		repo_client
			.send_message(
				run.github_pr_number as u64,
				format!(
					"🎉 Try build successful!\n{checks_message}\nBuild commit: {commit_link} (`{commit_sha}`)",
					checks_message = checks_message,
					commit_link = commit_link(&repo_owner.login, &repo.name, run_commit_sha),
					commit_sha = run_commit_sha,
				),
			)
			.await
			.context("send message")?;
	} else {
		let mut reviewers = Vec::new();
		for id in &pr.reviewer_ids {
			let user = client.get_user(UserId(*id as u64)).await?;
			reviewers.push(user.login);
		}

		repo_client
			.send_message(
				run.github_pr_number as u64,
				format!(
					"🎉 Build successful!\n{checks_message}Approved by: `{reviewers}`\nPushing {commit_link} to {branch}",
					checks_message = checks_message,
					reviewers = reviewers.join(", "),
					commit_link = commit_link(&repo_owner.login, &repo.name, run_commit_sha),
					branch = pr.target_branch,
				),
			)
			.await
			.context("send message")?;

		match repo_client.push_branch(&pr.target_branch, run_commit_sha, false).await {
			Ok(_) => {}
			Err(e) => {
				fail_run(
					conn,
					&repo_client,
					run.id,
					run.github_pr_number,
					&format!(
						r#"🚨 Tests passed but failed to push to {target_branch}
<details>
<summary>Error</summary>

{error:#}

</details>
"#,
						error = e,
						target_branch = pr.target_branch,
					),
				)
				.await?;

				tracing::error!("failed to push branch {}: {:#}", pr.target_branch, e);
			}
		}
	}

	Ok(())
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
		.set((
			crate::schema::github_ci_runs::status.eq(GithubCiRunStatus::InProgress),
			crate::schema::github_ci_runs::started_at.eq(chrono::Utc::now()),
		))
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
				fail_run(
					conn,
					&repo_client,
					run_id,
					ci_run.github_pr_number,
					&format!("🚨 Failed to find base branch `{branch}`"),
				)
				.await?;
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
	let mut reviewed_by = Vec::new();
	if ci_run.is_dry_run {
		let user = client.get_user(UserId(ci_run.requested_by_id as u64)).await?;
		reviewed_by.push(format!("Reviewed-by: {login} <{id}+{login}@users.noreply.github.com>", login = user.login, id = user.id));
		reviewers.push(user.login);
	} else {
		for id in &pr.reviewer_ids {
			let user = client.get_user(UserId(*id as u64)).await?;
			reviewed_by.push(format!("Reviewed-by: {login} <{id}+{login}@users.noreply.github.com>", login = user.login, id = user.id));
			reviewers.push(user.login);
		}
	}

	let commit_message = format!(
		"Auto merge of {issue} - {branch}, r={reviewers}\n\n{title}\n{body}\n\n{reviewed_by}",
		issue = issue_link(&repo_owner.login, &repo.name, ci_run.github_pr_number as u64),
		branch = pr.source_branch,
		reviewers = reviewers.join(", "),
		title = pr.title,
		body = pr.body,
		reviewed_by = reviewed_by.join("\n"),
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
			let message = if let Some(octocrab::Error::GitHub {
				source: GitHubError {
					status_code: http::StatusCode::CONFLICT,
					..
				},
				..
			}) = e.downcast_ref::<octocrab::Error>()
			{
				format!(
					r#"🔒 Merge conflict
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
				)
			} else {
				format!(
					r#"🚨 Failed to start CI run
<details>
<summary>Error</summary>

{error:#}

</details>
"#,
					error = e
				)
			};

			fail_run(conn, &repo_client, run_id, ci_run.github_pr_number, &message).await?;
			return Ok(false);
		}
	};

	match repo_client.push_branch(&ci_run.ci_branch, &commit.sha, true).await {
		Ok(_) => {}
		Err(e) => {
			fail_run(
				conn,
				&repo_client,
				run_id,
				ci_run.github_pr_number,
				&format!(
					r#"🚨 Failed to start CI run
<details>
<summary>Error</summary>

{error:#}

</details>
"#,
					error = e
				),
			)
			.await?;

			return Ok(false);
		}
	}

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
	let Some((run_commit_sha, repo_id)) = diesel::update(crate::schema::github_ci_runs::dsl::github_ci_runs)
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
		.context("update")?
	else {
		return Ok(());
	};

	tracing::info!("cancelled ci run {run_id} for {repo_id}");

	if let Some(run_commit_sha) = run_commit_sha {
		// TODO: cancel all checks on this commit on GitHub
	}

	let _ = client;

	Ok(())
}
