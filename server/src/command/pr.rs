use std::sync::Arc;

use diesel_async::AsyncPgConnection;

use super::utils::{PrMergeQueue, UpdatePrMergeQueue};
use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;

#[derive(Debug)]
pub enum PullRequestCommand {
	Opened,
	Push,
	IntoDraft,
	ReadyForReview,
	Closed,
}

pub async fn handle(
	_: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
	_: PullRequestCommand,
) -> anyhow::Result<()> {
	let Some(pr) = &context.pr else {
		anyhow::bail!("pull request command missing pull request");
	};

	let user_id = pr.user.as_ref().map(|user| user.id).unwrap_or(context.user.id);

	// Try select the PR in the database first
	let current = PrMergeQueue::fetch(context.repo_id, user_id, pr, conn).await?;

	// Try figure out what changed
	UpdatePrMergeQueue::new(pr, &current).do_update(conn).await?;

	if let Some(Some(current_run_id)) = pr.merged_at.is_none().then_some(current.merge_ci_run_id) {
		// We need to cancel the checks on the current run somehow...
	}

	if !context.config.queue.enabled {
		return Ok(());
	}

	Ok(())
}
