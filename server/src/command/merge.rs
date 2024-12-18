use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;
use crate::schema::ci::{Base, CiRun, Head, InsertCiRun};
use crate::schema::pr::{Pr, UpdatePr};

#[derive(Debug)]
pub struct MergeCommand {
	pub reviewers: Vec<String>,
	pub priority: Option<i32>,
}

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
	command: MergeCommand,
) -> anyhow::Result<()> {
	if !context.config.enabled {
		return Ok(());
	}

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	if !repo_client
		.has_permission(context.user.id, &context.config.merge_permissions)
		.await?
	{
		tracing::debug!("user does not have permission to do this");
		return Ok(());
	}

	if context.pr.merged_at.is_some() {
		tracing::debug!("pull request already merged");
		return Ok(());
	}

	let mut current = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;
	let mut update = UpdatePr::new(&context.pr, &mut current);

	if let Some(priority) = command.priority {
		update.default_priority = Some(priority);
		current.default_priority = Some(priority);
	}

	let mut provided_reviewers = Vec::new();
	if !command.reviewers.is_empty() {
		for reviewer in command.reviewers {
			let user = client.get_user_by_name(&reviewer).await?;
			provided_reviewers.push(user.id.0 as i64);
		}

		provided_reviewers.sort();
	}

	// If they didnt provide a list then whoever is issuing the command is
	// approving.
	if provided_reviewers.is_empty() {
		provided_reviewers.push(context.user.id.0 as i64);
	}

	if provided_reviewers != current.reviewer_ids {
		update.reviewer_ids = Some(provided_reviewers.clone());
		current.reviewer_ids = provided_reviewers;
	}

	if let Some(run) = CiRun::get_active(conn, context.repo_id, context.pr.number as i64).await? {
		run.cancel(conn, client).await?;
	}

	// We should now start a CI Run for this PR.
	InsertCiRun {
		github_repo_id: context.repo_id.0 as i64,
		github_pr_number: context.issue_number as i32,
		base_ref: &Base::from_pr(&context.pr).to_string(),
		head_commit_sha: Head::from_pr(&context.pr).sha(),
		run_commit_sha: None,
		ci_branch: &format!(
			"{}/{}",
			context.config.merge_branch_prefix.trim_end_matches('/'),
			context.pr.base.ref_field
		),
		priority: command.priority.unwrap_or(current.default_priority.unwrap_or(5)),
		requested_by_id: context.user.id.0 as i64,
		is_dry_run: false,
	}
	.insert(conn, client, &context.config, &current)
	.await?;

	update.do_update(conn).await?;

	Ok(())
}
