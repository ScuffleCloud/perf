use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use diesel_async::pooled_connection::bb8;
use diesel_async::AsyncPgConnection;
use octocrab::models::RepositoryId;
use serde::Deserialize;

use crate::github::config::GitHubBrawlRepoConfig;
use crate::github::installation::InstallationClient;
use crate::schema::ci::CiRun;
use crate::schema::ci_checks::CiCheck;
use crate::schema::pr::Pr;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
	Queued,
	InProgress,
	Completed,
	Waiting,
	Requested,
	Pending,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunConclusion {
	Success,
	Failure,
	Neutral,
	Cancelled,
	Skipped,
	TimedOut,
	ActionRequired,
}

#[derive(Debug, Clone, Deserialize)]
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

pub async fn handle(
	client: &Arc<InstallationClient>,
	database_pool: &bb8::Pool<AsyncPgConnection>,
	repo_id: RepositoryId,
	config: &GitHubBrawlRepoConfig,
	check_run_event: serde_json::Value,
) -> anyhow::Result<()> {
	let check_run_event: CheckRunEvent = serde_json::from_value(check_run_event).context("deserialize check run event")?;

	let mut conn = database_pool.get().await.context("get database connection")?;

	let Some(run) = CiRun::find_by_run_commit_sha(&mut conn, &check_run_event.head_sha).await? else {
		return Ok(());
	};

	// If the run is not still running, we don't need to do anything
	if run.completed_at.is_some() {
		return Ok(());
	}

	let required = config
		.required_status_checks
		.iter()
		.any(|check| check.eq_ignore_ascii_case(&check_run_event.name));

	let ci_check = CiCheck::new(&check_run_event, run.id, required);
	ci_check.upsert(&mut conn).await.context("upsert ci check")?;

	let Some(pr) = Pr::fetch(&mut conn, repo_id, run.github_pr_number as i64).await? else {
		anyhow::bail!("pr not found");
	};

	run.refresh(&mut conn, client, config, &pr).await?;

	Ok(())
}
