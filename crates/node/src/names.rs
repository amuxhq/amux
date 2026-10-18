//! What an unnamed agent's name must not be: a branch of the repository it
//! starts in. The daemon core picks the name itself (see
//! [`daemon_core::names`]).

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;

/// The names a new branch in `cwd`'s repository could not take: every local
/// branch, and each folder a branch sits under (`a/b` rules out `a`). Empty
/// when `cwd` is not in a repository or git cannot be run.
pub async fn branch_names(cwd: &Path) -> HashSet<String> {
    let output = tokio::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .await;
    let Ok(output) = output else {
        return HashSet::new();
    };
    if !output.status.success() {
        return HashSet::new();
    }
    let mut names = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(branch) = line.strip_prefix("refs/heads/") else {
            continue;
        };
        let mut prefix = String::new();
        for part in branch.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            names.insert(prefix.to_lowercase());
        }
    }
    names
}
