use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;

pub async fn handle(
	client: &Arc<InstallationClient>,
	_: &mut AsyncPgConnection,
	context: BrawlCommandContext,
) -> anyhow::Result<()> {
	// I think we should have some info like the time it took to respond.
	// The user name, & their permissions.
	let status = if context.config.enabled {
		if context.config.queue.enabled {
			"enabled with queue"
		} else {
			"enabled without queue"
		}
	} else {
		"disabled"
	};

	// Should we also say what permissions the user has?
	let message = format!(
		"Pong, @{user_name}!\nBrawl is currently {status} on this repo.",
		user_name = context.user.login
	);

	client
		.get_repository(context.repo_id)
		.context("get repository")?
		.send_message(context.issue_number, &message)
		.await
		.context("send message")?;

	Ok(())
}
