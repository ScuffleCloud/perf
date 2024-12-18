pub fn commit_link(owner: &str, repo: &str, sha: &str) -> String {
	format!("https://github.com/{}/{}/commit/{}", owner, repo, sha)
}

pub fn issue_link(owner: &str, repo: &str, issue_number: u64) -> String {
	format!("https://github.com/{}/{}/issues/{}", owner, repo, issue_number)
}
