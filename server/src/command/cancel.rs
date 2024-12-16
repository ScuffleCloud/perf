use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
) -> anyhow::Result<()> {
	if !context.config.queue.enabled {
		return Ok(());
	}

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	let issue = repo_client.get_issue(context.issue_number).await?;

	// Check if the user has permission to do this. (either merge or try)
	if context.user.id != issue.user.id {
		let mut has_permission = false;
		for permission in context
			.config
			.queue
			.merge_permissions
			.iter()
			.chain(context.config.queue.try_permissions.iter().flatten())
		{
			if repo_client.has_permission(context.user.id, permission).await? {
				has_permission = true;
				break;
			}
		}

		if !has_permission {
			tracing::debug!("user does not have permission to do this");
			return Ok(());
		}
	}

	// Check if there is anything running for this PR & cancel it

	Ok(())
}
