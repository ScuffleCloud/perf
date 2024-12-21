use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::RepositoryId;

use crate::database::ci_run::CiRun;
use crate::database::ci_run_check::CiCheck;
use crate::database::pr::Pr;
use crate::github::merge_workflow::GitHubMergeWorkflow;
use crate::github::models::CheckRunEvent;
use crate::github::repo::GitHubRepoClient;

pub async fn handle<R: GitHubRepoClient>(
    repo: &R,
    conn: &mut AsyncPgConnection,
    check_run_event: serde_json::Value,
) -> anyhow::Result<()> {
    let check_run_event: CheckRunEvent = serde_json::from_value(check_run_event).context("deserialize check run event")?;

    let Some(run) = CiRun::by_run_commit_sha(&check_run_event.head_sha)
        .get_result(conn)
        .await
        .optional()
        .context("fetch run")?
    else {
        return Ok(());
    };

    // If the run is not still running, we don't need to do anything
    if run.completed_at.is_some() {
        return Ok(());
    }

    let required = repo
        .config()
        .required_status_checks
        .iter()
        .any(|check| check.eq_ignore_ascii_case(&check_run_event.name));

    let ci_check = CiCheck::new(&check_run_event, run.id, required);
    ci_check.upsert().execute(conn).await.context("upsert ci check")?;

    let pr = Pr::find(RepositoryId(run.github_repo_id as u64), run.github_pr_number as u64)
        .get_result(conn)
        .await
        .context("fetch pr")?;

    if required {
        repo.merge_workflow().refresh(&run, repo, conn, &pr).await?;
    }

    Ok(())
}
