use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::ci::{create_ci_run, get_latest_ci_run, Base, Head};
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

	let pr = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;
	UpdatePr::new(&context.pr, &pr).do_update(conn).await?;

	let Some(run) = get_latest_ci_run(conn, context.repo_id, context.pr.number as i64).await? else {
		repo_client.send_message(context.issue_number, "🚨 There has never been a merge run on this PR.").await?;
		return Ok(());
	};

	if run.completed_at.is_none() {
		repo_client.send_message(context.issue_number, format!(
			"🚨 There is currently an active run, cancel it first using `?brawl {}`.",
			if run.is_dry_run { "-r" } else { "try cancel" }
		)).await?;
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

	create_ci_run(conn,
		context.repo_id,
		context.pr.number as i64,
		&run.ci_branch,
		run.priority,
		context.user.id,
		&Base::from_string(&run.base_ref).context("bad base ref")?,
		&Head::from_sha(&run.head_commit_sha),
		run.is_dry_run,
	).await?;
	repo_client.send_message(context.issue_number, "🚨 This PR is currently merging, use `?brawl -r` to cancel a merge run.").await?;

	Ok(())
}
