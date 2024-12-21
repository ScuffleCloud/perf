use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::CiRun;
use crate::database::pr::Pr;
use crate::github::merge_workflow::GitHubMergeWorkflow;
use crate::github::messages;
use crate::github::repo::GitHubRepoClient;

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
) -> anyhow::Result<()> {
    if !context.repo.config().enabled {
        return Ok(());
    }

    let pr = Pr::new(&context.pr, context.user.id, context.repo.id())
        .upsert()
        .get_result(conn)
        .await?;

    let Some(run) = CiRun::latest(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    else {
        context
            .repo
            .send_message(
                context.pr.number,
                &messages::error_no_body("There has never been a merge run on this PR."),
            )
            .await?;
        return Ok(());
    };

    if run.completed_at.is_none() {
        context
            .repo
            .send_message(
                context.pr.number,
                &messages::error_no_body("The previous run has not completed yet."),
            )
            .await?;

        return Ok(());
    }

    let has_perms = if run.is_dry_run {
        context.repo.can_try(context.user.id).await?
    } else {
        context.repo.can_merge(context.user.id).await?
    };

    if !has_perms {
        return Ok(());
    }

    let run = CiRun::insert(context.repo.id(), context.pr.number)
        .base_ref(run.base_ref)
        .head_commit_sha(run.head_commit_sha)
        .ci_branch(run.ci_branch)
        .priority(run.priority)
        .requested_by_id(context.user.id.0 as i64)
        .is_dry_run(run.is_dry_run)
        .approved_by_ids(run.approved_by_ids)
        .build()
        .query()
        .get_result(conn)
        .await?;

    if run.is_dry_run {
        context.repo.merge_workflow().start(&run, context.repo, conn, &pr).await?;
    } else {
        context.repo.merge_workflow().queued(&run, context.repo).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {}
