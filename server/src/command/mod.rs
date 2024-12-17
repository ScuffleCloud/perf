use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use diesel_async::pooled_connection::bb8;
use diesel_async::AsyncPgConnection;
use dry_run::DryRunCommand;
use octocrab::models::pulls::PullRequest;
use octocrab::models::{Author, RepositoryId, UserId, UserProfile};
use pr::PullRequestCommand;
use review::{ReviewAction, ReviewCommand};

use crate::github::config::GitHubBrawlRepoConfig;
use crate::github::installation::InstallationClient;

pub mod dry_run;
pub mod ping;
pub mod pr;
pub mod retry;
pub mod review;

pub enum BrawlCommand {
	DryRun(DryRunCommand),
	Review(ReviewCommand),
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
	pub pr: PullRequest,
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

		match self {
			BrawlCommand::DryRun(command) => dry_run::handle(client, &mut *conn, context, command).await,
			BrawlCommand::Review(command) => review::handle(client, &mut *conn, context, command).await,
			BrawlCommand::Retry => retry::handle(client, &mut *conn, context).await,
			BrawlCommand::Ping => ping::handle(client, &mut *conn, context).await,
			BrawlCommand::PullRequest(command) => pr::handle(client, &mut *conn, context, command).await,
		}
	}
}

pub enum BrawlCommandError {
	NoCommand,
	InvalidCommand(String),
	InvalidSyntax(String),
	InvalidArgument(String),
}

impl FromStr for BrawlCommand {
	type Err = BrawlCommandError;

	fn from_str(body: &str) -> Result<Self, Self::Err> {
		let lower = body.to_lowercase();
		let mut splits = lower.split_whitespace();

		while let Some(command) = splits.find(|s| matches!(*s, "?brawl" | "@brawl" | "/brawl")).and_then(|_| splits.next()) {
			match command {
				"r+" | "r-" => {
					let mut reviewers = Vec::new();
					let mut priority = None;

					for split in splits.by_ref() {
						if let Some(split) = split.strip_prefix("r=") {
							if split.is_empty() {
								tracing::debug!("invalid syntax, reviewer's name cannot be empty");
								return Err(BrawlCommandError::InvalidSyntax("reviewer's name cannot be empty".into()));
							}

							let splits = split.split(',');
							for reviewer in splits {
								if reviewer.is_empty() {
									tracing::debug!("invalid syntax, reviewer's name cannot be empty");
									return Err(BrawlCommandError::InvalidSyntax("reviewer's name cannot be empty".into()));
								}

								reviewers.push(reviewer.to_string());
							}
						} else if let Some(split) = split.strip_prefix("p=") {
							if split.is_empty() {
								tracing::debug!("invalid syntax, priority cannot be empty");
								return Err(BrawlCommandError::InvalidSyntax("priority cannot be empty".into()));
							}

							let Ok(p) = split.parse() else {
								tracing::debug!("invalid syntax, priority must be a positive integer");
								return Err(BrawlCommandError::InvalidSyntax("priority must be a positive integer".into()));
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
				"try" => match splits.next() {
					Some("cancel") => return Ok(BrawlCommand::DryRun(DryRunCommand::Cancel)),
					mut next => {
						let mut head_sha = None;
						let mut base_sha = None;

						while let Some(next_str) = next {
							if let Some(head) = next_str
								.strip_prefix("commit=")
								.or_else(|| next_str.strip_prefix("c="))
								.or_else(|| next_str.strip_prefix("head="))
								.or_else(|| next_str.strip_prefix("h="))
							{
								head_sha = Some(head.to_string());
							} else if let Some(base) = next_str.strip_prefix("base=").or_else(|| next_str.strip_prefix("b="))
							{
								base_sha = Some(base.to_string());
							} else {
								break;
							}

							next = splits.next();
						}

						return Ok(BrawlCommand::DryRun(DryRunCommand::New { head_sha, base_sha }));
					}
				},
				"retry" => {
					return Ok(BrawlCommand::Retry);
				}
				"ping" => {
					return Ok(BrawlCommand::Ping);
				}
				command => {
					tracing::debug!("invalid command: {}", command);
					return Err(BrawlCommandError::InvalidCommand(command.into()));
				}
			}
		}

		tracing::debug!("no command found");

		Err(BrawlCommandError::NoCommand)
	}
}
