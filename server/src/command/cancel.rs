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
    Pr::new(&context.pr, context.user.id, context.repo.id())
        .upsert()
        .get_result(conn)
        .await
        .context("update pr")?;

    if let Some(run) = CiRun::active(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    {
        let has_perms = if run.is_dry_run {
            context.repo.can_try(context.user.id).await?
        } else {
            context.repo.can_merge(context.user.id).await?
        };

        if !has_perms {
            tracing::debug!("user does not have permission to do this");
            return Ok(());
        }

        context
            .repo
            .merge_workflow()
            .cancel(&run, context.repo, conn)
            .await
            .context("cancel ci run")?;

        context
            .repo
            .send_message(context.pr.number, &messages::error_no_body("Cancelled CI run"))
            .await?;
    }

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use octocrab::models::UserId;

    use super::*;
    use crate::database::ci_run::Base;
    use crate::github::config::{GitHubBrawlRepoConfig, Permission};
    use crate::github::models::{PullRequest, User};
    use crate::github::repo::test_utils::{MockRepoAction, MockRepoClient};

    #[derive(Debug, Clone)]
    struct MockMergeWorkflow {
        pub cancelled: Arc<AtomicBool>,
    }

    impl GitHubMergeWorkflow for MockMergeWorkflow {
        async fn cancel(
            &self,
            run: &CiRun<'_>,
            repo: &impl GitHubRepoClient,
            conn: &mut AsyncPgConnection,
        ) -> anyhow::Result<()> {
            let _ = (run, repo, conn);
            if self
                .cancelled
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                Ok(())
            } else {
                panic!("CALLED CANCEL TWICE");
            }
        }
    }

    #[tokio::test]
    async fn test_cancel_try() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow {
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            ..Default::default()
        };

        Pr::new(&pr, UserId(1), client.id())
            .upsert()
            .get_result(&mut conn)
            .await
            .unwrap();

        CiRun::insert(client.id, 1)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .requested_by_id(1)
            .is_dry_run(true)
            .approved_by_ids(vec![1])
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

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(1));
                // First pr is a merge pr
                assert_eq!(permissions, vec![Permission::Team("try".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 1);
                insta::assert_snapshot!(message, @"🚨 Cancelled CI run");
                result.send(Ok(())).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        task.await.unwrap();

        mock.cancelled
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .expect("cancelled was called");
    }

    #[tokio::test]
    async fn test_cancel_merge() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow {
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            merge_permissions: vec![Permission::Team("merge".to_string())],
            ..Default::default()
        });

        let pr = PullRequest {
            number: 2,
            ..Default::default()
        };

        Pr::new(&pr, UserId(2), client.id())
            .upsert()
            .get_result(&mut conn)
            .await
            .unwrap();

        CiRun::insert(client.id, 2)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .requested_by_id(1)
            .is_dry_run(true)
            .approved_by_ids(vec![1])
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
                    pr: Arc::new(PullRequest {
                        number: 2,
                        ..Default::default()
                    }),
                    user: User {
                        id: UserId(2),
                        login: "test".to_string(),
                    },
                },
            )
            .await
            .unwrap();

            (conn, client)
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(2));
                // First pr is a merge pr
                assert_eq!(permissions, vec![Permission::Team("merge".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::SendMessage {
                issue_number,
                message,
                result,
            } => {
                assert_eq!(issue_number, 2);
                insta::assert_snapshot!(message, @"🚨 Cancelled CI run");
                result.send(Ok(())).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        task.await.unwrap();

        mock.cancelled
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .expect("cancelled was called");
    }

    #[tokio::test]
    async fn test_cancel_no_perms() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow {
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            merge_permissions: vec![Permission::Team("merge".to_string())],
            ..Default::default()
        });

        let pr = PullRequest {
            number: 3,
            ..Default::default()
        };

        Pr::new(&pr, UserId(3), client.id())
            .upsert()
            .get_result(&mut conn)
            .await
            .unwrap();

        CiRun::insert(client.id, 3)
            .base_ref(Base::from_sha("base"))
            .head_commit_sha(Cow::Borrowed("head"))
            .ci_branch(Cow::Borrowed("ci"))
            .requested_by_id(1)
            .is_dry_run(false)
            .approved_by_ids(vec![1])
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
                    pr: Arc::new(pr),
                    user: User {
                        id: UserId(3),
                        login: "test".to_string(),
                    },
                },
            )
            .await
            .unwrap();
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![Permission::Team("merge".to_string())]);
                result.send(Ok(false)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        task.await.unwrap();

        assert!(
            !AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed),
            "cancelled was called"
        );
    }

    #[tokio::test]
    async fn test_cancel_no_run() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow {
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let (client, _) = MockRepoClient::new(mock.clone());

        let pr = PullRequest {
            number: 4,
            ..Default::default()
        };

        handle(
            &mut conn,
            BrawlCommandContext {
                repo: &client,
                pr: Arc::new(pr),
                user: User {
                    id: UserId(4),
                    login: "test".to_string(),
                },
            },
        )
        .await
        .unwrap();

        assert!(
            !AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed),
            "cancelled was called"
        );
    }
}
