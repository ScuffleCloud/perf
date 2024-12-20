use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::CiRun;
use crate::database::pr::Pr;
use crate::github::installation::GitHubRepoClient;
use crate::github::messages;

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
) -> anyhow::Result<()> {
    Pr::new(&context.pr, context.user.id, context.repo.id())
        .upsert()
        .get_result(conn)
        .await
        .context("update pr")?;

    if let Some(run) = CiRun::active(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    {
        let has_perms = if run.is_dry_run {
            context.repo.can_try(context.user.id).await?
        } else {
            context.repo.can_merge(context.user.id).await?
        };

        if !has_perms {
            tracing::debug!("user does not have permission to do this");
            return Ok(());
        }

        run.cancel(conn, context.repo).await.context("cancel ci run")?;

        context
            .repo
            .send_message(context.pr.number, &messages::error_no_body("Cancelled CI run"))
            .await?;
    }

    Ok(())
}
