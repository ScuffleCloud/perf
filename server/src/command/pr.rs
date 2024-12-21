use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::CiRun;
use crate::database::enums::GithubCiRunStatus;
use crate::database::pr::Pr;
use crate::github::merge_workflow::GitHubMergeWorkflow;
use crate::github::messages;
use crate::github::repo::GitHubRepoClient;

#[derive(Debug, PartialEq, Eq)]
pub enum PullRequestCommand {
    Opened,
    Push,
    IntoDraft,
    ReadyForReview,
    Closed,
}

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
    _: PullRequestCommand,
) -> anyhow::Result<()> {
    if let Some(current) = Pr::find(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch pr")?
    {
        let update = current.update_from(&context.pr);
        if update.needs_update() {
            update.query().execute(conn).await?;

            // Fetch the active run (if there is one)
            let run = CiRun::active(context.repo.id(), context.pr.number)
                .get_result(conn)
                .await
                .optional()
                .context("fetch ci run")?;

            match run {
                Some(run) if !run.is_dry_run => {
                    context.repo.merge_workflow().cancel(&run, context.repo, conn).await?;
                    context
                        .repo
                        .send_message(
                            run.github_pr_number as u64,
                            &messages::error_no_body(format!(
                                "PR has changed while a merge was {}, cancelling the merge job.",
                                match run.status {
                                    GithubCiRunStatus::Queued => "queued",
                                    _ => "in progress",
                                },
                            )),
                        )
                        .await?;
                }
                _ => {}
            }
        }
    } else {
        Pr::new(&context.pr, context.user.id, context.repo.id())
            .insert()
            .execute(conn)
            .await
            .context("insert pr")?;
    }

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use chrono::Utc;
    use octocrab::models::UserId;

    use super::*;
    use crate::command::BrawlCommand;
    use crate::database::ci_run::Base;
    use crate::database::get_test_connection;
    use crate::github::models::{PullRequest, User};
    use crate::github::repo::test_utils::{MockRepoAction, MockRepoClient};

    #[derive(Default, Clone)]
    struct MockMergeWorkFlow {
        cancel: Arc<AtomicBool>,
    }

    impl GitHubMergeWorkflow for MockMergeWorkFlow {
        async fn cancel(
            &self,
            run: &CiRun<'_>,
            _: &impl GitHubRepoClient,
            conn: &mut AsyncPgConnection,
        ) -> anyhow::Result<()> {
            self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);

            CiRun::update(run.id)
                .status(GithubCiRunStatus::Cancelled)
                .completed_at(Utc::now())
                .build()
                .not_done()
                .execute(conn)
                .await?;

            Ok(())
        }
    }

    #[tokio::test]
    async fn test_pr_opened() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, _) = MockRepoClient::new(mock.clone());

        let pr = PullRequest {
            number: 1,
            ..Default::default()
        };

        let task = tokio::spawn(async move {
            BrawlCommand::PullRequest(PullRequestCommand::Opened)
                .handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        pr: Arc::new(pr),
                        user: User {
                            id: UserId(1),
                            login: "test".to_string(),
                        },
                    },
                )
                .await
                .unwrap();

            (conn, client)
        });

        let (mut conn, client) = task.await.unwrap();

        assert!(!AtomicBool::load(&mock.cancel, std::sync::atomic::Ordering::Relaxed));

        let pr = Pr::find(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(pr.is_some(), "PR was not created");
    }

    #[tokio::test]
    async fn test_pr_push_while_merge() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let pr = PullRequest {
            number: 1,
            ..Default::default()
        };

        Pr::new(&pr, UserId(1), client.id())
            .insert()
            .execute(&mut conn)
            .await
            .unwrap();

        CiRun::insert(client.id(), pr.number)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .requested_by_id(1)
            .approved_by_ids(vec![])
            .is_dry_run(false)
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            BrawlCommand::PullRequest(PullRequestCommand::Push)
                .handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        pr: Arc::new(PullRequest {
                            number: 1,
                            title: "test".to_string(),
                            ..Default::default()
                        }),
                        user: User {
                            id: UserId(1),
                            login: "test".to_string(),
                        },
                    },
                )
                .await
                .unwrap();

            (conn, client)
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @"🚨 PR has changed while a merge was queued, cancelling the merge job.");
                result.send(Ok(())).unwrap();
            }
            r => panic!("Expected a send message action, got {:?}", r),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_none(), "Run was not cancelled");
        assert!(AtomicBool::load(&mock.cancel, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_pr_push_while_merge_no_change() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, _) = MockRepoClient::new(mock.clone());

        let pr = PullRequest {
            number: 1,
            title: "test".to_string(),
            body: "test".to_string(),
            ..Default::default()
        };

        Pr::new(&pr, UserId(1), client.id())
            .insert()
            .execute(&mut conn)
            .await
            .unwrap();

        CiRun::insert(client.id(), pr.number)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .requested_by_id(1)
            .approved_by_ids(vec![])
            .is_dry_run(false)
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            BrawlCommand::PullRequest(PullRequestCommand::Push)
                .handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        pr: Arc::new(PullRequest {
                            number: 1,
                            title: "test".to_string(),
                            body: "test".to_string(),
                            ..Default::default()
                        }),
                        user: User {
                            id: UserId(1),
                            login: "test".to_string(),
                        },
                    },
                )
                .await
                .unwrap();

            (conn, client)
        });

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_some(), "Run was cancelled");
        assert!(!AtomicBool::load(&mock.cancel, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_pr_push_while_dry_run() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, _) = MockRepoClient::new(mock.clone());

        let pr = PullRequest {
            number: 1,
            title: "test".to_string(),
            body: "test".to_string(),
            ..Default::default()
        };

        Pr::new(&pr, UserId(1), client.id())
            .insert()
            .execute(&mut conn)
            .await
            .unwrap();

        CiRun::insert(client.id(), pr.number)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .requested_by_id(1)
            .approved_by_ids(vec![])
            .is_dry_run(true)
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            BrawlCommand::PullRequest(PullRequestCommand::Push)
                .handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        pr: Arc::new(PullRequest {
                            number: 1,
                            title: "test".to_string(),
                            body: "2".to_string(),
                            ..Default::default()
                        }),
                        user: User {
                            id: UserId(1),
                            login: "test".to_string(),
                        },
                    },
                )
                .await
                .unwrap();

            (conn, client)
        });

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_some(), "Run was cancelled");
        assert!(!AtomicBool::load(&mock.cancel, std::sync::atomic::Ordering::Relaxed));
    }
}
