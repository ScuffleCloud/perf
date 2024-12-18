use std::borrow::Cow;

use chrono::{DateTime, Utc};
use diesel::prelude::{AsChangeset, Insertable, Queryable};
use diesel::query_dsl::methods::FilterDsl;
use diesel::{ExpressionMethods, Selectable};
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::github::webhook::check_event::{CheckRunConclusion, CheckRunEvent};
use crate::schema::enums::GithubCiRunStatusCheckStatus;

#[derive(Debug, Insertable, Queryable, Selectable, AsChangeset)]
#[diesel(table_name = crate::schema::github_ci_run_status_checks)]
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

	pub async fn upsert(&self, conn: &mut AsyncPgConnection) -> Result<(), diesel::result::Error> {
		diesel::insert_into(crate::schema::github_ci_run_status_checks::dsl::github_ci_run_status_checks)
			.values(self)
			.on_conflict(crate::schema::github_ci_run_status_checks::id)
			.do_update()
			.set(self)
			.execute(conn)
			.await?;

		Ok(())
	}

	pub async fn get_for_run(conn: &mut AsyncPgConnection, run_id: i32) -> Result<Vec<Self>, diesel::result::Error> {
		crate::schema::github_ci_run_status_checks::dsl::github_ci_run_status_checks
			.filter(crate::schema::github_ci_run_status_checks::github_ci_run_id.eq(run_id))
			.get_results(conn)
			.await
	}
}
