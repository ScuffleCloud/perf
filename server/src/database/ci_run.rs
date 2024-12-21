use std::borrow::Cow;

use diesel::deserialize::FromSqlRow;
use diesel::expression::AsExpression;
use diesel::pg::Pg;
use diesel::prelude::{AsChangeset, Identifiable, Insertable, Queryable, Selectable};
use diesel::query_builder::QueryFragment;
use diesel::query_dsl::methods::{FilterDsl, LimitDsl, OrderDsl, SelectDsl};
use diesel::{ExpressionMethods, SelectableHelper};
use diesel_async::methods::{ExecuteDsl, LoadQuery};
use diesel_async::AsyncPgConnection;
use octocrab::models::RepositoryId;

use super::enums::GithubCiRunStatus;
use crate::github::models::PullRequest;

impl CiRun<'_> {
    pub fn active(
        repo_id: RepositoryId,
        pr_number: u64,
    ) -> impl LoadQuery<'static, AsyncPgConnection, CiRun<'static>> + QueryFragment<Pg> {
        super::schema::github_ci_runs::dsl::github_ci_runs
            .filter(super::schema::github_ci_runs::github_repo_id.eq(repo_id.0 as i64))
            .filter(super::schema::github_ci_runs::github_pr_number.eq(pr_number as i32))
            .filter(super::schema::github_ci_runs::completed_at.is_null())
            .select(CiRun::as_select())
    }

    pub fn latest(
        repo_id: RepositoryId,
        pr_number: u64,
    ) -> impl LoadQuery<'static, AsyncPgConnection, CiRun<'static>> + QueryFragment<Pg> {
        super::schema::github_ci_runs::dsl::github_ci_runs
            .filter(super::schema::github_ci_runs::github_repo_id.eq(repo_id.0 as i64))
            .filter(super::schema::github_ci_runs::github_pr_number.eq(pr_number as i32))
            .order(super::schema::github_ci_runs::id.desc())
            .limit(1)
            .select(CiRun::as_select())
    }

    pub fn by_run_commit_sha(
        run_commit_sha: &str,
    ) -> impl LoadQuery<'_, AsyncPgConnection, CiRun<'static>> + QueryFragment<Pg> {
        super::schema::github_ci_runs::dsl::github_ci_runs
            .filter(super::schema::github_ci_runs::run_commit_sha.eq(run_commit_sha))
            .select(CiRun::as_select())
    }

    pub fn pending() -> impl LoadQuery<'static, AsyncPgConnection, CiRun<'static>> + QueryFragment<Pg> {
        super::schema::github_ci_runs::dsl::github_ci_runs
            .filter(super::schema::github_ci_runs::completed_at.is_null())
            .select(CiRun::as_select())
    }

    pub fn update(id: i32) -> UpdateCiRunBuilder<'static> {
        UpdateCiRun::builder(id)
    }

    pub fn insert(repo_id: RepositoryId, pr_number: u64) -> InsertCiRunBuilder<'static> {
        InsertCiRun::builder(repo_id.0 as i64, pr_number as i32)
    }
}

impl<'a> InsertCiRun<'a> {
    pub fn query(&'a self) -> impl LoadQuery<'a, AsyncPgConnection, CiRun<'static>> + QueryFragment<Pg> {
        diesel::insert_into(super::schema::github_ci_runs::dsl::github_ci_runs)
            .values(self)
            .returning(CiRun::as_select())
    }
}

impl<'a> UpdateCiRun<'a> {
    pub fn query(&'a self) -> impl ExecuteDsl<AsyncPgConnection> + QueryFragment<Pg> + 'a {
        diesel::update(self).set(self)
    }

    pub fn not_done(&'a self) -> impl ExecuteDsl<AsyncPgConnection> + QueryFragment<Pg> + 'a {
        diesel::update(self)
            .filter(super::schema::github_ci_runs::completed_at.is_null())
            .set(self)
    }

    pub fn queued(&'a self) -> impl ExecuteDsl<AsyncPgConnection> + QueryFragment<Pg> + 'a {
        diesel::update(self)
            .filter(super::schema::github_ci_runs::status.eq(GithubCiRunStatus::Queued))
            .set(self)
    }
}

#[derive(Debug, Clone, AsExpression, FromSqlRow, PartialEq, Eq)]
#[diesel(sql_type = diesel::sql_types::Text)]
pub enum Base<'a> {
    Commit(Cow<'a, str>),
    Branch(Cow<'a, str>),
}

impl<'a> Base<'a> {
    pub fn from_pr(pr: &'a PullRequest) -> Self {
        Self::Branch(Cow::Borrowed(&pr.base.ref_field))
    }

    pub const fn from_sha(sha: &'a str) -> Self {
        Self::Commit(Cow::Borrowed(sha))
    }

    pub fn from_string(s: &'a str) -> Option<Self> {
        if let Some(sha) = s.strip_prefix("commit:") {
            return Some(Self::Commit(Cow::Borrowed(sha)));
        }

        if let Some(branch) = s.strip_prefix("branch:") {
            return Some(Self::Branch(Cow::Borrowed(branch)));
        }

        None
    }

    pub fn to_owned(&self) -> Base<'static> {
        match self {
            Base::Commit(sha) => Base::Commit(Cow::Owned(sha.to_string())),
            Base::Branch(branch) => Base::Branch(Cow::Owned(branch.to_string())),
        }
    }
}

impl diesel::serialize::ToSql<diesel::sql_types::Text, diesel::pg::Pg> for Base<'_> {
    fn to_sql(&self, out: &mut diesel::serialize::Output<diesel::pg::Pg>) -> diesel::serialize::Result {
        use std::io::Write;

        match self {
            Base::Commit(sha) => write!(out, "commit:{sha}")?,
            Base::Branch(branch) => write!(out, "branch:{branch}")?,
        }

        Ok(diesel::serialize::IsNull::No)
    }
}

impl diesel::deserialize::FromSql<diesel::sql_types::Text, diesel::pg::Pg> for Base<'_> {
    fn from_sql(bytes: <diesel::pg::Pg as diesel::backend::Backend>::RawValue<'_>) -> diesel::deserialize::Result<Self> {
        let s = std::str::from_utf8(bytes.as_bytes())?;
        let base = Base::from_string(s).ok_or_else(|| format!("invalid base: {s}"))?;
        Ok(base.to_owned())
    }
}

#[derive(Insertable, bon::Builder)]
#[diesel(table_name = super::schema::github_ci_runs)]
pub struct InsertCiRun<'a> {
    #[builder(start_fn)]
    pub github_repo_id: i64,
    #[builder(start_fn)]
    pub github_pr_number: i32,
    pub base_ref: Base<'a>,
    pub head_commit_sha: Cow<'a, str>,
    pub run_commit_sha: Option<Cow<'a, str>>,
    pub ci_branch: Cow<'a, str>,
    pub approved_by_ids: Vec<i64>,
    #[builder(default = 5)]
    pub priority: i32,
    pub requested_by_id: i64,
    pub is_dry_run: bool,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = super::schema::github_ci_runs)]
pub struct CiRun<'a> {
    pub id: i32,
    pub github_repo_id: i64,
    pub github_pr_number: i32,
    pub status: GithubCiRunStatus,
    pub base_ref: Base<'a>,
    pub head_commit_sha: Cow<'a, str>,
    pub run_commit_sha: Option<Cow<'a, str>>,
    pub ci_branch: Cow<'a, str>,
    pub priority: i32,
    pub requested_by_id: i64,
    pub is_dry_run: bool,
    pub approved_by_ids: Vec<i64>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Identifiable, AsChangeset, bon::Builder)]
#[diesel(table_name = super::schema::github_ci_runs)]
#[diesel(primary_key(id))]
pub struct UpdateCiRun<'a> {
    #[builder(start_fn)]
    pub id: i32,
    pub status: Option<GithubCiRunStatus>,
    pub run_commit_sha: Option<Cow<'a, str>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[builder(default = chrono::Utc::now())]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::database::test_query;
    use crate::github::models::PrBranch;

    test_query!(
        name: test_insert_commit,
        query: CiRun::insert(RepositoryId(1), 1)
                .approved_by_ids(vec![1, 2, 3])
                .base_ref(Base::Commit("sha".into()))
                .ci_branch("branch".into())
                .head_commit_sha("sha".into())
                .priority(5)
                .requested_by_id(1)
                .is_dry_run(true)
                .build()
                .query(),
        expected: @r#"
    INSERT INTO
      "github_ci_runs" (
        "github_repo_id",
        "github_pr_number",
        "base_ref",
        "head_commit_sha",
        "run_commit_sha",
        "ci_branch",
        "approved_by_ids",
        "priority",
        "requested_by_id",
        "is_dry_run"
      )
    VALUES
      ($1, $2, $3, $4, DEFAULT, $5, $6, $7, $8, $9)
    RETURNING
      "github_ci_runs"."id",
      "github_ci_runs"."github_repo_id",
      "github_ci_runs"."github_pr_number",
      "github_ci_runs"."status",
      "github_ci_runs"."base_ref",
      "github_ci_runs"."head_commit_sha",
      "github_ci_runs"."run_commit_sha",
      "github_ci_runs"."ci_branch",
      "github_ci_runs"."priority",
      "github_ci_runs"."requested_by_id",
      "github_ci_runs"."is_dry_run",
      "github_ci_runs"."approved_by_ids",
      "github_ci_runs"."completed_at",
      "github_ci_runs"."started_at",
      "github_ci_runs"."created_at",
      "github_ci_runs"."updated_at" -- binds: [1, 1, Commit("sha"), "sha", "branch", [1, 2, 3], 5, 1, true]
    "#,
    );

    test_query!(
        name: test_insert_branch,
        query: CiRun::insert(RepositoryId(1), 1)
                .approved_by_ids(vec![1, 2, 3])
                .base_ref(Base::Branch("branch".into()))
                .ci_branch("branch".into())
                .head_commit_sha("sha".into())
                .priority(5)
                .requested_by_id(1)
                .is_dry_run(true)
                .build()
                .query(),
        expected: @r#"
    INSERT INTO
      "github_ci_runs" (
        "github_repo_id",
        "github_pr_number",
        "base_ref",
        "head_commit_sha",
        "run_commit_sha",
        "ci_branch",
        "approved_by_ids",
        "priority",
        "requested_by_id",
        "is_dry_run"
      )
    VALUES
      ($1, $2, $3, $4, DEFAULT, $5, $6, $7, $8, $9)
    RETURNING
      "github_ci_runs"."id",
      "github_ci_runs"."github_repo_id",
      "github_ci_runs"."github_pr_number",
      "github_ci_runs"."status",
      "github_ci_runs"."base_ref",
      "github_ci_runs"."head_commit_sha",
      "github_ci_runs"."run_commit_sha",
      "github_ci_runs"."ci_branch",
      "github_ci_runs"."priority",
      "github_ci_runs"."requested_by_id",
      "github_ci_runs"."is_dry_run",
      "github_ci_runs"."approved_by_ids",
      "github_ci_runs"."completed_at",
      "github_ci_runs"."started_at",
      "github_ci_runs"."created_at",
      "github_ci_runs"."updated_at" -- binds: [1, 1, Branch("branch"), "sha", "branch", [1, 2, 3], 5, 1, true]
    "#,
    );

    test_query!(
        name: test_update_failure,
        query: CiRun::update(1)
                .completed_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .started_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .status(GithubCiRunStatus::Failure)
                .updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .build()
                .query(),
        expected: @r#"
    UPDATE
      "github_ci_runs"
    SET
      "status" = $1,
      "completed_at" = $2,
      "started_at" = $3,
      "updated_at" = $4
    WHERE
      ("github_ci_runs"."id" = $5) -- binds: [Failure, 2024-06-20T02:40:00Z, 2024-06-20T02:40:00Z, 2024-06-20T02:40:00Z, 1]
    "#,
    );

    test_query!(
        name: test_update_success,
        query: CiRun::update(1)
                .completed_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .status(GithubCiRunStatus::Success)
                .updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .build()
                .not_done(),
        expected: @r#"
    UPDATE
      "github_ci_runs"
    SET
      "status" = $1,
      "completed_at" = $2,
      "updated_at" = $3
    WHERE
      (
        ("github_ci_runs"."id" = $4)
        AND ("github_ci_runs"."completed_at" IS NULL)
      ) -- binds: [Success, 2024-06-20T02:40:00Z, 2024-06-20T02:40:00Z, 1]
    "#,
    );

    test_query!(
        name: test_update_in_progress,
        query: CiRun::update(1)
                .started_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .status(GithubCiRunStatus::InProgress)
                .updated_at(chrono::DateTime::from_timestamp_nanos(1718851200000000000))
                .build()
                .queued(),
        expected: @r#"
    UPDATE
      "github_ci_runs"
    SET
      "status" = $1,
      "started_at" = $2,
      "updated_at" = $3
    WHERE
      (
        ("github_ci_runs"."id" = $4)
        AND ("github_ci_runs"."status" = $5)
      ) -- binds: [InProgress, 2024-06-20T02:40:00Z, 2024-06-20T02:40:00Z, 1, Queued]
    "#,
    );

    test_query!(
        name: test_active,
        query: CiRun::active(RepositoryId(1), 1),
        expected: @r#"
    SELECT
      "github_ci_runs"."id",
      "github_ci_runs"."github_repo_id",
      "github_ci_runs"."github_pr_number",
      "github_ci_runs"."status",
      "github_ci_runs"."base_ref",
      "github_ci_runs"."head_commit_sha",
      "github_ci_runs"."run_commit_sha",
      "github_ci_runs"."ci_branch",
      "github_ci_runs"."priority",
      "github_ci_runs"."requested_by_id",
      "github_ci_runs"."is_dry_run",
      "github_ci_runs"."approved_by_ids",
      "github_ci_runs"."completed_at",
      "github_ci_runs"."started_at",
      "github_ci_runs"."created_at",
      "github_ci_runs"."updated_at"
    FROM
      "github_ci_runs"
    WHERE
      (
        (
          ("github_ci_runs"."github_repo_id" = $1)
          AND ("github_ci_runs"."github_pr_number" = $2)
        )
        AND ("github_ci_runs"."completed_at" IS NULL)
      ) -- binds: [1, 1]
    "#,
    );

    test_query!(
        name: test_latest,
        query: CiRun::latest(RepositoryId(1), 1),
        expected: @r#"
    SELECT
      "github_ci_runs"."id",
      "github_ci_runs"."github_repo_id",
      "github_ci_runs"."github_pr_number",
      "github_ci_runs"."status",
      "github_ci_runs"."base_ref",
      "github_ci_runs"."head_commit_sha",
      "github_ci_runs"."run_commit_sha",
      "github_ci_runs"."ci_branch",
      "github_ci_runs"."priority",
      "github_ci_runs"."requested_by_id",
      "github_ci_runs"."is_dry_run",
      "github_ci_runs"."approved_by_ids",
      "github_ci_runs"."completed_at",
      "github_ci_runs"."started_at",
      "github_ci_runs"."created_at",
      "github_ci_runs"."updated_at"
    FROM
      "github_ci_runs"
    WHERE
      (
        ("github_ci_runs"."github_repo_id" = $1)
        AND ("github_ci_runs"."github_pr_number" = $2)
      )
    ORDER BY
      "github_ci_runs"."id" DESC
    LIMIT
      $3 -- binds: [1, 1, 1]
    "#,
    );

    test_query!(
        name: test_by_run_commit_sha,
        query: CiRun::by_run_commit_sha("sha"),
        expected: @r#"
    SELECT
      "github_ci_runs"."id",
      "github_ci_runs"."github_repo_id",
      "github_ci_runs"."github_pr_number",
      "github_ci_runs"."status",
      "github_ci_runs"."base_ref",
      "github_ci_runs"."head_commit_sha",
      "github_ci_runs"."run_commit_sha",
      "github_ci_runs"."ci_branch",
      "github_ci_runs"."priority",
      "github_ci_runs"."requested_by_id",
      "github_ci_runs"."is_dry_run",
      "github_ci_runs"."approved_by_ids",
      "github_ci_runs"."completed_at",
      "github_ci_runs"."started_at",
      "github_ci_runs"."created_at",
      "github_ci_runs"."updated_at"
    FROM
      "github_ci_runs"
    WHERE
      ("github_ci_runs"."run_commit_sha" = $1) -- binds: ["sha"]
    "#,
    );

    test_query!(
        name: test_pending,
        query: CiRun::pending(),
        expected: @r#"
    SELECT
      "github_ci_runs"."id",
      "github_ci_runs"."github_repo_id",
      "github_ci_runs"."github_pr_number",
      "github_ci_runs"."status",
      "github_ci_runs"."base_ref",
      "github_ci_runs"."head_commit_sha",
      "github_ci_runs"."run_commit_sha",
      "github_ci_runs"."ci_branch",
      "github_ci_runs"."priority",
      "github_ci_runs"."requested_by_id",
      "github_ci_runs"."is_dry_run",
      "github_ci_runs"."approved_by_ids",
      "github_ci_runs"."completed_at",
      "github_ci_runs"."started_at",
      "github_ci_runs"."created_at",
      "github_ci_runs"."updated_at"
    FROM
      "github_ci_runs"
    WHERE
      ("github_ci_runs"."completed_at" IS NULL) -- binds: []
    "#,
    );

    #[test]
    fn test_base_parse() {
        assert_eq!(Base::from_string("commit:sha"), Some(Base::Commit("sha".into())));
        assert_eq!(Base::from_string("branch:branch"), Some(Base::Branch("branch".into())));
        assert_eq!(Base::from_string("bad"), None);
        assert_eq!(Base::from_string("commit:sha:bad"), Some(Base::Commit("sha:bad".into())));
        assert_eq!(
            Base::from_string("branch:branch:bad"),
            Some(Base::Branch("branch:bad".into()))
        );
        assert_eq!(Base::from_sha("sha123:bad"), Base::Commit("sha123:bad".into()));
        assert_eq!(
            Base::from_pr(&PullRequest {
                base: PrBranch {
                    ref_field: "my-branch".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            Base::Branch("my-branch".into())
        );
    }

    #[test]
    fn test_base_to_owned() {
        assert!(matches!(Base::Commit("sha".into()).to_owned(), Base::Commit(Cow::Owned(s)) if s == "sha"));
        assert!(matches!(Base::Branch("branch".into()).to_owned(), Base::Branch(Cow::Owned(s)) if s == "branch"));
    }
}
