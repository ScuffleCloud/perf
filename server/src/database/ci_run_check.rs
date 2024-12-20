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
use crate::webhook::check_event::{CheckRunConclusion, CheckRunEvent};

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

impl<'a> CiCheck<'a> {
    pub fn new(ci_check: &'a CheckRunEvent, ci_run_id: i32, required: bool) -> Self {
        Self {
            id: ci_check.id,
            github_ci_run_id: ci_run_id,
            status_check_name: Cow::Borrowed(&ci_check.name),
            status_check_status: match ci_check.conclusion {
                None => GithubCiRunStatusCheckStatus::Pending,
                Some(CheckRunConclusion::Success) => GithubCiRunStatusCheckStatus::Success,
                Some(CheckRunConclusion::TimedOut) | Some(CheckRunConclusion::Failure) => {
                    GithubCiRunStatusCheckStatus::Failure
                }
                Some(CheckRunConclusion::Cancelled) => GithubCiRunStatusCheckStatus::Cancelled,
                Some(CheckRunConclusion::Skipped) => GithubCiRunStatusCheckStatus::Skipped,
                Some(CheckRunConclusion::ActionRequired) => GithubCiRunStatusCheckStatus::Failure,
                Some(CheckRunConclusion::Neutral) => GithubCiRunStatusCheckStatus::Pending, // not sure what this is
            },
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
