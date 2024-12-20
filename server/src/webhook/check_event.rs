use anyhow::Context;
use chrono::{DateTime, Utc};
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::RepositoryId;
use serde::Deserialize;

use crate::database::ci_run::CiRun;
use crate::database::ci_run_check::CiCheck;
use crate::database::pr::Pr;
use crate::github::installation::GitHubRepoClient;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
    Waiting,
    Requested,
    Pending,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRunEvent {
    pub id: i64,
    pub name: String,
    pub head_sha: String,
    pub html_url: Option<String>,
    pub details_url: Option<String>,
    pub url: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: CheckRunStatus,
    pub conclusion: Option<CheckRunConclusion>,
}

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
        run.refresh(conn, repo, &pr).await?;
    }

    Ok(())
}
