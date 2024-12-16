use std::borrow::Cow;

use anyhow::Context;
use diesel::prelude::{AsChangeset, Insertable, Queryable};
use diesel::query_dsl::methods::{FindDsl, SelectDsl};
use diesel::{OptionalExtension, Selectable, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::pulls::{MergeableState, PullRequest};
use octocrab::models::{IssueState, RepositoryId, UserId};

use crate::schema_enums::{GithubPrMergeStatus, GithubPrStatus};

#[derive(Insertable, Selectable, Queryable)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[diesel(table_name = crate::schema::github_pr)]
#[diesel(primary_key(github_repo_id, github_pr_number))]
pub struct PrMergeQueue<'a> {
	pub github_repo_id: i64,
	pub github_pr_number: i32,
	pub title: Cow<'a, str>,
	pub body: Cow<'a, str>,
	pub merge_status: GithubPrMergeStatus,
	pub author_id: i64,
	pub reviewer_ids: Vec<i64>,
	pub assigned_ids: Vec<i64>,
	pub status: GithubPrStatus,
	pub default_priority: Option<i32>,
	pub merge_ci_run_id: Option<i32>,
	pub merge_commit_sha: Option<Cow<'a, str>>,
	pub target_branch: Cow<'a, str>,
	pub source_branch: Cow<'a, str>,
	pub latest_commit_sha: Cow<'a, str>,
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl<'a> PrMergeQueue<'a> {
	pub fn new(pr: &'a PullRequest, repo_id: RepositoryId, user_id: UserId) -> Self {
		Self {
			github_repo_id: repo_id.0 as i64,
			github_pr_number: pr.number as i32,
			title: Cow::Borrowed(&pr.title.as_deref().unwrap_or("")),
			body: Cow::Borrowed(&pr.body.as_deref().unwrap_or("")),
			merge_status: match pr.mergeable_state {
				_ if pr.merged_at.is_some() => GithubPrMergeStatus::Merged,
				Some(MergeableState::Behind | MergeableState::Blocked | MergeableState::Clean) => GithubPrMergeStatus::Ready,
				Some(MergeableState::Dirty) => GithubPrMergeStatus::Conflict,
				Some(MergeableState::Unstable) => GithubPrMergeStatus::CheckFailure,
				Some(MergeableState::Unknown) => GithubPrMergeStatus::NotReady,
				Some(MergeableState::Draft) => GithubPrMergeStatus::NotReady,
				_ => GithubPrMergeStatus::Ready,
			},
			author_id: user_id.0 as i64,
			reviewer_ids: vec![],
			assigned_ids: {
				let mut ids = pr
					.assignees
					.iter()
					.flatten()
					.map(|assignee| assignee.id.0 as i64)
					.collect::<Vec<_>>();
				ids.sort();
				ids
			},
			status: match pr.state {
				Some(IssueState::Open) if matches!(pr.draft, Some(true)) => GithubPrStatus::Draft,
				Some(IssueState::Open) => GithubPrStatus::Open,
				Some(IssueState::Closed) => GithubPrStatus::Closed,
				_ => GithubPrStatus::Open,
			},
			default_priority: None,
			merge_ci_run_id: None,
			merge_commit_sha: pr.merge_commit_sha.as_deref().map(|s| Cow::Borrowed(s)),
			target_branch: Cow::Borrowed(&pr.base.ref_field),
			source_branch: Cow::Borrowed(&pr.head.ref_field),
			latest_commit_sha: Cow::Borrowed(&pr.head.sha),
			created_at: chrono::Utc::now(),
			updated_at: chrono::Utc::now(),
		}
	}

	pub async fn fetch(
		repo_id: RepositoryId,
		user_id: UserId,
		pr: &'a PullRequest,
		conn: &mut AsyncPgConnection,
	) -> anyhow::Result<Self> {
		let current: Option<PrMergeQueue<'static>> = crate::schema::github_pr::table
			.find((repo_id.0 as i64, pr.number as i32))
			.select(PrMergeQueue::as_select())
			.first(conn)
			.await
			.optional()
			.context("select")?;

		let current = if let Some(current) = current {
			current
		} else {
			let insert = PrMergeQueue::new(pr, repo_id, user_id);
			diesel::insert_into(crate::schema::github_pr::dsl::github_pr)
				.values(&insert)
				.execute(conn)
				.await
				.context("upsert github pr")?;

			insert
		};

		Ok(current)
	}
}

#[derive(AsChangeset)]
#[diesel(table_name = crate::schema::github_pr)]
#[diesel(primary_key(github_repo_id, github_pr_number))]
pub struct UpdatePrMergeQueue<'a> {
	#[allow(unused)]
	pub github_repo_id: i64,
	#[allow(unused)]
	pub github_pr_number: i32,
	pub title: Option<Cow<'a, str>>,
	pub body: Option<Cow<'a, str>>,
	pub merge_status: Option<GithubPrMergeStatus>,
	pub assigned_ids: Option<Vec<i64>>,
	pub reviewer_ids: Option<Vec<i64>>,
	pub default_priority: Option<i32>,
	pub status: Option<GithubPrStatus>,
	pub target_branch: Option<Cow<'a, str>>,
	pub latest_commit_sha: Option<Cow<'a, str>>,
	pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl<'a> UpdatePrMergeQueue<'a> {
	pub fn new(pr: &'a PullRequest, current: &PrMergeQueue<'_>) -> Self {
		let mut update = Self {
			github_repo_id: current.github_repo_id,
			github_pr_number: current.github_pr_number,
			merge_status: None,
			status: None,
			title: None,
			body: None,
			assigned_ids: None,
			reviewer_ids: None,
			default_priority: None,
			target_branch: None,
			latest_commit_sha: None,
			updated_at: chrono::Utc::now(),
		};

		if pr.title.as_deref().unwrap_or("") != current.title {
			update.title = Some(Cow::Borrowed(pr.title.as_deref().unwrap_or("")));
		}

		if pr.body.as_deref().unwrap_or("") != current.body {
			update.body = Some(Cow::Borrowed(pr.body.as_deref().unwrap_or("")));
		}

		if pr.base.ref_field != current.target_branch {
			update.target_branch = Some(Cow::Borrowed(&pr.base.ref_field));
		}

		if pr.head.sha != current.latest_commit_sha {
			update.latest_commit_sha = Some(Cow::Borrowed(&pr.head.sha));
		}

		let desired_status = match pr.mergeable_state {
			_ if pr.merged_at.is_some() => GithubPrMergeStatus::Merged,
			Some(MergeableState::Behind | MergeableState::Clean) => GithubPrMergeStatus::Ready,
			Some(MergeableState::Unstable | MergeableState::Blocked) => GithubPrMergeStatus::CheckFailure,
			Some(MergeableState::Dirty) => GithubPrMergeStatus::Conflict,
			Some(MergeableState::Unknown) => GithubPrMergeStatus::NotReady,
			Some(MergeableState::Draft) => GithubPrMergeStatus::NotReady,
			_ => GithubPrMergeStatus::Ready,
		};

		// If the status changed we need to update it in the DB.
		// Unless the status has become ready and the commit has not changed then we
		// should keep the current status.
		if desired_status != current.merge_status
			&& (desired_status != GithubPrMergeStatus::Ready || current.latest_commit_sha != pr.head.sha)
		{
			update.merge_status = Some(desired_status);
		}

		let mut assigned_ids = pr
			.assignees
			.iter()
			.flatten()
			.map(|assignee| assignee.id.0 as i64)
			.collect::<Vec<_>>();
		assigned_ids.sort();

		if assigned_ids != current.assigned_ids {
			update.assigned_ids = Some(assigned_ids);
		}

		update
	}

	pub fn changed(&self) -> bool {
		self.title.is_some()
			|| self.body.is_some()
			|| self.merge_status.is_some()
			|| self.assigned_ids.is_some()
			|| self.reviewer_ids.is_some()
			|| self.status.is_some()
			|| self.target_branch.is_some()
			|| self.latest_commit_sha.is_some()
			|| self.default_priority.is_some()
	}

	pub async fn do_update(self, conn: &mut AsyncPgConnection) -> anyhow::Result<()> {
		if !self.changed() {
			return Ok(());
		}

		diesel::update(crate::schema::github_pr::dsl::github_pr)
			.set(&self)
			.execute(conn)
			.await
			.context("update")?;

		Ok(())
	}
}

pub fn commit_link(owner: &str, repo: &str, sha: &str) -> String {
	format!("https://github.com/{}/{}/commit/{}", owner, repo, sha)
}
