use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::CiRun;
use crate::database::pr::Pr;
use crate::github::merge_workflow::GitHubMergeWorkflow;
use crate::github::messages;
use crate::github::repo::GitHubRepoClient;

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
) -> anyhow::Result<()> {
    if !context.repo.config().enabled {
        return Ok(());
    }

    let pr = Pr::new(&context.pr, context.user.id, context.repo.id())
        .upsert()
        .get_result(conn)
        .await?;

    let Some(run) = CiRun::latest(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    else {
        context
            .repo
            .send_message(
                context.pr.number,
                &messages::error_no_body("There has never been a merge run on this PR."),
            )
            .await?;
        return Ok(());
    };

    if run.completed_at.is_none() {
        context
            .repo
            .send_message(
                context.pr.number,
                &messages::error_no_body("The previous run has not completed yet."),
            )
            .await?;

        return Ok(());
    }

    let has_perms = if run.is_dry_run {
        context.repo.can_try(context.user.id).await?
    } else {
        context.repo.can_merge(context.user.id).await?
    };

    if !has_perms {
        return Ok(());
    }

    let run = CiRun::insert(context.repo.id(), context.pr.number)
        .base_ref(run.base_ref)
        .head_commit_sha(run.head_commit_sha)
        .ci_branch(run.ci_branch)
        .priority(run.priority)
        .requested_by_id(context.user.id.0 as i64)
        .is_dry_run(run.is_dry_run)
        .approved_by_ids(run.approved_by_ids)
        .build()
        .query()
        .get_result(conn)
        .await?;

    if run.is_dry_run {
        context.repo.merge_workflow().start(&run, context.repo, conn, &pr).await?;
    } else {
        context.repo.merge_workflow().queued(&run, context.repo).await?;
    }

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use octocrab::models::UserId;

    use super::*;
    use crate::command::BrawlCommand;
    use crate::database::ci_run::Base;
    use crate::database::enums::GithubCiRunStatus;
    use crate::database::get_test_connection;
    use crate::github::config::{GitHubBrawlRepoConfig, Permission};
    use crate::github::models::{PullRequest, User};
    use crate::github::repo::test_utils::{MockRepoAction, MockRepoClient};

    #[derive(Default, Clone)]
    struct MockMergeWorkFlow {
        start: Arc<AtomicBool>,
        queued: Arc<AtomicBool>,
    }

    impl GitHubMergeWorkflow for MockMergeWorkFlow {
        async fn start(
            &self,
            _: &CiRun<'_>,
            _: &impl GitHubRepoClient,
            _: &mut AsyncPgConnection,
            _: &Pr<'_>,
        ) -> anyhow::Result<bool> {
            self.start
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .expect("start already set");
            Ok(true)
        }

        async fn queued(&self, _: &CiRun<'_>, _: &impl GitHubRepoClient) -> anyhow::Result<()> {
            self.queued
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .expect("queued already set");
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_retry_no_runs() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let task = tokio::spawn(async move {
            handle(
                &mut conn,
                BrawlCommandContext {
                    repo: &client,
                    user: User {
                        id: UserId(1),
                        login: "test".into(),
                    },
                    pr: Arc::new(PullRequest {
                        number: 1,
                        ..Default::default()
                    }),
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
                insta::assert_snapshot!(message, @"🚨 There has never been a merge run on this PR.");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?}", r),
        }

        let (mut conn, client) = task.await.unwrap();

        let pr = Pr::find(client.id(), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.github_pr_number, 1);

        assert!(!AtomicBool::load(&mock.start, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.queued, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_retry_not_completed() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, mut rx) = MockRepoClient::new(mock.clone());

        Pr::new(
            &PullRequest {
                number: 1,
                ..Default::default()
            },
            UserId(1),
            client.id(),
        )
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        CiRun::insert(client.id(), 1)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .priority(1)
            .requested_by_id(1)
            .is_dry_run(false)
            .approved_by_ids(vec![])
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            handle(
                &mut conn,
                BrawlCommandContext {
                    repo: &client,
                    user: User {
                        id: UserId(1),
                        login: "test".into(),
                    },
                    pr: Arc::new(PullRequest {
                        number: 1,
                        ..Default::default()
                    }),
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
                insta::assert_snapshot!(message, @"🚨 The previous run has not completed yet.");
                result.send(Ok(())).unwrap();
            }
            r => panic!("unexpected action: {:?}", r),
        }

        let (mut conn, client) = task.await.unwrap();

        let pr = Pr::find(client.id(), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.github_pr_number, 1);

        assert!(!AtomicBool::load(&mock.start, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.queued, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_retry_dry_run() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".into())]),
            ..Default::default()
        });

        Pr::new(
            &PullRequest {
                number: 1,
                ..Default::default()
            },
            UserId(1),
            client.id(),
        )
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        let run = CiRun::insert(client.id(), 1)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .priority(1)
            .requested_by_id(5)
            .is_dry_run(true)
            .approved_by_ids(vec![1, 2, 3])
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        CiRun::update(run.id)
            .completed_at(chrono::Utc::now())
            .status(GithubCiRunStatus::Failure)
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        user: User {
                            id: UserId(1),
                            login: "test".into(),
                        },
                        pr: Arc::new(PullRequest {
                            number: 1,
                            ..Default::default()
                        }),
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();

            (conn, client)
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(1));
                assert_eq!(permissions, vec![Permission::Team("try".into())]);
                result.send(Ok(true)).unwrap();
            }
            r => panic!("unexpected action: {:?}", r),
        }

        let (mut conn, client) = task.await.unwrap();

        let pr = Pr::find(client.id(), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.github_pr_number, 1);

        let run = CiRun::latest(client.id(), 1).get_result(&mut conn).await.unwrap();

        dbg!(&run);

        assert_eq!(run.base_ref, Base::from_sha("base"));
        assert_eq!(run.head_commit_sha, Cow::Borrowed("head"));
        assert_eq!(run.ci_branch, Cow::Borrowed("ci"));
        assert_eq!(run.priority, 1);
        assert_eq!(run.requested_by_id, 1);
        assert!(run.is_dry_run);
        assert_eq!(run.approved_by_ids, vec![1, 2, 3]);
        assert_eq!(run.status, GithubCiRunStatus::Queued);

        assert!(AtomicBool::load(&mock.start, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.queued, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_retry_merge() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".into())]),
            merge_permissions: vec![Permission::Team("merge".into())],
            ..Default::default()
        });

        Pr::new(
            &PullRequest {
                number: 1,
                ..Default::default()
            },
            UserId(1),
            client.id(),
        )
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        let run = CiRun::insert(client.id(), 1)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .priority(1)
            .requested_by_id(5)
            .is_dry_run(false)
            .approved_by_ids(vec![1, 2, 3])
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        CiRun::update(run.id)
            .completed_at(chrono::Utc::now())
            .status(GithubCiRunStatus::Failure)
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        user: User {
                            id: UserId(1),
                            login: "test".into(),
                        },
                        pr: Arc::new(PullRequest {
                            number: 1,
                            ..Default::default()
                        }),
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();

            (conn, client)
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(1));
                assert_eq!(permissions, vec![Permission::Team("merge".into())]);
                result.send(Ok(true)).unwrap();
            }
            r => panic!("unexpected action: {:?}", r),
        }

        let (mut conn, client) = task.await.unwrap();

        let pr = Pr::find(client.id(), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.github_pr_number, 1);

        let run = CiRun::latest(client.id(), 1).get_result(&mut conn).await.unwrap();

        dbg!(&run);

        assert_eq!(run.base_ref, Base::from_sha("base"));
        assert_eq!(run.head_commit_sha, Cow::Borrowed("head"));
        assert_eq!(run.ci_branch, Cow::Borrowed("ci"));
        assert_eq!(run.priority, 1);
        assert_eq!(run.requested_by_id, 1);
        assert!(!run.is_dry_run);
        assert_eq!(run.approved_by_ids, vec![1, 2, 3]);
        assert_eq!(run.status, GithubCiRunStatus::Queued);

        assert!(!AtomicBool::load(&mock.start, std::sync::atomic::Ordering::Relaxed));
        assert!(AtomicBool::load(&mock.queued, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_retry_no_permissions() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".into())]),
            merge_permissions: vec![Permission::Team("merge".into())],
            ..Default::default()
        });

        Pr::new(
            &PullRequest {
                number: 1,
                ..Default::default()
            },
            UserId(1),
            client.id(),
        )
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        let run = CiRun::insert(client.id(), 1)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .priority(1)
            .requested_by_id(5)
            .is_dry_run(false)
            .approved_by_ids(vec![1, 2, 3])
            .build()
            .query()
            .get_result(&mut conn)
            .await
            .unwrap();

        CiRun::update(run.id)
            .completed_at(chrono::Utc::now())
            .status(GithubCiRunStatus::Failure)
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let task = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                handle(
                    &mut conn,
                    BrawlCommandContext {
                        repo: &client,
                        user: User {
                            id: UserId(1),
                            login: "test".into(),
                        },
                        pr: Arc::new(PullRequest {
                            number: 1,
                            ..Default::default()
                        }),
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();

            (conn, client)
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(1));
                assert_eq!(permissions, vec![Permission::Team("merge".into())]);
                result.send(Ok(false)).unwrap();
            }
            r => panic!("unexpected action: {:?}", r),
        }

        let (mut conn, client) = task.await.unwrap();

        let pr = Pr::find(client.id(), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.github_pr_number, 1);

        let run = CiRun::latest(client.id(), 1).get_result(&mut conn).await.unwrap();

        assert_eq!(run.base_ref, Base::from_sha("base"));
        assert_eq!(run.head_commit_sha, Cow::Borrowed("head"));
        assert_eq!(run.ci_branch, Cow::Borrowed("ci"));
        assert_eq!(run.priority, 1);
        assert_eq!(run.requested_by_id, 5);
        assert!(!run.is_dry_run);
        assert_eq!(run.approved_by_ids, vec![1, 2, 3]);
        assert_eq!(run.status, GithubCiRunStatus::Failure);

        assert!(!AtomicBool::load(&mock.start, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.queued, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_retry_not_enabled() {
        let mut conn = get_test_connection().await;
        let mock = MockMergeWorkFlow::default();
        let (client, _) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            enabled: false,
            ..Default::default()
        });

        BrawlCommand::Retry
            .handle(
                &mut conn,
                BrawlCommandContext {
                    repo: &client,
                    user: User {
                        id: UserId(1),
                        login: "test".into(),
                    },
                    pr: Arc::new(PullRequest {
                        number: 1,
                        ..Default::default()
                    }),
                },
            )
            .await
            .unwrap();

        assert!(!AtomicBool::load(&mock.start, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.queued, std::sync::atomic::Ordering::Relaxed));
    }
}
