use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
	async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
		manager
			.create_table(
				Table::create()
					.if_not_exists()
					.table(MergeQueue::Table)
					.col(pk_auto(MergeQueue::Id))
					.col(integer(MergeQueue::RepositoryId))
					.col(text(MergeQueue::TargetBranch))
					.col(integer(MergeQueue::PrId))
					.col(integer_null(MergeQueue::RollupState))
					.col(integer(MergeQueue::Priority))
					.col(integer(MergeQueue::ReviewerId))
					.col(boolean(MergeQueue::IsMerge))
					.col(integer(MergeQueue::Status).default(0)) // 0 = pending, 1 = running
					.col(timestamp(MergeQueue::CreatedAt).default(Expr::current_timestamp()))
					.col(timestamp(MergeQueue::UpdatedAt).default(Expr::current_timestamp()))
					.to_owned(),
			)
			.await?;

		manager
			.create_index(
				Index::create()
					.if_not_exists()
					.name("merge_queue_unique_repository_pr_idx")
					.table(MergeQueue::Table)
					.col(MergeQueue::RepositoryId)
					.col(MergeQueue::PrId)
					.unique()
					.to_owned(),
			)
			.await?;

		manager
			.create_index(
				Index::create()
					.if_not_exists()
					.name("merge_queue_unique_repository_target_branch_status")
					.table(MergeQueue::Table)
					.col(MergeQueue::RepositoryId)
					.col(MergeQueue::TargetBranch)
					.col(MergeQueue::Status)
					.and_where(
						// We can only have one merge running per repository/target branch
						Expr::col(MergeQueue::IsMerge).eq(true).and(Expr::col(MergeQueue::Status).eq(1)),
					)
					.unique()
					.to_owned(),
			)
			.await?;

		Ok(())
	}

	async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
		manager.drop_table(Table::drop().table(MergeQueue::Table).to_owned()).await?;

		Ok(())
	}
}

#[derive(DeriveIden)]
enum MergeQueue {
	Table,
	/// The unique identifier for the merge queue entry.
	Id,
	/// The ID of the repository that the PR belongs to.
	RepositoryId,
	/// The target branch that the PR is being merged into.
	TargetBranch,
	/// The ID of the PR that is being merged.
	PrId,
	/// The rollup state of the PR.
	RollupState,
	/// The priority of the PR.
	Priority,
	/// Status of the PR.
	Status,
	/// Whether this PR is attempting to be merged.
	IsMerge,
	/// The GitHub ID of the user who approved the PR.
	ReviewerId,
	/// Created At
	CreatedAt,
	/// Updated At
	UpdatedAt,
}
