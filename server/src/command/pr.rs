use std::sync::Arc;

use anyhow::Context;
use diesel_async::{AsyncConnection, AsyncPgConnection};

use super::BrawlCommandContext;
use crate::ci::CiRun;
use crate::github::installation::InstallationClient;
use crate::pr::{Pr, UpdatePr};
use crate::schema_enums::GithubCiRunStatus;

#[derive(Debug)]
pub enum PullRequestCommand {
	Opened,
	Push,
	IntoDraft,
	ReadyForReview,
	Closed,
}

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
	_: PullRequestCommand,
) -> anyhow::Result<()> {
	// Try select the PR in the database first

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	conn.transaction(|conn| {
		Box::pin(async move {
			let mut current = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;

			// Try figure out what changed
			UpdatePr::new(&context.pr, &mut current).do_update(conn).await?;

			if context.pr.merged_at.is_none() {
				// We need to cancel the checks on the current run somehow...
				if let Some(run) = CiRun::get_active(conn, context.repo_id, context.pr.number as i64).await? {
					if !run.is_dry_run {
						run.cancel(conn, client).await?;
						repo_client
							.send_message(
								context.issue_number,
								&format!(
									"🚨 PR state was changed while merge was {}, cancelling merge.",
									match run.status {
										GithubCiRunStatus::Queued => "queued",
										GithubCiRunStatus::Pending | GithubCiRunStatus::Running => "in progress",
										_ => anyhow::bail!("impossible CI status: {:?}", run.status),
									}
								),
							)
							.await?;
					}
				}
			}

			Ok(())
		})
	})
	.await
}
