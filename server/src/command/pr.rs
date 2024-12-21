use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::CiRun;
use crate::database::enums::GithubCiRunStatus;
use crate::database::pr::Pr;
use crate::github::merge_workflow::GitHubMergeWorkflow;
use crate::github::messages;
use crate::github::repo::GitHubRepoClient;

#[derive(Debug)]
pub enum PullRequestCommand {
    Opened,
    Push,
    IntoDraft,
    ReadyForReview,
    Closed,
}

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
    _: PullRequestCommand,
) -> anyhow::Result<()> {
    let mut updated = false;

    if let Some(current) = Pr::find(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch pr")?
    {
        let update = current.update_from(&context.pr);
        if update.needs_update() {
            update.query().execute(conn).await?;
            updated = true;
        }
    } else {
        Pr::new(&context.pr, context.user.id, context.repo.id())
            .insert()
            .execute(conn)
            .await
            .context("insert pr")?;
    }

    if updated && context.pr.merged_at.is_none() {
        // We need to cancel the checks on the current run somehow...
        if let Some(run) = CiRun::active(context.repo.id(), context.pr.number)
            .get_result(conn)
            .await
            .optional()
            .context("fetch ci run")?
        {
            if !run.is_dry_run {
                context.repo.merge_workflow().cancel(&run, context.repo, conn).await?;
                context
                    .repo
                    .send_message(
                        run.github_pr_number as u64,
                        &messages::error_no_body(format!(
                            "PR state was changed while merge was {}, cancelling merge.",
                            match run.status {
                                GithubCiRunStatus::Queued => "queued",
                                GithubCiRunStatus::InProgress => "in progress",
                                _ => anyhow::bail!("impossible CI status: {:?}", run.status),
                            },
                        )),
                    )
                    .await?;
            }
        }
    }

    Ok(())
}
