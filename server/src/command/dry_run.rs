use std::sync::Arc;

use anyhow::Context;
use diesel_async::{AsyncConnection, AsyncPgConnection};

use super::BrawlCommandContext;
use crate::ci::{cancel_ci_run, create_ci_run, get_active_ci_run, start_ci_run, Base, Head};
use crate::github::installation::InstallationClient;
use crate::pr::{Pr, UpdatePr};
use crate::schema_enums::GithubCiRunStatus;

#[derive(Debug)]
pub enum DryRunCommand {
	New {
		head_sha: Option<String>,
		base_sha: Option<String>,
	},
	Cancel,
}

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
	command: DryRunCommand,
) -> anyhow::Result<()> {
	if !context.config.enabled {
		return Ok(());
	}

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	if !repo_client
		.has_permission(context.user.id, context.config.try_permissions())
		.await?
	{
		tracing::debug!("user does not have permission to do this");
		return Ok(());
	}

	match command {
		DryRunCommand::New {
			mut head_sha,
			mut base_sha,
		} => {
			if let Some(base_sha) = &mut base_sha {
				let Some(base_commit) = repo_client.get_commit_by_sha(&base_sha).await.context("get base commit")? else {
					repo_client
						.send_message(context.issue_number, format!("Base commit `{}` was not found", base_sha))
						.await?;
					return Ok(());
				};

				*base_sha = base_commit.sha;
			}

			if let Some(head_sha) = &mut head_sha {
				let Some(head_commit) = repo_client.get_commit_by_sha(&head_sha).await.context("get head commit")? else {
					repo_client
						.send_message(context.issue_number, format!("Head commit `{}` was not found", head_sha))
						.await?;
					return Ok(());
				};

				*head_sha = head_commit.sha;
			}

			let head = head_sha
				.as_deref()
				.map(Head::from_sha)
				.unwrap_or_else(|| Head::from_pr(&context.pr));

			let base = base_sha
				.as_deref()
				.map(Base::from_sha)
				.unwrap_or_else(|| Base::from_pr(&context.pr));

			let branch = format!(
				"{}/{}",
				context.config.try_branch_prefix.trim_end_matches('/'),
				context.issue_number
			);

			conn
				.transaction(|conn| {
					Box::pin(async {
						let current = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;

						if let Some(run) = get_active_ci_run(conn, context.repo_id, context.pr.number as i64).await? {
							if run.is_dry_run {
								cancel_ci_run(conn, run.id, client).await.context("cancel ci run")?;
							} else {
								repo_client.send_message(context.issue_number, &format!("🚨 This PR already has a active merge {}", match run.status {
									GithubCiRunStatus::Queued => "queued",
									GithubCiRunStatus::Pending | GithubCiRunStatus::Running => "in progress",
									status => anyhow::bail!("impossible CI status: {:?}", status),
								})).await?;

								return Ok(());
							}
						}

						let run_id = create_ci_run(
							conn,
							context.repo_id,
							context.issue_number as i64,
							&branch,
							0,
							context.user.id,
							&base,
							&head,
							true,
						)
						.await
						.context("create ci run")?;

						UpdatePr::new(&context.pr, &current).do_update(conn).await.context("update pr")?;
						start_ci_run(conn, run_id, client, &context.config).await.context("start ci run")?;

						Ok(())
					})
				})
				.await
				.context("update pr merge queue")?;

		}
		DryRunCommand::Cancel => {
			if let Some(run) = get_active_ci_run(conn, context.repo_id, context.pr.number as i64).await? {
				if run.is_dry_run {
					cancel_ci_run(conn, run.id, client).await.context("cancel ci run")?;
				} else {
					repo_client.send_message(context.issue_number, "🚨 This PR is currently merging, use `?brawl -r` to cancel a merge run.").await?;
				}
			}
		}
	}

	Ok(())
}
