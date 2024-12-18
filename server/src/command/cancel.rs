use std::sync::Arc;

use anyhow::Context;
use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::github::installation::InstallationClient;
use crate::schema::ci::CiRun;
use crate::schema::pr::{Pr, UpdatePr};

pub async fn handle(
    client: &Arc<InstallationClient>,
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext,
) -> anyhow::Result<()> {
    let mut current = Pr::fetch_or_create(context.repo_id, &context.pr, conn)
        .await
        .context("fetch pr")?;
    UpdatePr::new(&context.pr, &mut current)
        .do_update(conn)
        .await
        .context("update pr")?;

    if let Some(run) = CiRun::get_active(conn, context.repo_id, context.pr.number as i64).await? {
        let perms = if run.is_dry_run {
            context.config.try_permissions()
        } else {
            &context.config.merge_permissions
        };

        let repo_client = client.get_repository(context.repo_id).context("get repo")?;

        if !repo_client.has_permission(context.user.id, perms).await? {
            tracing::debug!("user does not have permission to do this");
            return Ok(());
        }

        run.cancel(conn, client).await.context("cancel ci run")?;

        repo_client.send_message(context.issue_number, "🛑 Cancelled CI run").await?;
    }

    Ok(())
}
