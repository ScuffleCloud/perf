use anyhow::Context;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::BrawlCommandContext;
use crate::database::ci_run::{Base, CiRun};
use crate::database::enums::GithubCiRunStatus;
use crate::database::pr::Pr;
use crate::github::merge_workflow::GitHubMergeWorkflow;
use crate::github::messages;
use crate::github::repo::GitHubRepoClient;

#[derive(Debug, PartialEq, Eq)]
pub struct DryRunCommand {
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
}

pub async fn handle<R: GitHubRepoClient>(
    conn: &mut AsyncPgConnection,
    context: BrawlCommandContext<'_, R>,
    mut command: DryRunCommand,
) -> anyhow::Result<()> {
    if !context.repo.config().enabled {
        return Ok(());
    }

    if !context.repo.can_try(context.user.id).await? {
        tracing::debug!("user does not have permission to do this");
        return Ok(());
    }

    if let Some(base_sha) = &mut command.base_sha {
        let Some(base_commit) = context.repo.get_commit_by_sha(base_sha).await.context("get base commit")? else {
            context
                .repo
                .send_message(
                    context.pr.number,
                    &messages::error_no_body(format!("Base commit `{base_sha}` was not found")),
                )
                .await?;
            return Ok(());
        };

        *base_sha = base_commit.sha;
    }

    if let Some(head_sha) = &mut command.head_sha {
        let Some(head_commit) = context.repo.get_commit_by_sha(head_sha).await.context("get head commit")? else {
            context
                .repo
                .send_message(
                    context.pr.number,
                    &messages::error_no_body(format!("Head commit `{head_sha}` was not found")),
                )
                .await?;
            return Ok(());
        };

        *head_sha = head_commit.sha;
    }

    let base = command
        .base_sha
        .as_deref()
        .map(Base::from_sha)
        .unwrap_or_else(|| Base::from_pr(&context.pr));

    let branch = context.repo.config().try_branch(context.pr.number);

    if let Some(run) = CiRun::active(context.repo.id(), context.pr.number)
        .get_result(conn)
        .await
        .optional()
        .context("fetch ci run")?
    {
        if run.is_dry_run {
            context
                .repo
                .merge_workflow()
                .cancel(&run, context.repo, conn)
                .await
                .context("cancel ci run")?;
        } else {
            context
                .repo
                .send_message(
                    context.pr.number,
                    &messages::error_no_body(messages::format_fn(|f| {
                        write!(
                            f,
                            "This PR already has a active merge {}",
                            match run.status {
                                GithubCiRunStatus::Queued => "queued",
                                _ => "in progress",
                            }
                        )
                    })),
                )
                .await?;

            return Ok(());
        }
    }

    let pr = Pr::new(&context.pr, context.user.id, context.repo.id())
        .upsert()
        .get_result(conn)
        .await
        .context("update pr")?;

    let run = CiRun::insert(context.repo.id(), context.pr.number)
        .base_ref(base)
        .head_commit_sha(
            command
                .head_sha
                .as_deref()
                .unwrap_or_else(|| context.pr.head.sha.as_ref())
                .into(),
        )
        .ci_branch(branch.into())
        .requested_by_id(context.user.id.0 as i64)
        .is_dry_run(true)
        .approved_by_ids(Vec::new())
        .build()
        .query()
        .get_result(conn)
        .await
        .context("insert ci run")?;

    context.repo.merge_workflow().start(&run, context.repo, conn, &pr).await?;

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use chrono::Utc;
    use diesel::query_dsl::methods::{FindDsl, SelectDsl};
    use diesel::SelectableHelper;
    use octocrab::models::UserId;

    use super::*;
    use crate::command::BrawlCommand;
    use crate::github::config::{GitHubBrawlRepoConfig, Permission};
    use crate::github::models::{Commit, PrBranch, PullRequest, User};
    use crate::github::repo::test_utils::{MockRepoAction, MockRepoClient};

    #[derive(Debug, Clone, Default)]
    struct MockMergeWorkflow {
        pub started: Arc<AtomicBool>,
        pub cancelled: Arc<AtomicBool>,
    }

    impl GitHubMergeWorkflow for MockMergeWorkflow {
        async fn start(
            &self,
            run: &CiRun<'_>,
            _: &impl GitHubRepoClient,
            conn: &mut AsyncPgConnection,
            _: &Pr<'_>,
        ) -> anyhow::Result<bool> {
            if self
                .started
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                CiRun::update(run.id)
                    .status(GithubCiRunStatus::InProgress)
                    .build()
                    .query()
                    .execute(conn)
                    .await?;
            } else {
                panic!("CALLED START TWICE");
            }

            Ok(true)
        }

        async fn cancel(
            &self,
            run: &CiRun<'_>,
            _: &impl GitHubRepoClient,
            conn: &mut AsyncPgConnection,
        ) -> anyhow::Result<()> {
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
                println!("cancelling");
                CiRun::update(run.id)
                    .status(GithubCiRunStatus::Cancelled)
                    .completed_at(Utc::now())
                    .build()
                    .query()
                    .execute(conn)
                    .await?;
            } else {
                panic!("CALLED CANCEL TWICE");
            }

            Ok(())
        }
    }

    #[tokio::test]
    async fn test_dry_run() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow::default();

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            try_branch_prefix: "try-branch".to_string(),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            base: PrBranch {
                ref_field: "unknown".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let task = tokio::spawn(async move {
            BrawlCommand::DryRun(DryRunCommand {
                head_sha: None,
                base_sha: None,
            })
            .handle(
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

            (conn, client)
        });

        match rx.recv().await.unwrap() {
            MockRepoAction::HasPermission {
                user_id,
                permissions,
                result,
            } => {
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![Permission::Team("try".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.unwrap();

        assert!(run.is_dry_run);
        assert_eq!(run.base_ref, Base::Branch("unknown".into()));
        assert_eq!(run.ci_branch, "try-branch/1");
        assert_eq!(run.requested_by_id, 3);
        assert!(run.approved_by_ids.is_empty());
        assert_eq!(run.status, GithubCiRunStatus::InProgress);

        assert!(AtomicBool::load(&mock.started, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_dry_run_active_dry_run() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow::default();

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            try_branch_prefix: "try-branch".to_string(),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            base: PrBranch {
                ref_field: "unknown".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        Pr::new(&pr, UserId(3), client.id())
            .upsert()
            .get_result(&mut conn)
            .await
            .unwrap();

        let old_run_id = CiRun::insert(client.id(), 1)
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
            .unwrap()
            .id;

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
                DryRunCommand {
                    head_sha: None,
                    base_sha: None,
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
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![Permission::Team("try".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.unwrap();

        assert!(run.is_dry_run);
        assert_eq!(run.base_ref, Base::Branch("unknown".into()));
        assert_eq!(run.ci_branch, "try-branch/1");
        assert_eq!(run.requested_by_id, 3);
        assert!(run.approved_by_ids.is_empty());
        assert_eq!(run.status, GithubCiRunStatus::InProgress);

        assert!(AtomicBool::load(&mock.started, std::sync::atomic::Ordering::Relaxed));
        assert!(AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed));

        let old_run: CiRun<'_> = crate::database::schema::github_ci_runs::dsl::github_ci_runs
            .find(old_run_id)
            .select(CiRun::as_select())
            .get_result(&mut conn)
            .await
            .unwrap();
        assert_eq!(old_run.status, GithubCiRunStatus::Cancelled);
        assert!(old_run.completed_at.is_some());
    }

    #[tokio::test]
    async fn test_dry_run_active_merge() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow::default();

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            try_branch_prefix: "try-branch".to_string(),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            base: PrBranch {
                ref_field: "unknown".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        Pr::new(&pr, UserId(3), client.id())
            .upsert()
            .get_result(&mut conn)
            .await
            .unwrap();

        let run_id = CiRun::insert(client.id(), 1)
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
            .unwrap()
            .id;

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
                DryRunCommand {
                    head_sha: None,
                    base_sha: None,
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
                assert_eq!(user_id, UserId(3));
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
                insta::assert_snapshot!(message, @"🚨 This PR already has a active merge queued");
                result.send(Ok(())).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.unwrap();

        assert_eq!(run.id, run_id);
        assert!(!run.is_dry_run);
        assert_eq!(run.base_ref, Base::from_sha("base"));
        assert_eq!(run.ci_branch, "ci");
        assert_eq!(run.requested_by_id, 1);
        assert_eq!(run.approved_by_ids, vec![1]);
        assert_eq!(run.status, GithubCiRunStatus::Queued);

        assert!(!AtomicBool::load(&mock.started, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_dry_run_base_head_sha_found() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow::default();

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            try_branch_prefix: "try-branch".to_string(),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            base: PrBranch {
                ref_field: "unknown".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

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
                DryRunCommand {
                    head_sha: Some("head".to_string()),
                    base_sha: Some("base".to_string()),
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
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![Permission::Team("try".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::GetCommitBySha { sha, result } => {
                assert_eq!(sha, "base");
                result
                    .send(Ok(Some(Commit {
                        sha: "base_resolved".to_string(),
                        ..Default::default()
                    })))
                    .unwrap();
            }
            _ => panic!("unexpected action"),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::GetCommitBySha { sha, result } => {
                assert_eq!(sha, "head");
                result
                    .send(Ok(Some(Commit {
                        sha: "head_resolved".to_string(),
                        ..Default::default()
                    })))
                    .unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.unwrap();

        assert_eq!(run.base_ref, Base::from_sha("base_resolved"));
        assert_eq!(run.head_commit_sha, Cow::Borrowed("head_resolved"));
        assert_eq!(run.ci_branch, "try-branch/1");
        assert_eq!(run.requested_by_id, 3);
        assert!(run.approved_by_ids.is_empty());
        assert_eq!(run.status, GithubCiRunStatus::InProgress);

        assert!(AtomicBool::load(&mock.started, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_dry_run_head_not_found() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow::default();

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            try_branch_prefix: "try-branch".to_string(),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            base: PrBranch {
                ref_field: "unknown".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

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
                DryRunCommand {
                    head_sha: Some("head".to_string()),
                    base_sha: None,
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
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![Permission::Team("try".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::GetCommitBySha { sha, result } => {
                assert_eq!(sha, "head");
                result.send(Ok(None)).unwrap();
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
                insta::assert_snapshot!(message, @"🚨 Head commit `head` was not found");
                result.send(Ok(())).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_none());

        assert!(!AtomicBool::load(&mock.started, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_dry_run_base_not_found() {
        let mut conn = crate::database::get_test_connection().await;

        let mock = MockMergeWorkflow::default();

        let (client, mut rx) = MockRepoClient::new(mock.clone());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![Permission::Team("try".to_string())]),
            try_branch_prefix: "try-branch".to_string(),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            base: PrBranch {
                ref_field: "unknown".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

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
                DryRunCommand {
                    head_sha: None,
                    base_sha: Some("base".to_string()),
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
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![Permission::Team("try".to_string())]);
                result.send(Ok(true)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        match rx.recv().await.unwrap() {
            MockRepoAction::GetCommitBySha { sha, result } => {
                assert_eq!(sha, "base");
                result.send(Ok(None)).unwrap();
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
                insta::assert_snapshot!(message, @"🚨 Base commit `base` was not found");
                result.send(Ok(())).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_none());

        assert!(!AtomicBool::load(&mock.started, std::sync::atomic::Ordering::Relaxed));
        assert!(!AtomicBool::load(&mock.cancelled, std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_dry_run_no_perms() {
        let mut conn = crate::database::get_test_connection().await;

        let (client, mut rx) = MockRepoClient::new(MockMergeWorkflow::default());

        let client = client.with_config(GitHubBrawlRepoConfig {
            try_permissions: Some(vec![]),
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            ..Default::default()
        };

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
                DryRunCommand {
                    head_sha: None,
                    base_sha: None,
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
                assert_eq!(user_id, UserId(3));
                assert_eq!(permissions, vec![]);
                result.send(Ok(false)).unwrap();
            }
            _ => panic!("unexpected action"),
        }

        let (mut conn, client) = task.await.unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_none());
    }

    #[tokio::test]
    async fn test_dry_run_not_enabled() {
        let mut conn = crate::database::get_test_connection().await;

        let (client, _) = MockRepoClient::new(MockMergeWorkflow::default());

        let client = client.with_config(GitHubBrawlRepoConfig {
            enabled: false,
            ..Default::default()
        });

        let pr = PullRequest {
            number: 1,
            ..Default::default()
        };

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
            DryRunCommand {
                head_sha: None,
                base_sha: None,
            },
        )
        .await
        .unwrap();

        let run = CiRun::active(client.id(), 1).get_result(&mut conn).await.optional().unwrap();

        assert!(run.is_none());
    }
}
