use std::sync::Arc;

use anyhow::Context;
use diesel_async::{AsyncConnection, AsyncPgConnection};

use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;
use crate::schema::ci::{Base, CiRun, Head, InsertCiRun};
use crate::schema::enums::GithubCiRunStatus;
use crate::schema::pr::{Pr, UpdatePr};

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

    if let Some(base_sha) = &mut command.base_sha {
        let Some(base_commit) = repo_client.get_commit_by_sha(base_sha).await.context("get base commit")? else {
            repo_client
                .send_message(context.issue_number, format!("Base commit `{}` was not found", base_sha))
                .await?;
            return Ok(());
        };

        *base_sha = base_commit.sha;
    }

    if let Some(head_sha) = &mut command.head_sha {
        let Some(head_commit) = repo_client.get_commit_by_sha(head_sha).await.context("get head commit")? else {
            repo_client
                .send_message(context.issue_number, format!("Head commit `{}` was not found", head_sha))
                .await?;
            return Ok(());
        };

        *head_sha = head_commit.sha;
    }

    let head = command
        .head_sha
        .as_deref()
        .map(Head::from_sha)
        .unwrap_or_else(|| Head::from_pr(&context.pr));

    let base = command
        .base_sha
        .as_deref()
        .map(Base::from_sha)
        .unwrap_or_else(|| Base::from_pr(&context.pr));

    let branch = format!(
        "{}/{}",
        context.config.try_branch_prefix.trim_end_matches('/'),
        context.issue_number
    );

    conn.transaction(|conn| {
        Box::pin(async {
            let mut current = Pr::fetch_or_create(context.repo_id, &context.pr, conn).await?;

            if let Some(run) = CiRun::get_active(conn, context.repo_id, context.pr.number as i64).await? {
                if run.is_dry_run {
                    run.cancel(conn, client).await.context("cancel ci run")?;
                } else {
                    repo_client
                        .send_message(
                            context.issue_number,
                            &format!(
                                "🚨 This PR already has a active merge {}",
                                match run.status {
                                    GithubCiRunStatus::Queued => "queued",
                                    GithubCiRunStatus::InProgress => "in progress",
                                    status => anyhow::bail!("impossible CI status: {:?}", status),
                                }
                            ),
                        )
                        .await?;

                    return Ok(());
                }
            }

            UpdatePr::new(&context.pr, &mut current)
                .do_update(conn)
                .await
                .context("update pr")?;

            InsertCiRun {
                github_repo_id: context.repo_id.0 as i64,
                github_pr_number: context.issue_number as i32,
                base_ref: &base.to_string(),
                head_commit_sha: head.sha(),
                run_commit_sha: None,
                ci_branch: &branch,
                priority: 0,
                requested_by_id: context.user.id.0 as i64,
                is_dry_run: true,
            }
            .insert(conn, client, &context.config, &current)
            .await
            .context("create ci run")?;

            Ok(())
        })
    })
    .await
    .context("update pr merge queue")?;

    Ok(())
}
