use diesel_async::AsyncPgConnection;

use super::BrawlCommandContext;
use crate::github::installation::GitHubRepoClient;
use crate::github::messages;

pub async fn handle<R: GitHubRepoClient>(
    _: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
) -> anyhow::Result<()> {
    // Should we also say what permissions the user has?
    context
        .repo
        .send_message(
            context.pr.number,
            &messages::pong(
                context.user.login,
                if context.repo.config().enabled {
                    "enabled"
                } else {
                    "disabled"
                },
            ),
        )
        .await?;

    Ok(())
}
