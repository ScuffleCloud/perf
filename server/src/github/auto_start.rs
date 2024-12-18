use std::collections::HashMap;

use anyhow::Context;
use diesel_async::pooled_connection::bb8;
use diesel_async::AsyncPgConnection;
use octocrab::models::RepositoryId;
use scuffle_context::ContextFutExt;

use super::GitHubService;
use crate::schema::ci::CiRun;
use crate::schema::pr::Pr;

pub struct AutoStartSvc;

pub trait AutoStartConfig: Send + Sync + 'static {
	fn github_service(&self) -> &GitHubService;

	fn database_pool(&self) -> &bb8::Pool<AsyncPgConnection>;

	fn interval(&self) -> std::time::Duration;
}

impl<C: AutoStartConfig> scuffle_bootstrap::Service<C> for AutoStartSvc {
	async fn enabled(&self, _: &std::sync::Arc<C>) -> anyhow::Result<bool> {
		Ok(true)
	}

	async fn run(self, global: std::sync::Arc<C>, ctx: scuffle_context::Context) -> anyhow::Result<()> {
		tracing::info!("starting auto start service");

		while tokio::time::sleep(global.interval()).with_context(&ctx).await.is_some() {
			let mut conn = global.database_pool().get().await.context("get database connection")?;
			let github_service = global.github_service();

			let runs = CiRun::find_pending_runs(&mut conn).await?;
			let mut run_map = HashMap::<_, &CiRun>::new();
			for run in &runs {
				run_map
					.entry((run.github_repo_id, run.ci_branch.as_str()))
					.and_modify(|current| {
						let higher_priority = run.started_at.is_some()
							|| (run.priority, run.created_at) > (current.priority, current.created_at);

						if higher_priority && current.started_at.is_none() {
							*current = run;
						}
					})
					.or_insert(run);
			}

			for (_, run) in run_map {
				if let Err(e) = handle_run(run, github_service, &mut conn).await {
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

async fn handle_run(run: &CiRun, github_service: &GitHubService, conn: &mut AsyncPgConnection) -> anyhow::Result<()> {
	let repo_id = RepositoryId(run.github_repo_id as u64);

	let Some(installation_client) = github_service.get_client_by_repo(repo_id) else {
		anyhow::bail!("no installation client found for repo {}", run.github_repo_id);
	};

	let config = installation_client
		.get_repo_config(repo_id)
		.await
		.context("get repo config")?;

	let Some(pr) = Pr::fetch(conn, repo_id, run.github_pr_number as i64)
		.await
		.context("fetch pr")?
	else {
		anyhow::bail!("no pr found for run");
	};

	if run.started_at.is_none() {
		run.start(conn, &installation_client, &config, &pr)
			.await
			.context("start run")?;
	} else {
		run.refresh(conn, &installation_client, &config, &pr)
			.await
			.context("refresh run")?;
	}

	Ok(())
}
