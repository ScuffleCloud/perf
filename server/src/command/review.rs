use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::ci::{Base, CiRun, Head, InsertCiRun};
use crate::github::installation::InstallationClient;
use crate::pr::{Pr, UpdatePr};

#[derive(Debug)]
pub struct ReviewCommand {
	pub action: ReviewAction,
	pub reviewers: Vec<String>,
	pub priority: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
	Approve,
	Unapprove,
}

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
	command: ReviewCommand,
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

	let current = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;
	let mut update = UpdatePr::new(&context.pr, &current);

	if let Some(priority) = command.priority {
		update.default_priority = Some(priority);
	}

	let mut provided_reviewers = Vec::new();
	if !command.reviewers.is_empty() {
		for reviewer in command.reviewers {
			let user = client.get_user_by_name(&reviewer).await?;
			provided_reviewers.push(user.id.0 as i64);
		}

		provided_reviewers.sort();
	}

	// Try figure out what changed
	if command.action == ReviewAction::Approve {
		// Create a new CI run somehow...

		// If they didnt provide a list then whoever is issuing the command is
		// approving.
		if provided_reviewers.is_empty() {
			provided_reviewers.push(context.user.id.0 as i64);
		}

		if provided_reviewers != current.reviewer_ids {
			update.reviewer_ids = Some(provided_reviewers);
		}

		if let Some(run) = CiRun::get_active(conn, context.repo_id, context.pr.number as i64).await? {
			run.cancel(conn, client).await?;
		}

		// We should now start a CI Run for this PR.
		InsertCiRun {
			github_repo_id: context.repo_id.0 as i64,
			github_pr_number: context.issue_number as i32,
			base_ref: &Base::from_pr(&context.pr).to_string(),
			head_commit_sha: &Head::from_pr(&context.pr).sha(),
			run_commit_sha: None,
			ci_branch: &context.config.temp_branch_prefix,
			priority: command.priority.unwrap_or(current.default_priority.unwrap_or(5)),
			requested_by_id: context.user.id.0 as i64,
			is_dry_run: false,
		}
		.insert(conn, client, &context.config, &provided_reviewers)
		.await?;
	} else if !current.reviewer_ids.is_empty() {
		let mut new_ids = Vec::new();

		// If they provided a list of reviewers, then we should remove only those
		// provided. Otherwise, we should remove all reviewers.
		if !provided_reviewers.is_empty() {
			// Make sure none of the provided reviewers are in the list of current
			// reviewers.
			for reviewer in &current.reviewer_ids {
				if !provided_reviewers.contains(reviewer) {
					new_ids.push(*reviewer);
				}
			}
		}

		// If the list of reviewers changed, then we should update the DB.
		if current.reviewer_ids != new_ids {
			// If the list is now empty, & there is a CI run, then we should cancel the CI
			// run.
			if new_ids.is_empty() {
				if let Some(run) = CiRun::get_active(conn, context.repo_id, context.pr.number as i64).await? {
					if !run.is_dry_run {
						run.cancel(conn, client).await?;
					}
				}
			}

			update.reviewer_ids = Some(new_ids);
		}
	}

	update.do_update(conn).await?;

	Ok(())
}
