use std::str::FromStr;

use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize, Clone, smart_default::SmartDefault)]
#[serde(default)]
pub struct GitHubBrawlRepoConfig {
    /// Whether Brawl is enabled for this repo
    #[default(true)]
    pub enabled: bool,
    /// Labels to attach to PRs on different states
    pub labels: GitHubBrawlLabelsConfig,
    /// The target branches that this queue matches against.
    pub branches: Vec<String>,
    /// The branch prefix for @brawl try commands (default:
    /// "automation/brawl/try/")
    #[default("automation/brawl/try/")]
    pub try_branch_prefix: String,
    /// The branch prefix for @brawl merge commands (default:
    /// "automation/brawl/merge/")
    #[default("automation/brawl/merge/")]
    pub merge_branch_prefix: String,
    /// The branch prefix for temp branches used when performing merges
    /// (default: "automation/brawl/temp/")
    #[default("automation/brawl/temp/")]
    pub temp_branch_prefix: String,
    /// The permissions required to merge a PR (default: ["role:write"])
    #[default(vec![Permission::Role(Role::Push)])]
    pub merge_permissions: Vec<Permission>,
    /// The status checks required to merge a PR (default: ["brawl-done"])
    ///
    /// If brawl will wait for all of these status checks to be successful
    /// before merging. If not provided the PR will be merged instantly.
    #[default(vec![
		"brawl-done".to_string()
	])]
    pub required_status_checks: Vec<String>,
    /// The number of minutes to wait before declaring the merge failed if the
    /// required status checks are not met.
    #[default(60)]
    pub timeout_minutes: i32,
    /// The permissions required to try a commit (default: <same as merge
    /// permissions>)
    #[default(None)]
    pub try_permissions: Option<Vec<Permission>>,
}

impl GitHubBrawlRepoConfig {
    pub fn missing() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    pub fn try_permissions(&self) -> &[Permission] {
        self.try_permissions.as_ref().unwrap_or(&self.merge_permissions)
    }
}

#[derive(Debug, Deserialize, Clone, smart_default::SmartDefault)]
#[serde(default)]
pub struct GitHubBrawlLabelsConfig {
    /// The label to attach to PRs when they are in the merge queue
    pub on_merge_queued: Option<String>,
    /// The label to attach to PRs when they are being merged
    pub on_merge_in_progress: Option<String>,
    /// The label to attach to PRs when they fail to merge
    pub on_merge_failure: Option<String>,
    /// The label to attach to PRs when they are merged
    pub on_merge_success: Option<String>,
    /// The label to attach to PRs when they are being tried
    pub on_try_in_progress: Option<String>,
    /// The label to attach to PRs when they fail to try
    pub on_try_failure: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Permission {
    Role(Role),
    Team(String),
    User(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Pull,
    Push,
    Admin,
    Maintain,
    Triage,
}

impl From<Role> for octocrab::params::teams::Permission {
    fn from(role: Role) -> Self {
        match role {
            Role::Pull => Self::Pull,
            Role::Push => Self::Push,
            Role::Admin => Self::Admin,
            Role::Maintain => Self::Maintain,
            Role::Triage => Self::Triage,
        }
    }
}

impl FromStr for Role {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "pull" | "read" => Self::Pull,
            "push" | "write" => Self::Push,
            "triage" => Self::Triage,
            "maintain" => Self::Maintain,
            "admin" => Self::Admin,
            _ => return Err(()),
        })
    }
}

impl FromStr for Permission {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prefix, value) = s.split_once(':').ok_or(())?;

        Ok(match prefix {
            "role" => Self::Role(value.parse().map_err(|_| ())?),
            "team" => Self::Team(value.to_string()),
            "user" => Self::User(value.to_string()),
            _ => return Err(()),
        })
    }
}

impl<'de> Deserialize<'de> for Permission {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(|_| serde::de::Error::custom("invalid permission"))
    }
}
