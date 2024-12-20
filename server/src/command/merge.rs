use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::{Base, CiRun};
use crate::database::pr::Pr;
use crate::github::installation::GitHubRepoClient;

#[derive(Debug)]
pub struct MergeCommand {
    pub reviewers: Vec<String>,
    pub priority: Option<i32>,
}

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
    command: MergeCommand,
) -> anyhow::Result<()> {
    if !context.repo.config().enabled {
        return Ok(());
    }

    if !context.repo.can_merge(context.user.id).await? {
        tracing::debug!("user does not have permission to do this");
        return Ok(());
    }

    if context.pr.merged_at.is_some() {
        tracing::debug!("pull request already merged");
        return Ok(());
    }

    let mut pr = Pr::new(&context.pr, context.user.id, context.repo.id());

    if let Some(priority) = command.priority {
        pr.default_priority = Some(priority);
    }

    if !command.reviewers.is_empty() {
        for reviewer in command.reviewers {
            let user = context.repo.get_user_by_name(&reviewer).await?;
            pr.reviewer_ids.push(user.id.0 as i64);
        }

        pr.reviewer_ids.sort();
        pr.reviewer_ids.dedup();
    }

    // If they didnt provide a list then whoever is issuing the command is
    // approving.
    if pr.reviewer_ids.is_empty() {
        pr.reviewer_ids.push(context.user.id.0 as i64);
    }

    if let Some(run) = CiRun::active(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    {
        run.cancel(conn, context.repo).await?;
    }

    let pr = pr.upsert().get_result(conn).await?;

    // We should now start a CI Run for this PR.
    CiRun::insert(context.repo.id(), context.pr.number)
        .base_ref(Base::from_pr(&context.pr))
        .head_commit_sha(context.pr.head.sha.as_str().into())
        .ci_branch(
            format!(
                "{}/{}",
                context.repo.config().merge_branch_prefix.trim_end_matches('/'),
                context.pr.base.ref_field
            )
            .into(),
        )
        .maybe_priority(command.priority.or(pr.default_priority))
        .requested_by_id(context.user.id.0 as i64)
        .is_dry_run(false)
        .build()
        .query()
        .get_result(conn)
        .await?;

    Ok(())
}
