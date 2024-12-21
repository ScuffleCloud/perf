use std::borrow::Cow;

use chrono::{DateTime, Utc};
use diesel::pg::Pg;
use diesel::prelude::{AsChangeset, Insertable, Queryable};
use diesel::query_builder::QueryFragment;
use diesel::query_dsl::methods::{FilterDsl, SelectDsl};
use diesel::{ExpressionMethods, Selectable, SelectableHelper};
use diesel_async::methods::{ExecuteDsl, LoadQuery};
use diesel_async::AsyncPgConnection;

use super::enums::GithubCiRunStatusCheckStatus;
use crate::github::models::{CheckRunConclusion, CheckRunEvent};

#[derive(Debug, Insertable, Queryable, Selectable, AsChangeset)]
#[diesel(table_name = super::schema::github_ci_run_status_checks)]
pub struct CiCheck<'a> {
    pub id: i64,
    pub github_ci_run_id: i32,
    pub status_check_name: Cow<'a, str>,
    pub status_check_status: GithubCiRunStatusCheckStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub url: Cow<'a, str>,
    pub required: bool,
}

fn convert_status(conclusion: Option<&CheckRunConclusion>) -> GithubCiRunStatusCheckStatus {
    match conclusion {
        None => GithubCiRunStatusCheckStatus::Pending,
        Some(CheckRunConclusion::Success) => GithubCiRunStatusCheckStatus::Success,
        Some(CheckRunConclusion::TimedOut) | Some(CheckRunConclusion::Failure) => GithubCiRunStatusCheckStatus::Failure,
        Some(CheckRunConclusion::Cancelled) => GithubCiRunStatusCheckStatus::Cancelled,
        Some(CheckRunConclusion::Skipped) => GithubCiRunStatusCheckStatus::Skipped,
        Some(CheckRunConclusion::ActionRequired) => GithubCiRunStatusCheckStatus::Failure,
        Some(CheckRunConclusion::Neutral) => GithubCiRunStatusCheckStatus::Pending, // not sure what this is
    }
}

impl<'a> CiCheck<'a> {
    pub fn new(ci_check: &'a CheckRunEvent, ci_run_id: i32, required: bool) -> Self {
        Self {
            id: ci_check.id,
            github_ci_run_id: ci_run_id,
            status_check_name: Cow::Borrowed(&ci_check.name),
            status_check_status: convert_status(ci_check.conclusion.as_ref()),
            started_at: ci_check.started_at.unwrap_or(Utc::now()),
            completed_at: ci_check.completed_at,
            url: Cow::Borrowed(
                ci_check
                    .html_url
                    .as_deref()
                    .or(ci_check.details_url.as_deref())
                    .unwrap_or_else(|| ci_check.url.as_ref()),
            ),
            required,
        }
    }

    pub fn upsert(&'a self) -> impl ExecuteDsl<AsyncPgConnection> + QueryFragment<Pg> + 'a {
        diesel::insert_into(super::schema::github_ci_run_status_checks::dsl::github_ci_run_status_checks)
            .values(self)
            .on_conflict(super::schema::github_ci_run_status_checks::id)
            .do_update()
            .set(self)
    }

    pub fn for_run(run_id: i32) -> impl LoadQuery<'static, AsyncPgConnection, CiCheck<'static>> + QueryFragment<Pg> {
        super::schema::github_ci_run_status_checks::dsl::github_ci_run_status_checks
            .filter(super::schema::github_ci_run_status_checks::github_ci_run_id.eq(run_id))
            .select(CiCheck::as_select())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::database::test_query;
    use crate::github::models::CheckRunStatus;

    test_query! {
        name: test_upsert,
        query: CiCheck::new(&CheckRunEvent {
            id: 1,
            name: "test".to_string(),
            head_sha: "test".to_string(),
            html_url: None,
            details_url: None,
            url: "test".to_string(),
            started_at: Some(chrono::DateTime::from_timestamp_nanos(1718851200000000000)),
            completed_at: None,
            status: CheckRunStatus::Pending,
            conclusion: None,
        }, 1, true).upsert(),
        expected: @r#"
    INSERT INTO
      "github_ci_run_status_checks" (
        "id",
        "github_ci_run_id",
        "status_check_name",
        "status_check_status",
        "started_at",
        "completed_at",
        "url",
        "required"
      )
    VALUES
      ($1, $2, $3, $4, $5, DEFAULT, $6, $7) ON CONFLICT ("id") DO
    UPDATE
    SET
      "github_ci_run_id" = $8,
      "status_check_name" = $9,
      "status_check_status" = $10,
      "started_at" = $11,
      "url" = $12,
      "required" = $13 -- binds: [1, 1, "test", Pending, 2024-06-20T02:40:00Z, "test", true, 1, "test", Pending, 2024-06-20T02:40:00Z, "test", true]
    "#,
    }

    test_query! {
        name: test_for_run,
        query: CiCheck::for_run(1),
        expected: @r#"
    SELECT
      "github_ci_run_status_checks"."id",
      "github_ci_run_status_checks"."github_ci_run_id",
      "github_ci_run_status_checks"."status_check_name",
      "github_ci_run_status_checks"."status_check_status",
      "github_ci_run_status_checks"."started_at",
      "github_ci_run_status_checks"."completed_at",
      "github_ci_run_status_checks"."url",
      "github_ci_run_status_checks"."required"
    FROM
      "github_ci_run_status_checks"
    WHERE
      (
        "github_ci_run_status_checks"."github_ci_run_id" = $1
      ) -- binds: [1]
    "#,
    }

    #[test]
    fn test_convert_status() {
        assert_eq!(convert_status(None), GithubCiRunStatusCheckStatus::Pending);
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::Success)),
            GithubCiRunStatusCheckStatus::Success
        );
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::TimedOut)),
            GithubCiRunStatusCheckStatus::Failure
        );
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::Failure)),
            GithubCiRunStatusCheckStatus::Failure
        );
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::Cancelled)),
            GithubCiRunStatusCheckStatus::Cancelled
        );
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::Skipped)),
            GithubCiRunStatusCheckStatus::Skipped
        );
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::ActionRequired)),
            GithubCiRunStatusCheckStatus::Failure
        );
        assert_eq!(
            convert_status(Some(&CheckRunConclusion::Neutral)),
            GithubCiRunStatusCheckStatus::Pending
        );
    }
}
