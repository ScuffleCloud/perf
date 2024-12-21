use std::str::FromStr;
use std::sync::Arc;

use diesel_async::AsyncPgConnection;
use dry_run::DryRunCommand;
use merge::MergeCommand;

use crate::github::models::{PullRequest, User};
use crate::github::repo::GitHubRepoClient;

mod cancel;
mod dry_run;
mod merge;
mod ping;
mod pr;
mod retry;

pub use pr::PullRequestCommand;

pub enum BrawlCommand {
    DryRun(DryRunCommand),
    Merge(MergeCommand),
    Retry,
    Cancel,
    Ping,
    PullRequest(PullRequestCommand),
}

pub struct BrawlCommandContext<'a, R> {
    pub repo: &'a R,
    pub user: User,
    pub pr: Arc<PullRequest>,
}

impl BrawlCommand {
    pub async fn handle<R: GitHubRepoClient>(
        self,
        conn: &mut AsyncPgConnection,
        context: BrawlCommandContext<'_, R>,
    ) -> anyhow::Result<()> {
        match self {
            BrawlCommand::DryRun(command) => dry_run::handle(conn, context, command).await,
            BrawlCommand::Merge(command) => merge::handle(conn, context, command).await,
            BrawlCommand::Retry => retry::handle(conn, context).await,
            BrawlCommand::Cancel => cancel::handle(conn, context).await,
            BrawlCommand::Ping => ping::handle(conn, context).await,
            BrawlCommand::PullRequest(command) => pr::handle(conn, context, command).await,
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

        let Some(command) = splits
            .find(|s| matches!(*s, "?brawl" | "@brawl" | "/brawl"))
            .and_then(|_| splits.next())
        else {
            return Err(BrawlCommandError::NoCommand);
        };

        match command {
            "merge" => {
                let mut priority = None;

                if let Some(split) = splits.next().and_then(|s| s.strip_prefix("p=")) {
                    if split.is_empty() {
                        tracing::debug!("invalid syntax, priority cannot be empty");
                        return Err(BrawlCommandError::InvalidSyntax("priority cannot be empty".into()));
                    }

                    let Ok(p) = split.parse() else {
                        tracing::debug!("invalid syntax, priority must be a positive integer");
                        return Err(BrawlCommandError::InvalidSyntax("priority must be a positive integer".into()));
                    };

                    priority = Some(p);
                }

                Ok(BrawlCommand::Merge(MergeCommand { priority }))
            }
            "cancel" => Ok(BrawlCommand::Cancel),
            "try" => {
                let mut head_sha = None;
                let mut base_sha = None;

                for next_str in splits {
                    if let Some(head) = next_str
                        .strip_prefix("commit=")
                        .or_else(|| next_str.strip_prefix("c="))
                        .or_else(|| next_str.strip_prefix("head="))
                        .or_else(|| next_str.strip_prefix("h="))
                    {
                        head_sha = Some(head.to_string());
                    } else if let Some(base) = next_str.strip_prefix("base=").or_else(|| next_str.strip_prefix("b=")) {
                        base_sha = Some(base.to_string());
                    } else {
                        break;
                    }
                }

                Ok(BrawlCommand::DryRun(DryRunCommand { head_sha, base_sha }))
            }
            "retry" => Ok(BrawlCommand::Retry),
            "ping" => Ok(BrawlCommand::Ping),
            command => {
                tracing::debug!("invalid command: {}", command);
                Err(BrawlCommandError::InvalidCommand(command.into()))
            }
        }
    }
}
