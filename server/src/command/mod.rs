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

#[derive(Debug, PartialEq, Eq)]
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

#[derive(Debug, PartialEq, Eq)]
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
            .find(|s| matches!(*s, "?brawl" | "@brawl" | "/brawl" | ">brawl"))
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

                    let Ok(p) = split.parse::<u32>() else {
                        tracing::debug!("invalid syntax, priority must be a positive integer");
                        return Err(BrawlCommandError::InvalidSyntax("priority must be a positive integer".into()));
                    };

                    priority = Some(p as i32);
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_parse() {
        let cases = [
            (
                "@brawl merge p=1",
                Ok(BrawlCommand::Merge(MergeCommand { priority: Some(1) })),
            ),
            ("@brawl merge", Ok(BrawlCommand::Merge(MergeCommand { priority: None }))),
            (
                "@brawl merge p=12a",
                Err(BrawlCommandError::InvalidSyntax("priority must be a positive integer".into())),
            ),
            (
                "@brawl merge p=-12",
                Err(BrawlCommandError::InvalidSyntax("priority must be a positive integer".into())),
            ),
            ("@brawl merge p abc", Ok(BrawlCommand::Merge(MergeCommand { priority: None }))),
            (
                "@brawl merge p=",
                Err(BrawlCommandError::InvalidSyntax("priority cannot be empty".into())),
            ),
            ("@brawl cancel", Ok(BrawlCommand::Cancel)),
            (
                "@brawl try commit=1234567890 base=9876543210",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: Some("1234567890".to_string()),
                    base_sha: Some("9876543210".to_string()),
                })),
            ),
            (
                "@brawl try commit=1234567890",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: Some("1234567890".to_string()),
                    base_sha: None,
                })),
            ),
            (
                "@brawl try base=9876543210",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: None,
                    base_sha: Some("9876543210".to_string()),
                })),
            ),
            (
                "@brawl try base=9876543210 somethign else after",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: None,
                    base_sha: Some("9876543210".to_string()),
                })),
            ),
            (
                "@brawl try",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: None,
                    base_sha: None,
                })),
            ),
            ("@brawl retry", Ok(BrawlCommand::Retry)),
            ("@brawl ping", Ok(BrawlCommand::Ping)),
            (
                "?brawl try",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: None,
                    base_sha: None,
                })),
            ),
            (
                "/brawl try",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: None,
                    base_sha: None,
                })),
            ),
            (
                ">brawl try",
                Ok(BrawlCommand::DryRun(DryRunCommand {
                    head_sha: None,
                    base_sha: None,
                })),
            ),
            ("<no command>", Err(BrawlCommandError::NoCommand)),
            ("@brawl @brawl merge", Err(BrawlCommandError::InvalidCommand("@brawl".into()))),
        ];

        for (input, expected) in cases {
            assert_eq!(BrawlCommand::from_str(input), expected);
        }
    }
}
