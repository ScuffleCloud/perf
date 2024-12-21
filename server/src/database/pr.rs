use std::borrow::Cow;

use diesel::pg::Pg;
use diesel::prelude::{AsChangeset, Identifiable, Insertable, Queryable};
use diesel::query_builder::QueryFragment;
use diesel::query_dsl::methods::{FindDsl, SelectDsl};
use diesel::{Selectable, SelectableHelper};
use diesel_async::methods::{ExecuteDsl, LoadQuery};
use diesel_async::AsyncPgConnection;
use octocrab::models::pulls::MergeableState;
use octocrab::models::{IssueState, RepositoryId, UserId};

use super::enums::{GithubPrMergeStatus, GithubPrStatus};
use crate::github::models::PullRequest;

#[derive(Insertable, Selectable, Queryable, Clone, AsChangeset)]
#[diesel(table_name = super::schema::github_pr)]
#[diesel(primary_key(github_repo_id, github_pr_number))]
pub struct Pr<'a> {
    pub github_repo_id: i64,
    pub github_pr_number: i32,
    pub title: Cow<'a, str>,
    pub body: Cow<'a, str>,
    pub merge_status: GithubPrMergeStatus,
    pub author_id: i64,
    pub assigned_ids: Vec<i64>,
    pub status: GithubPrStatus,
    pub default_priority: Option<i32>,
    pub merge_commit_sha: Option<Cow<'a, str>>,
    pub target_branch: Cow<'a, str>,
    pub source_branch: Cow<'a, str>,
    pub latest_commit_sha: Cow<'a, str>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(AsChangeset, Identifiable, Clone, bon::Builder)]
#[diesel(table_name = super::schema::github_pr)]
#[diesel(primary_key(github_repo_id, github_pr_number))]
pub struct UpdatePr<'a> {
    #[builder(start_fn)]
    pub github_repo_id: i64,
    #[builder(start_fn)]
    pub github_pr_number: i32,
    pub title: Option<Cow<'a, str>>,
    pub body: Option<Cow<'a, str>>,
    pub merge_status: Option<GithubPrMergeStatus>,
    pub assigned_ids: Option<Vec<i64>>,
    pub status: Option<GithubPrStatus>,
    pub default_priority: Option<Option<i32>>,
    pub merge_commit_sha: Option<Option<Cow<'a, str>>>,
    pub target_branch: Option<Cow<'a, str>>,
    pub latest_commit_sha: Option<Cow<'a, str>>,
    #[builder(default = chrono::Utc::now())]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// For testing purposes, we need to set the updated_at field to a specific
/// value. To make sure the snapshot tests are stable.
#[cfg(test)]
impl UpdatePr<'_> {
    pub fn with_updated_at(mut self, updated_at: chrono::DateTime<chrono::Utc>) -> Self {
        self.updated_at = updated_at;
        self
    }
}

fn pr_status(pr: &PullRequest) -> GithubPrStatus {
    match pr.state {
        Some(IssueState::Open) if matches!(pr.draft, Some(true)) => GithubPrStatus::Draft,
        Some(IssueState::Open) => GithubPrStatus::Open,
        Some(IssueState::Closed) => GithubPrStatus::Closed,
        _ => GithubPrStatus::Open,
    }
}

fn pr_merge_status(pr: &PullRequest) -> GithubPrMergeStatus {
    match pr.mergeable_state {
        _ if pr.merged_at.is_some() => GithubPrMergeStatus::Merged,
        Some(MergeableState::Behind | MergeableState::Blocked | MergeableState::Clean) => GithubPrMergeStatus::Ready,
        Some(MergeableState::Dirty) => GithubPrMergeStatus::Conflict,
        Some(MergeableState::Unstable) => GithubPrMergeStatus::CheckFailure,
        Some(MergeableState::Unknown) => GithubPrMergeStatus::NotReady,
        Some(MergeableState::Draft) => GithubPrMergeStatus::NotReady,
        _ => GithubPrMergeStatus::NotReady,
    }
}

fn pr_assigned_ids(pr: &PullRequest) -> Vec<i64> {
    let mut ids = pr.assignees.iter().map(|assignee| assignee.id.0 as i64).collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

impl<'a> Pr<'a> {
    /// Create a new PR from a PullRequest from the GitHub API.
    pub fn new(pr: &'a PullRequest, user_id: UserId, repo_id: RepositoryId) -> Self {
        Self {
            github_repo_id: repo_id.0 as i64,
            github_pr_number: pr.number as i32,
            title: Cow::Borrowed(&pr.title),
            body: Cow::Borrowed(&pr.body),
            merge_status: pr_merge_status(pr),
            author_id: pr.user.as_ref().map(|u| u.id.0 as i64).unwrap_or(user_id.0 as i64),
            assigned_ids: pr_assigned_ids(pr),
            status: pr_status(pr),
            default_priority: None,
            merge_commit_sha: pr.merged_at.and_then(|_| pr.merge_commit_sha.as_deref().map(Cow::Borrowed)),
            target_branch: Cow::Borrowed(&pr.base.ref_field),
            source_branch: Cow::Borrowed(&pr.head.ref_field),
            latest_commit_sha: Cow::Borrowed(&pr.head.sha),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    pub fn find(
        repo_id: RepositoryId,
        pr_number: u64,
    ) -> impl LoadQuery<'static, AsyncPgConnection, Pr<'static>> + QueryFragment<Pg> {
        super::schema::github_pr::dsl::github_pr
            .find((repo_id.0 as i64, pr_number as i32))
            .select(Pr::as_select())
    }

    pub fn upsert(&'a self) -> impl LoadQuery<'a, AsyncPgConnection, Pr<'static>> + QueryFragment<Pg> {
        diesel::insert_into(super::schema::github_pr::dsl::github_pr)
            .values(self)
            .on_conflict((
                super::schema::github_pr::dsl::github_repo_id,
                super::schema::github_pr::dsl::github_pr_number,
            ))
            .do_update()
            .set(UpdatePr {
                github_repo_id: self.github_repo_id,
                github_pr_number: self.github_pr_number,
                title: Some(Cow::Borrowed(self.title.as_ref())),
                body: Some(Cow::Borrowed(self.body.as_ref())),
                merge_status: Some(self.merge_status),
                assigned_ids: Some(self.assigned_ids.clone()),
                status: Some(self.status),
                default_priority: Some(self.default_priority),
                merge_commit_sha: Some(self.merge_commit_sha.as_deref().map(Cow::Borrowed)),
                target_branch: Some(Cow::Borrowed(self.target_branch.as_ref())),
                latest_commit_sha: Some(Cow::Borrowed(self.latest_commit_sha.as_ref())),
                updated_at: self.updated_at,
            })
            .returning(Pr::as_select())
    }

    pub fn insert(&'a self) -> impl ExecuteDsl<AsyncPgConnection> + 'a + QueryFragment<Pg> {
        diesel::insert_into(super::schema::github_pr::dsl::github_pr).values(self)
    }

    /// Create an update for this PR given a PullRequest from the GitHub API.
    pub fn update(&'a self) -> UpdatePrBuilder<'a> {
        UpdatePr::builder(self.github_repo_id, self.github_pr_number)
    }

    pub fn update_from(&'a self, new: &'a PullRequest) -> UpdatePr<'a> {
        let title = Cow::Borrowed(new.title.as_str());
        let body = Cow::Borrowed(new.body.as_str());
        let merge_status = pr_merge_status(new);
        let status = pr_status(new);
        let assigned_ids = pr_assigned_ids(new);

        let merge_commit_sha = new.merged_at.and(new.merge_commit_sha.as_deref()).map(Cow::Borrowed);

        UpdatePr::builder(self.github_repo_id, self.github_pr_number)
            .maybe_title(self.title.ne(&title).then_some(title))
            .maybe_body(self.body.ne(&body).then_some(body))
            .maybe_merge_status(self.merge_status.ne(&merge_status).then_some(merge_status))
            .maybe_assigned_ids(self.assigned_ids.ne(&assigned_ids).then_some(assigned_ids))
            .maybe_status(self.status.ne(&status).then_some(status))
            .maybe_merge_commit_sha(self.merge_commit_sha.ne(&merge_commit_sha).then_some(merge_commit_sha))
            .maybe_target_branch(
                self.target_branch
                    .ne(&new.base.ref_field)
                    .then_some(Cow::Borrowed(&new.base.ref_field)),
            )
            .maybe_latest_commit_sha(
                self.latest_commit_sha
                    .ne(&new.head.sha)
                    .then_some(Cow::Borrowed(&new.head.sha)),
            )
            .build()
    }
}

impl<'a> UpdatePr<'a> {
    /// Check if this update needs to be applied to the database.
    pub fn needs_update(&self) -> bool {
        self.title.is_some()
            || self.body.is_some()
            || self.merge_status.is_some()
            || self.assigned_ids.is_some()
            || self.status.is_some()
            || self.default_priority.is_some()
            || self.merge_commit_sha.is_some()
            || self.target_branch.is_some()
            || self.latest_commit_sha.is_some()
    }

    #[inline]
    pub fn query(&'a self) -> impl ExecuteDsl<AsyncPgConnection> + 'a + QueryFragment<Pg> {
        diesel::update(self).set(self)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::borrow::Cow;

    use diesel_async::RunQueryDsl;

    use super::*;
    use crate::database::test_query;

    test_query! {
        name: test_find_query,
        query: Pr::find(RepositoryId(1), 1),
        expected: @r#"
    SELECT
      "github_pr"."github_repo_id",
      "github_pr"."github_pr_number",
      "github_pr"."title",
      "github_pr"."body",
      "github_pr"."merge_status",
      "github_pr"."author_id",
      "github_pr"."assigned_ids",
      "github_pr"."status",
      "github_pr"."default_priority",
      "github_pr"."merge_commit_sha",
      "github_pr"."target_branch",
      "github_pr"."source_branch",
      "github_pr"."latest_commit_sha",
      "github_pr"."created_at",
      "github_pr"."updated_at"
    FROM
      "github_pr"
    WHERE
      (
        ("github_pr"."github_repo_id" = $1)
        AND ("github_pr"."github_pr_number" = $2)
      ) -- binds: [1, 1]
    "#,
    }

    test_query! {
        name: test_insert_query,
        query: Pr::insert(&Pr {
            github_repo_id: 1,
            github_pr_number: 1,
            title: Cow::Borrowed("test"),
            body: Cow::Borrowed("test"),
            created_at: chrono::DateTime::from_timestamp_nanos(1718851200000000000),
            updated_at: chrono::DateTime::from_timestamp_nanos(1718851200000000000),
            assigned_ids: vec![],
            author_id: 0,
            default_priority: None,
            latest_commit_sha: Cow::Borrowed("test"),
            source_branch: Cow::Borrowed("test"),
            target_branch: Cow::Borrowed("test"),
            merge_status: GithubPrMergeStatus::NotReady,
            status: GithubPrStatus::Open,
            merge_commit_sha: None,
        }),
        expected: @r#"
    INSERT INTO
      "github_pr" (
        "github_repo_id",
        "github_pr_number",
        "title",
        "body",
        "merge_status",
        "author_id",
        "assigned_ids",
        "status",
        "default_priority",
        "merge_commit_sha",
        "target_branch",
        "source_branch",
        "latest_commit_sha",
        "created_at",
        "updated_at"
      )
    VALUES
      (
        $1,
        $2,
        $3,
        $4,
        $5,
        $6,
        $7,
        $8,
        DEFAULT,
        DEFAULT,
        $9,
        $10,
        $11,
        $12,
        $13
      ) -- binds: [1, 1, "test", "test", NotReady, 0, [], Open, "test", "test", "test", 2024-06-20T02:40:00Z, 2024-06-20T02:40:00Z]
    "#,
    }

    test_query! {
        name: test_update_query,
        query: UpdatePr::builder(1, 1)
            .title(Cow::Borrowed("test"))
            .body(Cow::Borrowed("test"))
            .updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
            .build()
            .query(),
        expected: @r#"
    UPDATE
      "github_pr"
    SET
      "title" = $1,
      "body" = $2,
      "updated_at" = $3
    WHERE
      (
        ("github_pr"."github_repo_id" = $4)
        AND ("github_pr"."github_pr_number" = $5)
      ) -- binds: ["test", "test", 2024-06-20T02:40:00Z, 1, 1]
    "#,
    }

    test_query! {
        name: test_update_query_no_changes,
        query: UpdatePr::builder(1, 1)
            .updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
            .build()
            .query(),
        expected: @r#"
    UPDATE
      "github_pr"
    SET
      "updated_at" = $1
    WHERE
      (
        ("github_pr"."github_repo_id" = $2)
        AND ("github_pr"."github_pr_number" = $3)
      ) -- binds: [2024-06-20T02:40:00Z, 1, 1]
    "#,
    }

    test_query! {
        name: test_upsert_query,
        query: Pr::upsert(&Pr {
            github_repo_id: 1,
            github_pr_number: 1,
            title: Cow::Borrowed("test"),
            body: Cow::Borrowed("test"),
            created_at: chrono::DateTime::from_timestamp_nanos(1718851200000000000),
            updated_at: chrono::DateTime::from_timestamp_nanos(1718851200000000000),
            assigned_ids: vec![],
            author_id: 0,
            default_priority: None,
            latest_commit_sha: Cow::Borrowed("test"),
            source_branch: Cow::Borrowed("test"),
            target_branch: Cow::Borrowed("test"),
            merge_status: GithubPrMergeStatus::NotReady,
            status: GithubPrStatus::Open,
            merge_commit_sha: None,
        }),
        expected: @r#"
    INSERT INTO
      "github_pr" (
        "github_repo_id",
        "github_pr_number",
        "title",
        "body",
        "merge_status",
        "author_id",
        "assigned_ids",
        "status",
        "default_priority",
        "merge_commit_sha",
        "target_branch",
        "source_branch",
        "latest_commit_sha",
        "created_at",
        "updated_at"
      )
    VALUES
      (
        $1,
        $2,
        $3,
        $4,
        $5,
        $6,
        $7,
        $8,
        DEFAULT,
        DEFAULT,
        $9,
        $10,
        $11,
        $12,
        $13
      ) ON CONFLICT ("github_repo_id", "github_pr_number") DO
    UPDATE
    SET
      "title" = $14,
      "body" = $15,
      "merge_status" = $16,
      "assigned_ids" = $17,
      "status" = $18,
      "default_priority" = $19,
      "merge_commit_sha" = $20,
      "target_branch" = $21,
      "latest_commit_sha" = $22,
      "updated_at" = $23
    RETURNING
      "github_pr"."github_repo_id",
      "github_pr"."github_pr_number",
      "github_pr"."title",
      "github_pr"."body",
      "github_pr"."merge_status",
      "github_pr"."author_id",
      "github_pr"."assigned_ids",
      "github_pr"."status",
      "github_pr"."default_priority",
      "github_pr"."merge_commit_sha",
      "github_pr"."target_branch",
      "github_pr"."source_branch",
      "github_pr"."latest_commit_sha",
      "github_pr"."created_at",
      "github_pr"."updated_at" -- binds: [1, 1, "test", "test", NotReady, 0, [], Open, "test", "test", "test", 2024-06-20T02:40:00Z, 2024-06-20T02:40:00Z, "test", "test", NotReady, [], Open, None, None, "test", "test", 2024-06-20T02:40:00Z]
    "#,
    }

    test_query! {
        name: test_update_from_pr_query,
        query: Pr::update_from(
                &Pr::new(
                    &PullRequest {
                        number: 1,
                        title: "test".into(),
                        body: "test".into(),
                        ..Default::default()
                    },
                    UserId(0),
                    RepositoryId(1),
                ),
                &PullRequest {
                    number: 1,
                    title: "test".into(),
                    body: "test2".into(),
                    ..Default::default()
                },
            )
            .with_updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
            .query(),
        expected: @r#"
    UPDATE
      "github_pr"
    SET
      "body" = $1,
      "updated_at" = $2
    WHERE
      (
        ("github_pr"."github_repo_id" = $3)
        AND ("github_pr"."github_pr_number" = $4)
      ) -- binds: ["test2", 2024-06-20T02:40:00Z, 1, 1]
    "#,
    }

    #[test]
    fn test_update_needs_update() {
        let mut update = UpdatePr::builder(1, 1)
            .updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
            .build();

        assert!(!update.needs_update());

        macro_rules! test_update {
            ($field:ident, $value:expr) => {
                update.$field = Some($value);
                assert!(update.needs_update());
                update.$field = None;
                assert!(!update.needs_update());
            };
        }

        test_update!(title, Cow::Borrowed("test"));
        test_update!(body, Cow::Borrowed("test"));
        test_update!(merge_status, GithubPrMergeStatus::Ready);
        test_update!(assigned_ids, vec![1, 2, 3]);
        test_update!(status, GithubPrStatus::Closed);
        test_update!(default_priority, Some(1));
        test_update!(merge_commit_sha, Some(Cow::Borrowed("test")));
        test_update!(default_priority, None);
        test_update!(merge_commit_sha, None);
        test_update!(target_branch, Cow::Borrowed("test"));
        test_update!(latest_commit_sha, Cow::Borrowed("test"));
    }

    #[tokio::test]
    async fn test_integration_insert() {
        let mut conn = crate::database::get_test_connection().await;

        Pr {
            github_repo_id: 1,
            github_pr_number: 1,
            body: Cow::Borrowed("test2"),
            latest_commit_sha: Cow::Borrowed("test"),
            source_branch: Cow::Borrowed("test"),
            target_branch: Cow::Borrowed("test"),
            title: Cow::Borrowed("test"),
            assigned_ids: vec![],
            author_id: 0,
            default_priority: None,
            merge_status: GithubPrMergeStatus::NotReady,
            status: GithubPrStatus::Open,
            merge_commit_sha: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        let pr = Pr::find(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.body, "test2");
    }

    #[tokio::test]
    async fn test_integration_update() {
        let mut conn = crate::database::get_test_connection().await;

        Pr {
            github_repo_id: 1,
            github_pr_number: 1,
            body: Cow::Borrowed("test2"),
            latest_commit_sha: Cow::Borrowed("test"),
            source_branch: Cow::Borrowed("test"),
            target_branch: Cow::Borrowed("test"),
            title: Cow::Borrowed("test"),
            assigned_ids: vec![],
            author_id: 0,
            default_priority: None,
            merge_status: GithubPrMergeStatus::NotReady,
            status: GithubPrStatus::Open,
            merge_commit_sha: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        UpdatePr::builder(1, 1)
            .body(Cow::Borrowed("test"))
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let pr = Pr::find(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.body, "test");
    }

    #[tokio::test]
    async fn test_integration_find() {
        let mut conn = crate::database::get_test_connection().await;

        Pr {
            github_repo_id: 1,
            github_pr_number: 1,
            body: Cow::Borrowed("test3"),
            latest_commit_sha: Cow::Borrowed("test"),
            source_branch: Cow::Borrowed("test"),
            target_branch: Cow::Borrowed("test"),
            title: Cow::Borrowed("test"),
            assigned_ids: vec![],
            author_id: 0,
            default_priority: None,
            merge_status: GithubPrMergeStatus::NotReady,
            status: GithubPrStatus::Open,
            merge_commit_sha: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
        .insert()
        .execute(&mut conn)
        .await
        .unwrap();

        let pr = Pr::find(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.github_repo_id, 1);
        assert_eq!(pr.github_pr_number, 1);
        assert_eq!(pr.body, "test3");
    }

    #[test]
    fn test_pr_status_from_pr() {
        let cases = [
            (IssueState::Open, None, GithubPrStatus::Open),
            (IssueState::Open, Some(true), GithubPrStatus::Draft),
            (IssueState::Closed, None, GithubPrStatus::Closed),
            (IssueState::Open, Some(false), GithubPrStatus::Open),
            (IssueState::Closed, Some(true), GithubPrStatus::Closed),
            (IssueState::Closed, Some(false), GithubPrStatus::Closed),
        ];

        for (state, draft, status) in cases {
            assert_eq!(
                pr_status(&PullRequest {
                    state: Some(state),
                    draft,
                    ..Default::default()
                }),
                status
            );
        }
    }

    #[test]
    fn test_pr_merge_status_from_pr() {
        let cases = [
            (MergeableState::Behind, GithubPrMergeStatus::Ready),
            (MergeableState::Blocked, GithubPrMergeStatus::Ready),
            (MergeableState::Clean, GithubPrMergeStatus::Ready),
            (MergeableState::Dirty, GithubPrMergeStatus::Conflict),
            (MergeableState::Unstable, GithubPrMergeStatus::CheckFailure),
            (MergeableState::Unknown, GithubPrMergeStatus::NotReady),
            (MergeableState::Draft, GithubPrMergeStatus::NotReady),
        ];

        for (state, status) in cases {
            assert_eq!(
                pr_merge_status(&PullRequest {
                    mergeable_state: Some(state),
                    ..Default::default()
                }),
                status
            );
        }

        assert_eq!(
            pr_merge_status(&PullRequest {
                merged_at: Some(chrono::Utc::now()),
                ..Default::default()
            }),
            GithubPrMergeStatus::Merged
        );
    }

    #[tokio::test]
    async fn test_update() {
        let mut conn = crate::database::get_test_connection().await;

        let pr = Pr::new(
            &PullRequest {
                number: 1,
                ..Default::default()
            },
            UserId(0),
            RepositoryId(1),
        )
        .upsert()
        .get_result(&mut conn)
        .await
        .unwrap();

        pr.update()
            .body("test2".into())
            .build()
            .query()
            .execute(&mut conn)
            .await
            .unwrap();

        let pr = Pr::find(RepositoryId(1), 1).get_result(&mut conn).await.unwrap();
        assert_eq!(pr.body, "test2");
    }
}
