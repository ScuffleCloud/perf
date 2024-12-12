// @generated automatically by Diesel CLI.

pub mod sql_types {
	#[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
	#[diesel(postgres_type(name = "github_pr_rollup_eligibility"))]
	pub struct GithubPrRollupEligibility;

	#[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
	#[diesel(postgres_type(name = "github_pr_status"))]
	pub struct GithubPrStatus;
}

diesel::table! {
	use diesel::sql_types::*;
	use super::sql_types::GithubPrStatus;
	use super::sql_types::GithubPrRollupEligibility;

	github_pr_merge_queue (github_repo_id, github_pr_id) {
		github_repo_id -> Int8,
		github_pr_id -> Int8,
		is_mergable -> Bool,
		is_dry_run -> Bool,
		author_id -> Int8,
		reviewer_ids -> Array<Nullable<Int8>>,
		approver_ids -> Array<Nullable<Int8>>,
		status -> GithubPrStatus,
		priority -> Int4,
		rollup_eligibility -> GithubPrRollupEligibility,
		rolled_up_github_pr_id -> Nullable<Int8>,
		head_commit_sha -> Text,
		base_commit_sha -> Text,
		head_ref -> Text,
		base_ref -> Text,
		merge_commit_sha -> Nullable<Text>,
		completed_at -> Nullable<Timestamptz>,
		created_at -> Timestamptz,
		updated_at -> Timestamptz,
	}
}

diesel::table! {
	health_check (id) {
		id -> Int4,
		updated_at -> Timestamptz,
	}
}

diesel::allow_tables_to_appear_in_same_query!(github_pr_merge_queue, health_check,);
