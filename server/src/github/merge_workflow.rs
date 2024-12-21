use std::borrow::{Borrow, Cow};
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use octocrab::models::UserId;
use octocrab::params::repos::Reference;

use super::messages::{self, IssueMessage};
use super::models::User;
use super::repo::GitHubRepoClient;
use crate::database::ci_run::{Base, CiRun};
use crate::database::ci_run_check::CiCheck;
use crate::database::enums::{GithubCiRunStatus, GithubCiRunStatusCheckStatus};
use crate::database::pr::Pr;
use crate::github::repo::MergeResult;

fn format_duration(duration: chrono::Duration) -> impl std::fmt::Display {
    messages::format_fn(move |f| {
        let seconds = duration.num_seconds() % 60;
        let minutes = (duration.num_seconds() / 60) % 60;
        let hours = duration.num_seconds() / 60 / 60;

        if hours > 0 {
            write!(f, "{hours:0}:{minutes:0>2}:{seconds:0>2}")?;
        } else if minutes > 0 {
            write!(f, "{minutes:0}:{seconds:0>2}")?;
        } else {
            write!(f, "{seconds}s")?;
        }

        Ok(())
    })
}

async fn fetch_approved_by(repo: &impl GitHubRepoClient, run: &CiRun<'_>) -> anyhow::Result<Vec<Arc<User>>> {
    let mut approved_by = Vec::new();
    for id in &run.approved_by_ids {
        let user = repo.get_user(UserId(*id as u64)).await?;
        approved_by.push(user);
    }

    Ok(approved_by)
}

fn build_reviewers(users: &[Arc<User>], tag: bool) -> impl std::fmt::Display + '_ {
    messages::format_fn(move |f| {
        if users.is_empty() {
            write!(f, "<no reviewers>")?;
            return Ok(());
        }

        let mut first = true;
        for reviewer in users {
            if !first {
                if tag {
                    write!(f, " ")?;
                } else {
                    write!(f, ",")?;
                }
            }

            if tag {
                write!(f, "@")?;
            }

            write!(f, "{login}", login = reviewer.login)?;
            first = false;
        }

        Ok(())
    })
}

pub trait GitHubMergeWorkflow: Send + Sync {
    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn refresh(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        let _ = (run, repo, conn, pr);
        unimplemented!("GitHubMergeWorkflow::refresh");
        #[allow(unreachable_code)]
        std::future::pending()
    }

    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn cancel(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        let _ = (run, repo, conn);
        unimplemented!("GitHubMergeWorkflow::cancel");
        #[allow(unreachable_code)]
        std::future::pending()
    }

    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn queued(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        let _ = (run, repo);
        unimplemented!("GitHubMergeWorkflow::queued");
        #[allow(unreachable_code)]
        std::future::pending()
    }

    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn start(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        let _ = (run, repo, conn, pr);
        unimplemented!("GitHubMergeWorkflow::start");
        #[allow(unreachable_code)]
        std::future::pending()
    }
}

impl<T: GitHubMergeWorkflow> GitHubMergeWorkflow for &T {
    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn refresh(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        (*self).refresh(run, repo, conn, pr)
    }

    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn cancel(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        (*self).cancel(run, repo, conn)
    }

    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn queued(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        (*self).queued(run, repo)
    }

    #[cfg_attr(all(test, coverage_nightly), coverage(off))]
    fn start(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        (*self).start(run, repo, conn, pr)
    }
}

pub struct DefaultMergeWorkflow;

impl DefaultMergeWorkflow {
    async fn fail(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr_number: i32,
        message: impl Borrow<IssueMessage>,
    ) -> anyhow::Result<()> {
        if CiRun::update(run.id)
            .status(GithubCiRunStatus::Failure)
            .completed_at(chrono::Utc::now())
            .build()
            .query()
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
            run_id = %run.id,
            repo_id = %run.github_repo_id,
            pr_number = %run.github_pr_number,
            run_type = if run.is_dry_run { "dry run" } else { "merge" },
            run_sha = run.run_commit_sha.as_deref().unwrap_or("<not started>"),
            run_branch = %run.ci_branch,
            url = %repo.pr_link(run.github_pr_number as u64),
            "ci run failed",
        );

        repo.delete_branch(&run.ci_branch).await?;

        Ok(())
    }

    async fn success(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
        checks: &[&CiCheck<'_>],
    ) -> anyhow::Result<()> {
        println!("success");

        if CiRun::update(run.id)
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

        let checks_message = messages::format_fn(|f| {
            for check in checks {
                writeln!(f, "- [{name}]({url})", name = check.status_check_name, url = check.url)?;
            }

            Ok(())
        });

        let Some(run_commit_sha) = run.run_commit_sha.as_ref() else {
            anyhow::bail!("run commit sha is null");
        };

        let duration = format_duration(
            run.completed_at
                .unwrap_or(chrono::Utc::now())
                .signed_duration_since(run.started_at.unwrap_or(chrono::Utc::now())),
        );

        if run.is_dry_run {
            let requested_by = repo.get_user(UserId(run.requested_by_id as u64)).await?;

            repo.send_message(
                run.github_pr_number as u64,
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
                run.github_pr_number as u64,
                &messages::tests_pass_merge(
                    duration,
                    checks_message,
                    build_reviewers(&fetch_approved_by(repo, run).await?, true),
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
                        run,
                        repo,
                        conn,
                        run.github_pr_number,
                        &messages::error(
                            messages::format_fn(|f| {
                                writeln!(f, "Tests passed but failed to push to branch {}", pr.target_branch)
                            }),
                            e,
                        ),
                    )
                    .await?;

                    return Ok(());
                }
            }
        }

        tracing::info!(
            run_id = %run.id,
            repo_id = %run.github_repo_id,
            pr_number = %run.github_pr_number,
            run_type = if run.is_dry_run { "dry run" } else { "merge" },
            run_sha = run.run_commit_sha.as_deref().unwrap_or("<not started>"),
            run_branch = %run.ci_branch,
            url = repo.pr_link(run.github_pr_number as u64),
            "ci run completed",
        );

        // Delete the CI branch
        match repo.delete_branch(&run.ci_branch).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!(
                    run_id = %run.id,
                    repo_id = %run.github_repo_id,
                    pr_number = %run.github_pr_number,
                    ci_branch = %run.ci_branch,
                    "failed to delete ci branch: {e:#}",
                );
            }
        }

        Ok(())
    }
}

impl GitHubMergeWorkflow for DefaultMergeWorkflow {
    async fn refresh(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
    ) -> anyhow::Result<bool> {
        if run.completed_at.is_some() {
            return Ok(false);
        }

        let Some(started_at) = run.started_at else {
            return Ok(false);
        };

        let checks = CiCheck::for_run(run.id).get_results(conn).await?;

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
                    run,
                    repo,
                    conn,
                    run.github_pr_number,
                    &messages::tests_failed(&check.status_check_name, &check.url),
                )
                .await?;
                return Ok(true);
            } else if check.status_check_status != GithubCiRunStatusCheckStatus::Success {
                success = false;
                missing_checks.push((check.status_check_name.as_ref(), Some(check)));
            }

            required_checks.push(check);
        }

        if success {
            self.success(run, repo, conn, pr, required_checks.as_ref()).await?;
        } else if chrono::Utc::now().signed_duration_since(started_at)
            > chrono::Duration::minutes(repo.config().timeout_minutes as i64)
        {
            self.fail(
                run,
                repo,
                conn,
                run.github_pr_number,
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

        Ok(true)
    }

    async fn start(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
        pr: &Pr<'_>,
    ) -> anyhow::Result<bool> {
        let update = CiRun::update(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now());

        let base_sha = match &run.base_ref {
            Base::Commit(sha) => Cow::Borrowed(sha.as_ref()),
            Base::Branch(branch) => {
                let Some(branch) = repo.get_ref_latest_commit(&Reference::Branch(branch.to_string())).await? else {
                    self.fail(
                        run,
                        repo,
                        conn,
                        run.github_pr_number,
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

                Cow::Owned(branch.sha)
            }
        };

        let approved_by = fetch_approved_by(repo, run).await?;

        let requested_by = repo.get_user(UserId(run.requested_by_id as u64)).await?;

        let commit_message = messages::commit_message(
            repo.pr_link(run.github_pr_number as u64),
            &pr.source_branch,
            if run.is_dry_run {
                "<dry>".to_owned()
            } else if !approved_by.is_empty() {
                build_reviewers(&approved_by, false).to_string()
            } else {
                requested_by.login.clone()
            },
            &pr.title,
            &pr.body,
            messages::format_fn(|f| {
                writeln!(
                    f,
                    "Requested-by: {login} <{id}+{login}@users.noreply.github.com>",
                    login = requested_by.login,
                    id = requested_by.id
                )?;

                for approved_by in &approved_by {
                    writeln!(
                        f,
                        "Reviewed-by: {login} <{id}+{login}@users.noreply.github.com>",
                        login = approved_by.login,
                        id = approved_by.id
                    )?;
                }

                Ok(())
            }),
        );

        let commit = match repo.create_merge(&commit_message, &base_sha, &run.head_commit_sha).await {
            Ok(MergeResult::Success(commit)) => commit,
            Ok(MergeResult::Conflict) => {
                self.fail(
                    run,
                    repo,
                    conn,
                    run.github_pr_number,
                    messages::merge_conflict(
                        &pr.source_branch,
                        &pr.target_branch,
                        repo.commit_link(&run.head_commit_sha),
                        repo.commit_link(&base_sha),
                    ),
                )
                .await?;

                return Ok(false);
            }
            Err(e) => {
                self.fail(
                    run,
                    repo,
                    conn,
                    run.github_pr_number,
                    &messages::error(messages::format_fn(|f| write!(f, "Failed to create merge commit")), e),
                )
                .await?;

                return Ok(false);
            }
        };

        update
            .run_commit_sha(Cow::Borrowed(&commit.sha))
            .build()
            .queued()
            .execute(conn)
            .await
            .context("update")?;

        match repo.push_branch(&run.ci_branch, &commit.sha, true).await {
            Ok(_) => {}
            Err(e) => {
                self.fail(
                    run,
                    repo,
                    conn,
                    run.github_pr_number,
                    &messages::error(
                        messages::format_fn(|f| write!(f, "Failed to push branch {branch}", branch = run.ci_branch)),
                        e,
                    ),
                )
                .await?;

                return Ok(false);
            }
        }

        repo.send_message(
            run.github_pr_number as u64,
            &messages::tests_start(repo.commit_link(&run.head_commit_sha), repo.commit_link(&commit.sha)),
        )
        .await?;

        tracing::info!(
            run_id = %run.id,
            repo_id = %run.github_repo_id,
            pr_number = %run.github_pr_number,
            run_type = if run.is_dry_run { "dry run" } else { "merge" },
            run_sha = commit.sha,
            run_branch = %run.ci_branch,
            url = repo.pr_link(run.github_pr_number as u64),
            "ci run started",
        );

        Ok(true)
    }

    async fn cancel(
        &self,
        run: &CiRun<'_>,
        repo: &impl GitHubRepoClient,
        conn: &mut AsyncPgConnection,
    ) -> anyhow::Result<()> {
        if CiRun::update(run.id)
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
            run_id = %run.id,
            repo_id = %run.github_repo_id,
            pr_number = %run.github_pr_number,
            run_type = if run.is_dry_run { "dry run" } else { "merge" },
            run_sha = run.run_commit_sha.as_deref().unwrap_or("<not started>"),
            run_branch = %run.ci_branch,
            url = repo.pr_link(run.github_pr_number as u64),
            "ci run cancelled",
        );

        if let Some(run_commit_sha) = &run.run_commit_sha {
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

            repo.delete_branch(&run.ci_branch).await?;
        }

        Ok(())
    }

    async fn queued(&self, run: &CiRun<'_>, repo: &impl GitHubRepoClient) -> anyhow::Result<()> {
        let requested_by = repo.get_user(UserId(run.requested_by_id as u64)).await?;

        repo.send_message(
            run.github_pr_number as u64,
            &messages::commit_approved(
                repo.commit_link(&run.head_commit_sha),
                format!("@{login}", login = requested_by.login),
                build_reviewers(&fetch_approved_by(repo, run).await?, true),
            ),
        )
        .await?;

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::time::Duration;

    use chrono::Utc;
    use octocrab::models::RepositoryId;
    use tokio::sync::mpsc;

    use super::*;
    use crate::database::ci_run::{InsertCiRun, UpdateCiRun};
    use crate::database::enums::{GithubPrMergeStatus, GithubPrStatus};
    use crate::github::config::GitHubBrawlRepoConfig;
    use crate::github::models::{CheckRunConclusion, CheckRunEvent, CheckRunStatus, Commit};
    use crate::github::repo::test_utils::{MockRepoAction, MockRepoClient};

    #[bon::builder]
    fn mock_ci_run<'a>(
        #[builder(default = false)] dry_run: bool,
        #[builder(default = 1)] requested_by_id: i64,
        #[builder(default = vec![])] approved_by_ids: Vec<i64>,
        #[builder(default = "head")] head_commit_sha: &'a str,
        #[builder(default = "branch")] ci_branch: &'a str,
        #[builder(default = Base::Commit(Cow::Borrowed("sha")))] base_ref: Base<'a>,
        #[builder(default = GithubCiRunStatus::InProgress)] status: GithubCiRunStatus,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> CiRun<'a> {
        CiRun {
            id: 1,
            github_repo_id: 1,
            github_pr_number: 1,
            is_dry_run: dry_run,
            requested_by_id,
            head_commit_sha: Cow::Borrowed(head_commit_sha),
            ci_branch: Cow::Borrowed(ci_branch),
            run_commit_sha: None,
            approved_by_ids,
            base_ref,
            completed_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            started_at,
            status,
            priority: 0,
        }
    }

    #[tokio::test]
    async fn test_fetch_approved_by() {
        let (client, mut rx) = MockRepoClient::new(DefaultMergeWorkflow);

        let run = mock_ci_run().approved_by_ids(vec![1, 2, 3]).call();

        let users = tokio::spawn(async move { fetch_approved_by(&client, &run).await.unwrap() });

        let user1 = Arc::new(User {
            id: UserId(1),
            login: "user1".to_owned(),
        });

        let user2 = Arc::new(User {
            id: UserId(2),
            login: "user2".to_owned(),
        });

        let user3 = Arc::new(User {
            id: UserId(3),
            login: "user3".to_owned(),
        });

        for user in [&user1, &user2, &user3] {
            match rx.recv().await.unwrap() {
                MockRepoAction::GetUser { user_id, result } if user_id == user.id => result.send(Ok(user.clone())).unwrap(),
                r => panic!("unexpected action: {:?} expected get user with id {:?}", r, user.id),
            }
        }

        let users = tokio::time::timeout(Duration::from_secs(1), users).await.unwrap().unwrap();

        assert_eq!(users, vec![user1.clone(), user2.clone(), user3.clone(),]);
    }

    #[tokio::test]
    async fn test_build_reviewers() {
        let users: &[Arc<User>] = &[
            Arc::new(User {
                id: UserId(1),
                login: "user1".to_owned(),
            }),
            Arc::new(User {
                id: UserId(2),
                login: "user2".to_owned(),
            }),
            Arc::new(User {
                id: UserId(3),
                login: "user3".to_owned(),
            }),
        ];

        insta::assert_snapshot!(build_reviewers(users, true).to_string(), @"@user1 @user2 @user3");
        insta::assert_snapshot!(build_reviewers(users, false).to_string(), @"user1,user2,user3");
        insta::assert_snapshot!(build_reviewers(&[], true).to_string(), @"<no reviewers>");
        insta::assert_snapshot!(build_reviewers(&[], false).to_string(), @"<no reviewers>");
    }

    async fn ci_run_test_boilerplate(
        insert_ci_run: InsertCiRun<'_>,
    ) -> (
        AsyncPgConnection,
        MockRepoClient<DefaultMergeWorkflow>,
        Pr<'static>,
        CiRun<'static>,
        mpsc::Receiver<MockRepoAction>,
    ) {
        let mut conn = crate::database::get_test_connection().await;

        let (client, rx) = MockRepoClient::new(DefaultMergeWorkflow);

        let pr = Pr {
            github_repo_id: 1,
            github_pr_number: 1,
            title: "title".into(),
            body: "body".into(),
            merge_status: GithubPrMergeStatus::Ready,
            author_id: 1,
            assigned_ids: vec![],
            status: GithubPrStatus::Open,
            default_priority: None,
            merge_commit_sha: None,
            target_branch: "target_branch".into(),
            source_branch: "source_branch".into(),
            latest_commit_sha: "latest_commit_sha".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        diesel::insert_into(crate::database::schema::github_pr::table)
            .values(&pr)
            .execute(&mut conn)
            .await
            .unwrap();

        let run = insert_ci_run.query().get_result(&mut conn).await.unwrap();

        (conn, client, pr, run, rx)
    }

    #[tokio::test]
    async fn test_ci_run_refesh_non_applicable() {
        let (mut conn, client, pr, run, rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(false)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        drop(rx);

        let status = tokio::time::timeout(
            Duration::from_secs(1),
            client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!status, "run hasnt started yet so this should return false");

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::Success)
            .started_at(chrono::Utc::now())
            .completed_at(chrono::Utc::now())
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        let status = tokio::time::timeout(
            Duration::from_secs(1),
            client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!status, "run has already completed so this should return false");
    }

    #[tokio::test]
    async fn test_ci_run_refesh_merge_success() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(false)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now())
            .run_commit_sha(Cow::Borrowed("run_commit_sha"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        let check = CheckRunEvent {
            name: "brawl-done".to_owned(),
            status: CheckRunStatus::Completed,
            conclusion: Some(CheckRunConclusion::Success),
            url: "https://github.com/owner/repo/actions/runs/1".to_owned(),
            ..Default::default()
        };

        let check = CiCheck::new(&check, run.id, true);

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            (conn, client, pr)
        });

        let (mut conn, client, pr) = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        check.upsert().execute(&mut conn).await.unwrap();

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🎉 Build successful!
                Completed in 0s
                - [brawl-done](https://github.com/owner/repo/actions/runs/1)

                Approved by: @user1
                Pushing https://github.com/owner/repo/commit/run_commit_sha to target_branch
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::PushBranch {
                branch,
                sha,
                result,
                force,
            } => {
                assert_eq!(branch, "target_branch");
                assert_eq!(sha, "run_commit_sha");
                assert!(!force, "force should be false");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected push branch", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Success);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_refesh_merge_success_push_failure() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(false)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now())
            .run_commit_sha(Cow::Borrowed("run_commit_sha"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        let check = CheckRunEvent {
            name: "brawl-done".to_owned(),
            status: CheckRunStatus::Completed,
            conclusion: Some(CheckRunConclusion::Success),
            url: "https://github.com/owner/repo/actions/runs/1".to_owned(),
            ..Default::default()
        };

        let check = CiCheck::new(&check, run.id, true);

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            (conn, client, pr)
        });

        let (mut conn, client, pr) = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        check.upsert().execute(&mut conn).await.unwrap();

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            conn
        });

        println!("waiting for user");

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        println!("waiting for send message");

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🎉 Build successful!
                Completed in 0s
                - [brawl-done](https://github.com/owner/repo/actions/runs/1)

                Approved by: @user1
                Pushing https://github.com/owner/repo/commit/run_commit_sha to target_branch
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        println!("waiting for push branch");

        match rx.recv().await.unwrap() {
            MockRepoAction::PushBranch {
                branch,
                sha,
                result,
                force,
            } => {
                assert_eq!(branch, "target_branch");
                assert_eq!(sha, "run_commit_sha");
                assert!(!force, "force should be false");
                result.send(Err(anyhow::anyhow!("push failed somehow???"))).unwrap();
            }
            r => panic!("unexpected action: {:?} expected push branch", r),
        }

        println!("waiting for send message on failure");

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🚨 Tests passed but failed to push to branch target_branch

                <details>
                <summary>Error</summary>

                push failed somehow???

                </details>
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        println!("waiting for delete branch");

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        println!("waiting for task completion");

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_refesh_try_success() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now())
            .run_commit_sha(Cow::Borrowed("run_commit_sha"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        let check = CheckRunEvent {
            name: "brawl-done".to_owned(),
            status: CheckRunStatus::Completed,
            conclusion: Some(CheckRunConclusion::Success),
            url: "https://github.com/owner/repo/actions/runs/1".to_owned(),
            ..Default::default()
        };

        let check = CiCheck::new(&check, run.id, true);

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            (conn, client, pr)
        });

        let (mut conn, client, pr) = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        check.upsert().execute(&mut conn).await.unwrap();

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🎉 Try build successful!
                Completed in 0s
                - [brawl-done](https://github.com/owner/repo/actions/runs/1)

                Requested by: @user1
                Build commit: https://github.com/owner/repo/commit/run_commit_sha (`run_commit_sha`)
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Success);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_refesh_failure() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now())
            .run_commit_sha(Cow::Borrowed("run_commit_sha"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        let check = CheckRunEvent {
            name: "brawl-done".to_owned(),
            status: CheckRunStatus::Completed,
            conclusion: Some(CheckRunConclusion::Failure),
            url: "https://github.com/owner/repo/actions/runs/1".to_owned(),
            ..Default::default()
        };

        let check = CiCheck::new(&check, run.id, true);

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            (conn, client, pr)
        });

        let (mut conn, client, pr) = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        check.upsert().execute(&mut conn).await.unwrap();

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @"💔 Test failed - [brawl-done](https://github.com/owner/repo/actions/runs/1)");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_refesh_timeout() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let client = client.with_config(GitHubBrawlRepoConfig {
            timeout_minutes: 0,
            ..Default::default()
        });

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(chrono::Utc::now())
            .run_commit_sha(Cow::Borrowed("run_commit_sha"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        CiCheck::new(
            &CheckRunEvent {
                name: "brawl-done".to_owned(),
                status: CheckRunStatus::InProgress,
                conclusion: None,
                url: "https://github.com/owner/repo/actions/runs/1".to_owned(),
                ..Default::default()
            },
            run.id,
            true,
        )
        .upsert()
        .execute(&mut conn)
        .await
        .unwrap();

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().refresh(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "run has already completed so this should return false");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                💔 CI run timed out after 0 minutes
                - [brawl-done](https://github.com/owner/repo/actions/runs/1) (pending)
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_start_base_commit_success() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().start(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "start should succeed");

            conn
        });

        // Approved by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (approved by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Requested by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (requested by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Create merge
        match rx.recv().await.unwrap() {
            MockRepoAction::CreateMerge {
                base_sha,
                message,
                head_sha,
                result,
            } => {
                assert_eq!(base_sha, "sha");
                assert_eq!(head_sha, "head_commit_sha");
                insta::assert_snapshot!(message, @r"
                Auto merge of https://github.com/owner/repo/pull/1 - source_branch, r=<dry>

                title
                body

                Requested-by: user1 (requested by) <1+user1 (requested by)@users.noreply.github.com>
                Reviewed-by: user1 (approved by) <1+user1 (approved by)@users.noreply.github.com>
                ");
                result
                    .send(Ok(MergeResult::Success(Commit {
                        sha: "merge_commit_sha".to_owned(),
                        ..Default::default()
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected create merge", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::PushBranch {
                branch,
                sha,
                result,
                force,
            } => {
                assert_eq!(branch, "ci_branch");
                assert_eq!(sha, "merge_commit_sha");
                assert!(force, "force should be true");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected push branch", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @"⌛ Trying commit https://github.com/owner/repo/commit/head_commit_sha with merge https://github.com/owner/repo/commit/merge_commit_sha...");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::InProgress);
        assert!(run.completed_at.is_none());
        assert_eq!(run.run_commit_sha, Some(Cow::Borrowed("merge_commit_sha")));
        assert!(run.started_at.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_start_base_branch_success() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Branch(Cow::Borrowed("branch")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().start(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(status, "start should succeed");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetRefLatestCommit { gh_ref, result } => {
                assert!(
                    matches!(&gh_ref, Reference::Branch(b) if b == "branch"),
                    "expected branch reference got {:?}",
                    gh_ref
                );

                result
                    .send(Ok(Some(Commit {
                        sha: "branch_commit_sha".to_owned(),
                        ..Default::default()
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get ref latest commit", r),
        }

        // Approved by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (approved by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Requested by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (requested by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Create merge
        match rx.recv().await.unwrap() {
            MockRepoAction::CreateMerge {
                base_sha,
                message,
                head_sha,
                result,
            } => {
                assert_eq!(base_sha, "branch_commit_sha");
                assert_eq!(head_sha, "head_commit_sha");
                insta::assert_snapshot!(message, @r"
                Auto merge of https://github.com/owner/repo/pull/1 - source_branch, r=<dry>

                title
                body

                Requested-by: user1 (requested by) <1+user1 (requested by)@users.noreply.github.com>
                Reviewed-by: user1 (approved by) <1+user1 (approved by)@users.noreply.github.com>
                ");
                result
                    .send(Ok(MergeResult::Success(Commit {
                        sha: "merge_commit_sha".to_owned(),
                        ..Default::default()
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected create merge", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::PushBranch {
                branch,
                sha,
                result,
                force,
            } => {
                assert_eq!(branch, "ci_branch");
                assert_eq!(sha, "merge_commit_sha");
                assert!(force, "force should be true");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected push branch", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @"⌛ Trying commit https://github.com/owner/repo/commit/head_commit_sha with merge https://github.com/owner/repo/commit/merge_commit_sha...");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::InProgress);
        assert!(run.completed_at.is_none());
        assert_eq!(run.run_commit_sha, Some(Cow::Borrowed("merge_commit_sha")));
        assert!(run.started_at.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_start_base_branch_not_found() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Branch(Cow::Borrowed("branch")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().start(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(!status, "start should fail");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetRefLatestCommit { gh_ref, result } => {
                assert!(
                    matches!(&gh_ref, Reference::Branch(b) if b == "branch"),
                    "expected branch reference got {:?}",
                    gh_ref
                );

                result.send(Ok(None)).unwrap();
            }
            r => panic!("unexpected action: {:?} expected get ref latest commit", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🚨 Failed to find branch branch

                <details>
                <summary>Error</summary>

                The base branch could not be found, and was likely deleted after the PR was added to the queue.

                </details>
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_none());
        assert!(run.started_at.is_none());
    }

    #[tokio::test]
    async fn test_ci_run_start_failed_merge_failed() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().start(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(!status, "start should fail");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (requested by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Approved by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (approved by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Create merge
        match rx.recv().await.unwrap() {
            MockRepoAction::CreateMerge {
                base_sha,
                head_sha,
                result,
                ..
            } => {
                assert_eq!(base_sha, "sha");
                assert_eq!(head_sha, "head_commit_sha");
                result.send(Err(anyhow::anyhow!("failed to create merge"))).unwrap();
            }
            r => panic!("unexpected action: {:?} expected create merge", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🚨 Failed to create merge commit
                <details>
                <summary>Error</summary>

                failed to create merge

                </details>
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_none());
        assert!(run.started_at.is_none());
    }

    #[tokio::test]
    async fn test_ci_run_start_failed_push() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().start(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(!status, "start should fail");

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (requested by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Approved by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (approved by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Create merge
        match rx.recv().await.unwrap() {
            MockRepoAction::CreateMerge {
                base_sha,
                head_sha,
                result,
                ..
            } => {
                assert_eq!(base_sha, "sha");
                assert_eq!(head_sha, "head_commit_sha");
                result
                    .send(Ok(MergeResult::Success(Commit {
                        sha: "merge_commit_sha".to_owned(),
                        ..Default::default()
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected create merge", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::PushBranch {
                branch,
                sha,
                result,
                force,
            } => {
                assert_eq!(branch, "ci_branch");
                assert_eq!(sha, "merge_commit_sha");
                assert!(force, "force should be true");
                result.send(Err(anyhow::anyhow!("failed to push"))).unwrap();
            }
            r => panic!("unexpected action: {:?} expected push branch", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                🚨 Failed to push branch ci_branch
                <details>
                <summary>Error</summary>

                failed to push

                </details>
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
        assert!(run.started_at.is_some());
    }

    #[tokio::test]
    async fn test_ci_run_start_conflict() {
        let (mut conn, client, pr, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(true)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                client.merge_workflow().start(&run, &client, &mut conn, &pr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(!status, "start should fail");

            conn
        });

        // Approved by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (approved by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Requested by
        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (requested by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        // Create merge
        match rx.recv().await.unwrap() {
            MockRepoAction::CreateMerge {
                base_sha,
                message,
                head_sha,
                result,
            } => {
                assert_eq!(base_sha, "sha");
                assert_eq!(head_sha, "head_commit_sha");
                insta::assert_snapshot!(message, @r"
                Auto merge of https://github.com/owner/repo/pull/1 - source_branch, r=<dry>

                title
                body

                Requested-by: user1 (requested by) <1+user1 (requested by)@users.noreply.github.com>
                Reviewed-by: user1 (approved by) <1+user1 (approved by)@users.noreply.github.com>
                ");
                result.send(Ok(MergeResult::Conflict)).unwrap();
            }
            r => panic!("unexpected action: {:?} expected create merge", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r#"
                🔒 Merge conflict
                This pull request and the `target_branch` branch have diverged in a way
                that cannot be automatically merged. Please rebase your branch ontop of the
                latest `target_branch` branch and let the reviewer approve again.

                Attempted merge from https://github.com/owner/repo/commit/head_commit_sha into https://github.com/owner/repo/commit/sha

                <details><summary>How do I rebase?</summary>

                1. `git checkout source_branch` *(Switch to your branch)*
                2. `git fetch upstream target_branch` *(Fetch the latest changes from the
                   upstream)*
                3. `git rebase upstream/target_branch -p` *(Rebase your branch onto the
                   upstream branch)*
                4. Follow the prompts to resolve any conflicts (use `git status` if you get
                   lost).
                5. `git push self source_branch --force-with-lease` *(Update this PR)*`

                You may also read
                 [*Git Rebasing to Resolve Conflicts* by Drew Blessing](http://blessing.io/git/git-rebase/open-source/2015/08/23/git-rebasing-to-resolve-conflicts.html)
                 for a short tutorial.

                Please avoid the ["**Resolve conflicts**" button](https://help.github.com/articles/resolving-a-merge-conflict-on-github/) on GitHub.
                 It uses `git merge` instead of `git rebase` which makes the PR commit
                history more difficult to read.

                Sometimes step 4 will complete without asking for resolution. This is usually
                due to difference between how `Cargo.lock` conflict is handled during merge
                and rebase. This is normal, and you should still perform step 5 to update
                this PR.

                </details>
                "#);
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected push branch", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Failure);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_none());
        assert!(run.started_at.is_none());
    }

    #[tokio::test]
    async fn test_ci_queued() {
        let (_, client, _, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(false)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            DefaultMergeWorkflow.queued(&run, &client).await.unwrap();
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (requested by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::GetUser { user_id, result } => {
                assert_eq!(user_id, UserId(1));
                result
                    .send(Ok(Arc::new(User {
                        id: UserId(1),
                        login: "user1 (approved by)".to_owned(),
                    })))
                    .unwrap();
            }
            r => panic!("unexpected action: {:?} expected get user", r),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @r"
                📌 Commit https://github.com/owner/repo/commit/head_commit_sha has been approved and added to the merge queue.

                Requested by: @user1 (requested by)

                Approved by: @user1 (approved by)
                ");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected send message", r),
        }

        task.await.unwrap();
    }

    #[tokio::test]
    async fn test_ci_run_cancel_not_started() {
        let (mut conn, client, _, run, _) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(false)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        let task = tokio::spawn(async move {
            client.merge_workflow().cancel(&run, &client, &mut conn).await.unwrap();

            conn
        });

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Cancelled);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_none());
        assert!(run.started_at.is_none());
    }

    #[tokio::test]
    async fn test_ci_run_cancel_started() {
        let (mut conn, client, _, run, mut rx) = ci_run_test_boilerplate(
            InsertCiRun::builder(1, 1)
                .base_ref(Base::Commit(Cow::Borrowed("sha")))
                .head_commit_sha(Cow::Borrowed("head_commit_sha"))
                .ci_branch(Cow::Borrowed("ci_branch"))
                .is_dry_run(false)
                .requested_by_id(1)
                .approved_by_ids(vec![1])
                .build(),
        )
        .await;

        UpdateCiRun::builder(run.id)
            .status(GithubCiRunStatus::InProgress)
            .started_at(Utc::now())
            .run_commit_sha(Cow::Borrowed("run_commit_sha"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();

        let task = tokio::spawn(async move {
            client.merge_workflow().cancel(&run, &client, &mut conn).await.unwrap();

            conn
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::DeleteBranch { branch, result } => {
                assert_eq!(branch, "ci_branch");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?} expected delete branch", r),
        }

        let mut conn = task.await.unwrap();

        let run = CiRun::latest(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(run.status, GithubCiRunStatus::Cancelled);
        assert!(run.completed_at.is_some());
        assert!(run.run_commit_sha.is_some());
        assert!(run.started_at.is_some());
    }

    #[test]
    fn test_format_duration() {
        insta::assert_snapshot!(format_duration(chrono::Duration::seconds(1)), @"1s");
        insta::assert_snapshot!(format_duration(chrono::Duration::seconds(60)), @"1:00");
        insta::assert_snapshot!(format_duration(chrono::Duration::seconds(60 * 60)), @"1:00:00");
        insta::assert_snapshot!(format_duration(chrono::Duration::seconds(60 * 60 + 1)), @"1:00:01");
        insta::assert_snapshot!(format_duration(chrono::Duration::seconds(60 * 61 + 1)), @"1:01:01");
    }
}
