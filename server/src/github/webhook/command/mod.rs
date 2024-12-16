use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use diesel_async::pooled_connection::bb8;
use diesel_async::{AsyncPgConnection, TransactionManager};
use dry_run::DryRunCommand;
use octocrab::models::pulls::PullRequest;
use octocrab::models::{Author, RepositoryId, UserId, UserProfile};
use pr::PullRequestCommand;
use review::{ReviewAction, ReviewCommand};

use crate::github::config::GitHubBrawlRepoConfig;
use crate::github::installation::InstallationClient;

pub mod cancel;
pub mod dry_run;
pub mod ping;
pub mod pr;
pub mod retry;
pub mod review;
mod utils;

pub enum BrawlCommand {
	DryRun(DryRunCommand),
	Review(ReviewCommand),
	Cancel,
	Retry,
	Ping,
	PullRequest(PullRequestCommand),
}

#[derive(Debug)]
pub struct User {
	pub id: UserId,
	pub login: String,
}

impl From<UserProfile> for User {
	fn from(user: UserProfile) -> Self {
		Self {
			id: user.id,
			login: user.login,
		}
	}
}

impl From<Author> for User {
	fn from(author: Author) -> Self {
		Self {
			id: author.id,
			login: author.login,
		}
	}
}

#[derive(Debug)]
pub struct BrawlCommandContext {
	pub repo_id: RepositoryId,
	pub user: User,
	pub issue_number: u64,
	pub pr: Option<PullRequest>,
	pub config: GitHubBrawlRepoConfig,
}

impl BrawlCommand {
	pub async fn handle(
		self,
		client: &Arc<InstallationClient>,
		database: &bb8::Pool<AsyncPgConnection>,
		context: BrawlCommandContext,
	) -> anyhow::Result<()> {
		let mut conn = database.get().await.context("database get")?;

		diesel_async::AnsiTransactionManager::begin_transaction(&mut *conn)
			.await
			.context("begin transaction")?;

		let result = match self {
			BrawlCommand::DryRun(command) => dry_run::handle(client, &mut *conn, context, command).await,
			BrawlCommand::Review(command) => review::handle(client, &mut *conn, context, command).await,
			BrawlCommand::Cancel => cancel::handle(client, &mut *conn, context).await,
			BrawlCommand::Retry => retry::handle(client, &mut *conn, context).await,
			BrawlCommand::Ping => ping::handle(client, &mut *conn, context).await,
			BrawlCommand::PullRequest(command) => pr::handle(client, &mut *conn, context, command).await,
		};

		if let Err(err) = result {
			if let Err(tx_err) = diesel_async::AnsiTransactionManager::rollback_transaction(&mut *conn)
				.await
				.context("rollback transaction")
			{
				tracing::error!("failed to rollback transaction: {:#}", tx_err);
			}

			Err(err)
		} else {
			diesel_async::AnsiTransactionManager::commit_transaction(&mut *conn)
				.await
				.context("commit transaction")?;
			Ok(())
		}
	}
}

impl FromStr for BrawlCommand {
	type Err = ();

	fn from_str(body: &str) -> Result<Self, Self::Err> {
		let lower = body.to_lowercase();
		let mut splits = lower.split_whitespace();

		while let Some(command) = splits.next() {
			if !command.eq_ignore_ascii_case("?brawl") {
				continue;
			}

			match splits.next().unwrap_or_default() {
				"r+" | "r-" => {
					let mut reviewers = Vec::new();
					let mut priority = None;

					for split in splits.by_ref() {
						if let Some(split) = split.strip_prefix("r=") {
							if split.is_empty() {
								tracing::debug!("invalid syntax, reviewer's name cannot be empty");
								return Err(());
							}

							let splits = split.split(',');
							for reviewer in splits {
								if reviewer.is_empty() {
									tracing::debug!("invalid syntax, reviewer's name cannot be empty");
									return Err(());
								}

								reviewers.push(reviewer.to_string());
							}
						} else if let Some(split) = split.strip_prefix("p=") {
							if split.is_empty() {
								tracing::debug!("invalid syntax, priority cannot be empty");
								return Err(());
							}

							let Ok(p) = split.parse() else {
								tracing::debug!("invalid syntax, priority must be a positive integer");
								return Err(());
							};

							priority = Some(p);
						} else {
							break;
						}
					}

					let action = match command {
						"r+" => ReviewAction::Approve,
						"r-" => ReviewAction::Unapprove,
						_ => unreachable!(),
					};

					return Ok(BrawlCommand::Review(ReviewCommand {
						action,
						reviewers,
						priority,
					}));
				}
				"try" => {
					let commit_sha = splits
						.next()
						.and_then(|s| s.strip_prefix("commit="))
						.map(|commit| commit.to_string());

					return Ok(BrawlCommand::DryRun(DryRunCommand {
						head_sha: commit_sha,
						base_sha: None,
					}));
				}
				"cancel" => {
					return Ok(BrawlCommand::Cancel);
				}
				"retry" => {
					return Ok(BrawlCommand::Retry);
				}
				"ping" => {
					return Ok(BrawlCommand::Ping);
				}
				command => {
					tracing::debug!("invalid command: {}", command);
					return Err(());
				}
			}
		}

		tracing::debug!("no command found");

		Err(())
	}
}
