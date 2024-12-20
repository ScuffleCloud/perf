use std::collections::HashMap;
use std::ops::DerefMut;

use anyhow::Context;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::RepositoryId;
use scuffle_context::ContextFutExt;

use crate::database::ci_run::CiRun;
use crate::database::pr::Pr;
use crate::github::installation::GitHubRepoClient;

pub struct AutoStartSvc;

pub trait AutoStartConfig: Send + Sync + 'static {
    type RepoClient: GitHubRepoClient;

    fn repo_client(&self, repo_id: RepositoryId) -> Option<Self::RepoClient>;

    fn database(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<impl DerefMut<Target = AsyncPgConnection> + Send>> + Send;

    fn interval(&self) -> std::time::Duration;
}

impl<C: AutoStartConfig> scuffle_bootstrap::Service<C> for AutoStartSvc {
    async fn enabled(&self, _: &std::sync::Arc<C>) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn run(self, global: std::sync::Arc<C>, ctx: scuffle_context::Context) -> anyhow::Result<()> {
        tracing::info!("starting auto start service");

        while tokio::time::sleep(global.interval()).with_context(&ctx).await.is_some() {
            let mut conn = global.database().await?;

            let runs = CiRun::pending().get_results(&mut conn).await?;
            let mut run_map = HashMap::<_, &CiRun>::new();
            for run in &runs {
                run_map
                    .entry((run.github_repo_id, run.ci_branch.as_ref()))
                    .and_modify(|current| {
                        let higher_priority = run.started_at.is_some()
                            || match current.priority.cmp(&run.priority) {
                                std::cmp::Ordering::Less => true,
                                std::cmp::Ordering::Greater => false,
                                std::cmp::Ordering::Equal => run.id < current.id,
                            };

                        if higher_priority && current.started_at.is_none() {
                            *current = run;
                        }
                    })
                    .or_insert(run);
            }

            for (_, run) in run_map {
                let Some(repo_client) = global.repo_client(RepositoryId(run.github_repo_id as u64)) else {
                    tracing::error!("no installation client found for repo {}", run.github_repo_id);
                    continue;
                };

                if let Err(e) = handle_run(run, &repo_client, &mut conn).await {
                    tracing::error!(
                        "error handling run (repo id: {}, pr number: {}, run id: {}): {}",
                        run.github_repo_id,
                        run.github_pr_number,
                        run.id,
                        e
                    );
                }
            }
        }

        Ok(())
    }
}

async fn handle_run(
    run: &CiRun<'_>,
    repo_client: &impl GitHubRepoClient,
    conn: &mut AsyncPgConnection,
) -> anyhow::Result<()> {
    let pr = Pr::find(RepositoryId(run.github_repo_id as u64), run.github_pr_number as u64)
        .get_result(conn)
        .await
        .context("fetch pr")?;

    if run.started_at.is_none() {
        run.start(conn, repo_client, &pr).await.context("start run")?;
    } else {
        run.refresh(conn, repo_client, &pr).await.context("refresh run")?;
    }

    Ok(())
}
