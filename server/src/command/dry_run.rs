use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::{Base, CiRun};
use crate::database::enums::GithubCiRunStatus;
use crate::database::pr::Pr;
use crate::github::installation::GitHubRepoClient;
use crate::github::messages;

#[derive(Debug)]
pub struct DryRunCommand {
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
}

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
    mut command: DryRunCommand,
) -> anyhow::Result<()> {
    if !context.repo.config().enabled {
        return Ok(());
    }

    if !context.repo.can_try(context.user.id).await? {
        tracing::debug!("user does not have permission to do this");
        return Ok(());
    }

    if let Some(base_sha) = &mut command.base_sha {
        let Some(base_commit) = context.repo.get_commit_by_sha(base_sha).await.context("get base commit")? else {
            context
                .repo
                .send_message(
                    context.pr.number,
                    &messages::error_no_body(format!("Base commit `{base_sha}` was not found")),
                )
                .await?;
            return Ok(());
        };

        *base_sha = base_commit.sha;
    }

    if let Some(head_sha) = &mut command.head_sha {
        let Some(head_commit) = context.repo.get_commit_by_sha(head_sha).await.context("get head commit")? else {
            context
                .repo
                .send_message(
                    context.pr.number,
                    &messages::error_no_body(format!("Head commit `{head_sha}` was not found")),
                )
                .await?;
            return Ok(());
        };

        *head_sha = head_commit.sha;
    }

    let base = command
        .base_sha
        .as_deref()
        .map(Base::from_sha)
        .unwrap_or_else(|| Base::from_pr(&context.pr));

    let branch = format!(
        "{}/{}",
        context.repo.config().try_branch_prefix.trim_end_matches('/'),
        context.pr.number
    );

    if let Some(run) = CiRun::active(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    {
        if run.is_dry_run {
            run.cancel(conn, context.repo).await.context("cancel ci run")?;
        } else {
            context
                .repo
                .send_message(
                    context.pr.number,
                    &messages::error_no_body(format!(
                        "This PR already has a active merge {}",
                        match run.status {
                            GithubCiRunStatus::Queued => "queued",
                            GithubCiRunStatus::InProgress => "in progress",
                            status => anyhow::bail!("impossible CI status: {:?}", status),
                        }
                    )),
                )
                .await?;

            return Ok(());
        }
    }

    Pr::new(&context.pr, context.user.id, context.repo.id())
        .upsert()
        .get_result(conn)
        .await
        .context("update pr")?;

    CiRun::insert(context.repo.id(), context.pr.number)
        .base_ref(base)
        .head_commit_sha(
            command
                .head_sha
                .as_deref()
                .unwrap_or_else(|| context.pr.head.sha.as_ref())
                .into(),
        )
        .ci_branch(branch.into())
        .requested_by_id(context.user.id.0 as i64)
        .is_dry_run(true)
        .build()
        .query()
        .get_result(conn)
        .await
        .context("insert ci run")?;

    Ok(())
}
