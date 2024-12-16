use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;
use octocrab::models::pulls::PullRequest;

use super::utils::commit_link;
use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;

#[derive(Debug)]
pub struct DryRunCommand {
	pub head_sha: Option<String>,
	pub base_sha: Option<String>,
}

pub async fn handle(
	client: &Arc<InstallationClient>,
	conn: &mut AsyncPgConnection,
	context: BrawlCommandContext,
	mut command: DryRunCommand,
) -> anyhow::Result<()> {
	if !context.config.queue.enabled {
		return Ok(());
	}

	let repo_client = client.get_repository(context.repo_id).context("get repository")?;

	// Check if the user has permission to do this. (only try or if not set, merge)
	let mut has_permission = false;
	for permission in context
		.config
		.queue
		.try_permissions
		.as_ref()
		.unwrap_or(context.config.queue.merge_permissions.as_ref())
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

	let repo = repo_client.get()?;
	let repo_owner = repo.owner.as_ref().context("repo owner")?;

	if let Some(base_sha) = command.base_sha {
		let Some(base_commit) = repo_client.get_commit_by_sha(&base_sha).await.context("get base commit")? else {
			repo_client
				.send_message(context.issue_number, format!("Base commit `{}` was not found", base_sha))
				.await?;
			return Ok(());
		};

		command.base_sha = Some(base_commit.sha);
	}

	if let Some(head_sha) = command.head_sha {
		let Some(head_commit) = repo_client.get_commit_by_sha(&head_sha).await.context("get head commit")? else {
			repo_client
				.send_message(context.issue_number, format!("Head commit `{}` was not found", head_sha))
				.await?;
			return Ok(());
		};

		command.head_sha = Some(head_commit.sha);
	}

	let pr_commit_link = |pr: &PullRequest, sha: &str| {
		commit_link(
			&pr.head
				.user
				.as_ref()
				.map(|u| u.login.as_str())
				.unwrap_or(repo_owner.login.as_str()),
			&pr.head.repo.as_ref().map(|r| r.name.as_str()).unwrap_or(repo.name.as_str()),
			sha,
		)
	};

	let (head_sha, head_link) = match (command.head_sha.as_deref(), &context.pr) {
		(Some(sha), _) => (sha, commit_link(&repo_owner.login, &repo.name, sha)),
		(None, Some(pr)) => (pr.head.sha.as_str(), pr_commit_link(pr, &pr.head.sha)),
		(None, None) => {
			repo_client
				.send_message(
					context.issue_number,
					"Unable to determine commit to test, try specifying a commit directly.",
				)
				.await?;
			return Ok(());
		}
	};

	let head = repo_client.get_commit(head_sha).await.context("get head commit")?;

	let commit = if let Some(base_sha) = command.base_sha {
		repo_client
			.create_commit(
				format!(
					"Dry run from #{issue} - r={user}\n\nTrying commit: {head_link} into {base_link}",
					issue = context.issue_number,
					user = context.user.login.to_lowercase(),
					head_link = head_link,
					base_link = commit_link(&repo_owner.login, &repo.name, &base_sha),
				),
				vec![head_sha.to_owned(), base_sha.to_owned()],
				head.commit.tree.sha,
			)
			.await
			.context("create commit")?
	} else if command.head_sha.is_some() {
		repo_client
			.create_commit(
				format!(
					"Dry run from #{issue} - r={user}\n\nTrying commit: {head_link}",
					issue = context.issue_number,
					user = context.user.login.to_lowercase(),
					head_link = head_link,
				),
				vec![head_sha.to_owned()],
				head.commit.tree.sha,
			)
			.await
			.context("create commit")?
	} else if let Some(pr) = &context.pr {
		repo_client
			.create_commit(
				format!(
					"Dry run from #{issue} - {head}, r={user}\n\nTrying commit: {head_link} into {base_link}",
					issue = context.issue_number,
					head = pr.head.ref_field,
					user = context.user.login.to_lowercase(),
					head_link = head_link,
					base_link = commit_link(&repo_owner.login, &repo.name, &pr.base.sha),
				),
				vec![head_sha.to_owned(), pr.base.sha.clone()],
				head.commit.tree.sha,
			)
			.await
			.context("create commit")?
	} else {
		anyhow::bail!("no commit sha provided");
	};

	let prefix = context.config.queue.try_branch_prefix.trim_end_matches('/');

	if prefix.is_empty() {
		tracing::error!("try branch prefix is empty");
		return Ok(());
	}

	let branch = format!("{}/{}", prefix, context.issue_number);

	if let Err(err) = repo_client.push_branch(&branch, &commit.sha, true).await {
		tracing::error!("push branch failed: {:#}", err);
		repo_client
			.send_message(
				context.issue_number,
				format!("Failed to push branch `{}` to try: {:#}", branch, err),
			)
			.await?;
		return Ok(());
	}

	repo_client
		.send_message(
			context.issue_number,
			format!(
				"⌛ Testing commit {} with merge {}...",
				commit_link(&repo_owner.login, &repo.name, head_sha),
				commit_link(&repo_owner.login, &repo.name, &commit.sha),
			),
		)
		.await?;

	Ok(())
}
