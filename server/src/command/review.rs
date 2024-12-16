use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::utils::{PrMergeQueue, UpdatePrMergeQueue};
use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;

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
	if !context.config.queue.enabled {
		return Ok(());
	}

	let Some(pr) = context.pr else {
		tracing::debug!("pull request missing");
		return Ok(());
	};

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	// Check if the user has permission to do this. (only merge)
	let mut has_permission = false;
	for permission in &context.config.queue.merge_permissions {
		if repo_client.has_permission(context.user.id, permission).await? {
			has_permission = true;
			break;
		}
	}

	if !has_permission {
		tracing::debug!("user does not have permission to do this");
		return Ok(());
	}

	if pr.merged_at.is_some() {
		tracing::debug!("pull request already merged");
		return Ok(());
	}

	let current = PrMergeQueue::fetch(context.repo_id, context.user.id, &pr, conn).await?;
	let mut update = UpdatePrMergeQueue::new(&pr, &current);

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

		// We should now start a CI Run for this PR.
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
				if let Some(current_run_id) = current.merge_ci_run_id {
					// Cancel the CI run somehow...
				}
			}

			update.reviewer_ids = Some(new_ids);
		}
	}

	update.do_update(conn).await?;

	Ok(())
}
