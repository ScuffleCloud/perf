use std::borrow::{Borrow, Cow};
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use axum::http;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::{UserId, UserProfile};
use octocrab::params::repos::Reference;
use octocrab::GitHubError;

use super::installation::GitHubRepoClient;
use super::messages::{self, IssueMessage};
use crate::database::ci_run::{Base, CiRun};
use crate::database::ci_run_check::CiCheck;
use crate::database::enums::{GithubCiRunStatus, GithubCiRunStatusCheckStatus};
use crate::database::pr::Pr;

async fn fetch_reviewers(
    repo: &impl GitHubRepoClient,
    pr: &Pr<'_>,
    requested_by_id: i64,
) -> anyhow::Result<Vec<Arc<UserProfile>>> {
    let mut reviewers = Vec::new();
    for id in &pr.reviewer_ids {
        let user = repo.get_user(UserId(*id as u64)).await?;
        reviewers.push(user);
    }

    if reviewers.is_empty() {
        reviewers.push(repo.get_user(UserId(requested_by_id as u64)).await?);
    }

    Ok(reviewers)
}

fn build_reviewers(users: &[Arc<UserProfile>]) -> impl std::fmt::Display + '_ {
    messages::format_fn(move |f| {
        let mut first = true;
        for reviewer in users {
            if !first {
                write!(f, ",")?;
            }

            write!(f, "{login}", login = reviewer.login)?;
            first = false;
        }

        Ok(())
    })
}

impl CiRun<'_> {
    pub async fn refresh(
        &self,
        conn: &mut AsyncPgConnection,
        repo: &impl GitHubRepoClient,
        pr: &Pr<'_>,
    ) -> anyhow::Result<()> {
        if self.completed_at.is_some() {
            return Ok(());
        }

        let Some(started_at) = self.started_at else {
            return Ok(());
        };

        let checks = CiCheck::for_run(self.id).get_results(conn).await?;

        let checks = checks
            .iter()
            .map(|c| (c.status_check_name.as_ref(), c))
            .collect::<HashMap<_, _>>();

        let mut success = true;
        let mut required_checks = Vec::new();
        let mut missing_checks = Vec::new();

        for check in &repo.config().required_status_checks {
            let Some(check) = checks.get(check.as_str()).copied() else {
                success = false;
                missing_checks.push((check.as_str(), None));
                continue;
            };

            if check.status_check_status == GithubCiRunStatusCheckStatus::Failure {
                self.fail(
                    conn,
                    repo,
                    self.github_pr_number,
                    &messages::tests_failed(&check.status_check_name, &check.url),
                )
                .await?;
                return Ok(());
            } else if check.status_check_status != GithubCiRunStatusCheckStatus::Success {
                success = false;
                missing_checks.push((check.status_check_name.as_ref(), Some(check)));
            }

            required_checks.push(check);
        }

        if success {
            self.success(conn, repo, pr, required_checks.as_ref()).await?;
        } else if chrono::Utc::now().signed_duration_since(started_at)
            > chrono::Duration::minutes(repo.config().timeout_minutes as i64)
        {
            self.fail(
                conn,
                repo,
                self.github_pr_number,
                &messages::tests_timeout(
                    repo.config().timeout_minutes,
                    messages::format_fn(|f| {
                        for (name, check) in &missing_checks {
                            if let Some(check) = check {
                                writeln!(f, "- [{name}]({url}) (pending)", name = name, url = check.url)?;
                            } else {
                                writeln!(f, "- {name} (not started)", name = name)?;
                            }
                        }

                        Ok(())
                    }),
                ),
            )
            .await?;
        }

        Ok(())
    }

    async fn fail(
        &self,
        conn: &mut AsyncPgConnection,
        repo: &impl GitHubRepoClient,
        pr_number: i32,
        message: impl Borrow<IssueMessage>,
    ) -> anyhow::Result<()> {
        if CiRun::update(self.id)
            .status(GithubCiRunStatus::Failure)
            .completed_at(chrono::Utc::now())
            .build()
            .not_done()
            .execute(conn)
            .await
            .context("update")?
            == 0
        {
            anyhow::bail!("run already completed");
        }

        repo.send_message(pr_number as u64, message.borrow())
            .await
            .context("send message")?;

        tracing::info!(
            run_id = %self.id,
            repo_id = %self.github_repo_id,
            pr_number = %self.github_pr_number,
            run_type = if self.is_dry_run { "dry run" } else { "merge" },
            run_sha = self.run_commit_sha.as_deref().unwrap_or("<not started>"),
            run_branch = %self.ci_branch,
            url = %repo.pr_link(self.github_pr_number as u64),
            "ci run failed",
        );

        repo.delete_branch(&self.ci_branch).await?;

        Ok(())
    }

    async fn success(
        &self,
        conn: &mut AsyncPgConnection,
        repo: &impl GitHubRepoClient,
        pr: &Pr<'_>,
        checks: &[&CiCheck<'_>],
    ) -> anyhow::Result<()> {
        if CiRun::update(self.id)
            .status(GithubCiRunStatus::Success)
            .completed_at(chrono::Utc::now())
            .build()
            .not_done()
            .execute(conn)
            .await
            .context("update")?
            == 0
        {
            anyhow::bail!("run already completed");
        }

        let mut checks_message = String::new();
        for check in checks {
            checks_message.push_str(&format!(
                "- [{name}]({url})\n",
                name = check.status_check_name,
                url = check.url,
            ));
        }

        let Some(run_commit_sha) = self.run_commit_sha.as_ref() else {
            anyhow::bail!("run commit sha is null");
        };

        let duration = messages::format_fn(|f| {
            let duration = self
                .completed_at
                .unwrap_or(chrono::Utc::now())
                .signed_duration_since(self.started_at.unwrap_or(chrono::Utc::now()));

            let seconds = duration.num_seconds() % 60;
            let minutes = (duration.num_seconds() / 60) % 60;
            let hours = duration.num_seconds() / 60 / 60;

            if hours > 0 {
                write!(f, "{hours:0>2}:{minutes:0>2}:{seconds:0>2}")?;
            } else if minutes > 0 {
                write!(f, "{minutes:0>2}:{seconds:0>2}")?;
            } else {
                write!(f, "{seconds}s")?;
            }

            Ok(())
        });

        if self.is_dry_run {
            let requested_by = repo.get_user(UserId(self.requested_by_id as u64)).await?;

            repo.send_message(
                self.github_pr_number as u64,
                &messages::tests_pass(
                    duration,
                    checks_message,
                    &requested_by.login,
                    repo.commit_link(run_commit_sha),
                    run_commit_sha,
                ),
            )
            .await
            .context("send message")?;
        } else {
            repo.send_message(
                self.github_pr_number as u64,
                &messages::tests_pass_merge(
                    duration,
                    checks_message,
                    build_reviewers(&fetch_reviewers(repo, pr, self.requested_by_id).await?),
                    repo.commit_link(run_commit_sha),
                    &pr.target_branch,
                ),
            )
            .await
            .context("send message")?;

            match repo.push_branch(&pr.target_branch, run_commit_sha, false).await {
                Ok(_) => {}
                Err(e) => {
                    self.fail(
                        conn,
                        repo,
                        self.github_pr_number,
                        &messages::error(
                            messages::format_fn(|f| {
                                writeln!(f, "Tests passed but failed to push to branch {}", pr.target_branch)
                            }),
                            e,
                        ),
                    )
                    .await?;
                }
            }
        }

        tracing::info!(
            run_id = %self.id,
            repo_id = %self.github_repo_id,
            pr_number = %self.github_pr_number,
            run_type = if self.is_dry_run { "dry run" } else { "merge" },
            run_sha = self.run_commit_sha.as_deref().unwrap_or("<not started>"),
            run_branch = %self.ci_branch,
            url = repo.pr_link(self.github_pr_number as u64),
            "ci run completed",
        );

        // Delete the CI branch
        match repo.delete_branch(&self.ci_branch).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!(
                    run_id = %self.id,
                    repo_id = %self.github_repo_id,
                    pr_number = %self.github_pr_number,
                    ci_branch = %self.ci_branch,
                    "failed to delete ci branch: {e:#}",
                );
            }
        }

        Ok(())
    }

    pub async fn start(
        &self,
        conn: &mut AsyncPgConnection,
        repo: &impl GitHubRepoClient,
        pr: &Pr<'_>,
    ) -> anyhow::Result<bool> {
        let update = CiRun::update(self.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now());

        let base_sha = match &self.base_ref {
            Base::Commit(sha) => Cow::Borrowed(sha.as_ref()),
            Base::Branch(branch) => {
                let Some(branch) = repo.get_ref(&Reference::Branch(branch.to_string())).await? else {
                    self.fail(
                        conn,
                        repo,
                        self.github_pr_number,
                        &messages::error(
                            messages::format_fn(|f| {
                                writeln!(f, "Failed to find branch {branch}")
                            }),
                            "The base branch could not be found, and was likely deleted after the PR was added to the queue.",
                        ),
                    )
                    .await?;

                    return Ok(false);
                };

                match branch.object {
                    octocrab::models::repos::Object::Commit { sha, .. } => Cow::Owned(sha),
                    octocrab::models::repos::Object::Tag { sha, .. } => Cow::Owned(sha),
                    _ => anyhow::bail!("invalid base"),
                }
            }
        };

        let reviewers = fetch_reviewers(repo, pr, self.requested_by_id).await?;

        let commit_message = messages::commit_message(
            repo.pr_link(self.github_pr_number as u64),
            &pr.source_branch,
            build_reviewers(&reviewers),
            &pr.title,
            &pr.body,
            messages::format_fn(|f| {
                for reviewer in &reviewers {
                    writeln!(
                        f,
                        "Reviewed-by: {login} <{id}+{login}@users.noreply.github.com>",
                        login = reviewer.login,
                        id = reviewer.id
                    )?;
                }

                Ok(())
            }),
        );

        let commit = match repo.create_merge(&commit_message, &base_sha, &self.head_commit_sha).await {
            Ok(commit) => commit,
            Err(e) => {
                self.fail(
                    conn,
                    repo,
                    self.github_pr_number,
                    if let Some(octocrab::Error::GitHub {
                        source:
                            GitHubError {
                                status_code: http::StatusCode::CONFLICT,
                                ..
                            },
                        ..
                    }) = e.downcast_ref::<octocrab::Error>()
                    {
                        messages::merge_conflict(
                            &pr.source_branch,
                            &pr.target_branch,
                            repo.commit_link(&self.head_commit_sha),
                            repo.commit_link(&base_sha),
                        )
                    } else {
                        messages::error("Failed to start CI run", e)
                    },
                )
                .await?;

                return Ok(false);
            }
        };

        match repo.push_branch(&self.ci_branch, &commit.sha, true).await {
            Ok(_) => {}
            Err(e) => {
                self.fail(
                    conn,
                    repo,
                    self.github_pr_number,
                    &messages::error("Failed to start CI run", e),
                )
                .await?;

                return Ok(false);
            }
        }

        update
            .run_commit_sha(Cow::Borrowed(&commit.sha))
            .build()
            .queued()
            .execute(conn)
            .await
            .context("update")?;

        repo.send_message(
            self.github_pr_number as u64,
            &messages::tests_start(repo.commit_link(&self.head_commit_sha), repo.commit_link(&commit.sha)),
        )
        .await?;

        tracing::info!(
            run_id = %self.id,
            repo_id = %self.github_repo_id,
            pr_number = %self.github_pr_number,
            run_type = if self.is_dry_run { "dry run" } else { "merge" },
            run_sha = commit.sha,
            run_branch = %self.ci_branch,
            url = repo.pr_link(self.github_pr_number as u64),
            "ci run started",
        );

        Ok(true)
    }

    pub async fn cancel(&self, conn: &mut AsyncPgConnection, repo: &impl GitHubRepoClient) -> anyhow::Result<()> {
        if CiRun::update(self.id)
            .status(GithubCiRunStatus::Cancelled)
            .completed_at(chrono::Utc::now())
            .build()
            .not_done()
            .execute(conn)
            .await
            .context("update")?
            == 0
        {
            return Ok(());
        }

        tracing::info!(
            run_id = %self.id,
            repo_id = %self.github_repo_id,
            pr_number = %self.github_pr_number,
            run_type = if self.is_dry_run { "dry run" } else { "merge" },
            run_sha = self.run_commit_sha.as_deref().unwrap_or("<not started>"),
            run_branch = %self.ci_branch,
            url = repo.pr_link(self.github_pr_number as u64),
            "ci run cancelled",
        );

        if let Some(run_commit_sha) = &self.run_commit_sha {
            // let page = repo
            //     .client()
            //     .workflows(repo.owner.login.clone(), repo.name.clone())
            //     .list_all_runs()
            //     .branch(self.ci_branch.clone())
            //     .per_page(100)
            //     .page(1u32)
            //     .send()
            //     .await?;

            // let mut total_workflows = page.items;

            // while let Some(page) = client.client().get_page(&page.next).await? {
            //     total_workflows.extend(page.items);
            // }

            // for workflow in total_workflows.into_iter().filter(|w| w.head_sha ==
            // run_commit_sha) {     if workflow.conclusion.is_none() {
            //         client
            //             .client()
            //             .post::<_, serde_json::Value>(
            //                 format!(
            //                     "/repos/{owner}/{repo}/actions/runs/{id}/cancel",
            //                     owner = repo_owner.login,
            //                     repo = repo.name,
            //                     id = workflow.id
            //                 ),
            //                 None::<&()>,
            //             )
            //             .await?;
            //         tracing::info!(
            //             run_id = %run_id,
            //             repo_id = %run.github_repo_id,
            //             "cancelled workflow {id} on https://github.com/{owner}/{repo}/actions/runs/{id}",
            //             owner = repo_owner.login,
            //             repo = repo.name,
            //             id = workflow.id
            //         );
            //     }
            // }

            repo.delete_branch(&self.ci_branch).await?;
        }

        Ok(())
    }

    pub async fn queued(&self, repo: &impl GitHubRepoClient, pr: &Pr<'_>) -> anyhow::Result<()> {
        repo.send_message(
            self.github_pr_number as u64,
            &messages::commit_approved(
                repo.commit_link(&self.head_commit_sha),
                build_reviewers(&fetch_reviewers(repo, pr, self.requested_by_id).await?),
            ),
        )
        .await?;

        Ok(())
    }
}
