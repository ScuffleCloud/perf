use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::ci::{CiRun, InsertCiRun};
use crate::github::installation::InstallationClient;
use crate::pr::{Pr, UpdatePr};

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
) -> anyhow::Result<()> {
	if !context.config.enabled {
		return Ok(());
	}

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	let mut pr = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;
	UpdatePr::new(&context.pr, &mut pr).do_update(conn).await?;

	let Some(run) = CiRun::get_latest(conn, context.repo_id, context.pr.number as i64).await? else {
		repo_client
			.send_message(context.issue_number, "🚨 There has never been a merge run on this PR.")
			.await?;
		return Ok(());
	};

	if run.completed_at.is_none() {
		repo_client
			.send_message(
				context.issue_number,
				format!(
					"🚨 There is currently an active run, cancel it first using `?brawl {}`.",
					if run.is_dry_run { "-r" } else { "try cancel" }
				),
			)
			.await?;
		return Ok(());
	}

	let permissions = if run.is_dry_run {
		&context.config.merge_permissions
	} else {
		context.config.try_permissions()
	};

	if !repo_client.has_permission(context.user.id, permissions).await? {
		return Ok(());
	}

	InsertCiRun {
		github_repo_id: context.repo_id.0 as i64,
		github_pr_number: context.issue_number as i32,
		base_ref: &run.base_ref,
		head_commit_sha: &run.head_commit_sha,
		run_commit_sha: None,
		ci_branch: &run.ci_branch,
		priority: run.priority,
		requested_by_id: context.user.id.0 as i64,
		is_dry_run: run.is_dry_run,
	}
	.insert(conn, client, &context.config, &pr)
	.await?;

	Ok(())
}
