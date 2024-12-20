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
use octocrab::models::pulls::PullRequest;
use octocrab::models::RepositoryId;

use super::enums::GithubCiRunStatus;

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
            .order(super::schema::github_ci_runs::created_at.desc())
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

#[derive(Debug, Clone, AsExpression, FromSqlRow)]
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
    #[builder(default = 5)]
    pub priority: i32,
    pub requested_by_id: i64,
    pub is_dry_run: bool,
}

#[derive(Queryable, Selectable)]
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
