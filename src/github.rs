use crate::{config::Project, dream, process};
use anyhow::{Context, Result, ensure};
use reqwest::Url;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

// Never put the token in command arguments, prompts, remote URLs or Git config.
// Git invokes this helper with get/store/erase; only get returns credentials.
const HELPER: &str = "!f() { if [ \"$1\" = get ]; then printf 'username=x-access-token\\npassword=%s\\n' \"$GH_TOKEN\"; fi; }; f";

pub fn read_token(path: &Path) -> Result<String> {
    let file = fs::File::open(path).with_context(|| {
        format!(
            "read GitHub token at {}; run install.sh to configure it",
            path.display()
        )
    })?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "GitHub token must be a regular file");
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "GitHub token file must be private; chmod 600 {}",
        path.display()
    );
    // Bound reads and reject shell syntax/whitespace without echoing secret input.
    use std::io::Read;
    let mut token = String::new();
    file.take(4099)
        .read_to_string(&mut token)
        .context("GitHub token file must contain UTF-8 text")?;
    let bounded = token.len() <= 4098;
    let token = token.trim_end_matches(['\r', '\n']);
    ensure!(
        bounded
            && !token.is_empty()
            && token.len() <= 4096
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "GitHub token file must contain one nonempty token (letters, digits and underscores)"
    );
    Ok(token.to_owned())
}

pub fn configure_worker(cmd: &mut Command, token: &str) -> Result<()> {
    cmd.env("GH_TOKEN", token)
        .env("GITHUB_TOKEN", token)
        .env_remove("MAISTRO_GITHUB_TOKEN");
    // Append to inherited command-scoped Git configuration. Nothing is persisted.
    let start = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .map(|s| s.parse::<usize>())
        .transpose()
        .context("invalid GIT_CONFIG_COUNT")?
        .unwrap_or(0);
    let settings = [
        ("credential.https://github.com.helper", ""),
        ("credential.https://github.com.helper", HELPER),
        ("url.https://github.com/.insteadOf", "git@github.com:"),
        ("url.https://github.com/.insteadOf", "ssh://git@github.com/"),
    ];
    let count = start
        .checked_add(settings.len())
        .context("invalid GIT_CONFIG_COUNT")?;
    for (i, (key, value)) in settings.into_iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{}", start + i), key)
            .env(format!("GIT_CONFIG_VALUE_{}", start + i), value);
    }
    cmd.env("GIT_CONFIG_COUNT", count.to_string());
    Ok(())
}

pub(crate) async fn publish_dream_pr(
    project: &Project,
    token_file: &Path,
    cancel: &CancellationToken,
) -> Result<Option<String>> {
    let token = read_token(token_file)?;
    let timeout = Duration::from_secs(30);
    let mut git = Command::new("git");
    git.current_dir(&project.repo).args([
        "remote",
        "get-url",
        "--push",
        "--all",
        "--",
        &project.remote,
    ]);
    configure_worker(&mut git, &token)?;
    let remote = process::run(git, "", timeout, cancel, None).await?;
    ensure!(
        remote.code == 0,
        "could not resolve the configured Git push remote"
    );
    let repository = remote.stdout.trim().strip_prefix("https://github.com/");
    let (owner, name) = repository
        .and_then(|repo| repo.strip_suffix(".git").unwrap_or(repo).split_once('/'))
        .filter(|(owner, name)| {
            [*owner, *name].into_iter().all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
        })
        .context("dream pull requests require a single GitHub push repository")?;
    let pulls = format!("repos/{owner}/{name}/pulls");
    let qualified_head = format!("head={owner}:{}", dream::BRANCH);
    let head = format!("head={}", dream::BRANCH);
    let base = format!("base={}", project.base_branch);

    let api = async |method: &str, endpoint: &str, jq: &str, fields: &[&str]| {
        let mut gh = Command::new("gh");
        gh.current_dir(&project.repo)
            .env("GH_TOKEN", &token)
            .env("GITHUB_TOKEN", &token)
            .env("GH_PROMPT_DISABLED", "1")
            .env_remove("GH_DEBUG")
            .args([
                "api",
                "--hostname",
                "github.com",
                "--method",
                method,
                endpoint,
                "--jq",
                jq,
            ]);
        for field in fields {
            gh.args(["-f", field]);
        }
        process::run(gh, "", timeout, cancel, None)
            .await
            .context("run GitHub CLI for the dream pull request")
    };
    let checked = |output: process::Output, operation| {
        ensure!(
            output.code == 0,
            "{operation} (gh exit {}): {}",
            output.code,
            output
                .stderr
                .replace(&token, "[redacted]")
                .chars()
                .take(1000)
                .collect::<String>()
                .trim()
        );
        Ok(output.stdout.trim().to_owned())
    };
    let find_open = async || {
        let result = api(
            "GET",
            &pulls,
            ".[0].html_url // empty",
            &["state=open", &qualified_head, &base],
        )
        .await?;
        checked(result, "list open dream pull requests")
    };
    let existing = find_open().await?;
    if !existing.is_empty() {
        return Ok(Some(existing));
    }

    let mut compare = Url::parse("https://api.github.com").unwrap();
    let refs = format!("{}...{}", project.base_branch, dream::BRANCH);
    compare
        .path_segments_mut()
        .unwrap()
        .extend(["repos", owner, name, "compare", &refs]);
    let ahead = checked(
        api("GET", compare.path(), ".ahead_by", &["per_page=1"]).await?,
        "compare the dreams branch with the base branch",
    )?;
    if ahead
        .parse::<u64>()
        .context("invalid GitHub comparison result")?
        == 0
    {
        return Ok(None);
    }

    let created = api(
        "POST",
        &pulls,
        ".html_url",
        &[
            &head,
            &base,
            "title=Daily dreams",
            "body=Changes from Maistro's daily dream phases.",
        ],
    )
    .await?;
    if created.code != 0 {
        // Another client may have opened the same PR after the initial lookup.
        let existing = find_open().await?;
        if !existing.is_empty() {
            return Ok(Some(existing));
        }
    }
    let url = checked(created, "create dream pull request")?;
    ensure!(
        url.starts_with("https://github.com/"),
        "GitHub returned no pull request URL"
    );
    Ok(Some(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn token_file_must_be_private_and_contain_only_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        assert!(
            read_token(&path)
                .unwrap_err()
                .to_string()
                .contains("install.sh")
        );
        fs::write(&path, "github_pat_test\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read_token(&path)
                .unwrap_err()
                .to_string()
                .contains("chmod 600")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token(&path).unwrap(), "github_pat_test");
        for invalid in ["", " \n", "secret\nsecond", "$(command)", &"x".repeat(4097)] {
            fs::write(&path, invalid).unwrap();
            let error = read_token(&path).unwrap_err().to_string();
            assert!(error.contains("one nonempty token"));
            if !invalid.is_empty() && invalid.len() > 2 {
                assert!(!error.contains(invalid));
            }
        }
    }

    fn git(dir: &Path, args: &[&str]) -> std::process::Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", dir.join("global-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/bin/false")
            .args(args);
        configure_worker(&mut cmd, "github_pat_test").unwrap();
        assert!(!format!("{:?}", cmd.as_std().get_args()).contains("github_pat_test"));
        cmd.into_std()
    }

    #[test]
    fn git_uses_saved_token_only_for_github_and_never_stores_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = "[credential]\nhelper = !echo stale-helper >&2; exit 1\n";
        fs::write(dir.path().join("global-config"), config).unwrap();
        for (host, success) in [("github.com", true), ("example.test", false)] {
            let mut child = git(dir.path(), &["credential", "fill"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            writeln!(child.stdin.take().unwrap(), "protocol=https\nhost={host}\n").unwrap();
            let result = child.wait_with_output().unwrap();
            assert_eq!(result.status.success(), success);
            let out = String::from_utf8(result.stdout).unwrap();
            assert_eq!(out.contains("password=github_pat_test"), success);
            if success {
                assert!(result.stderr.is_empty());
                assert!(out.contains("username=x-access-token"));
            }
        }
        let mut child = git(dir.path(), &["credential", "approve"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(
            b"protocol=https\nhost=github.com\nusername=x-access-token\npassword=github_pat_test\n\n"
        ).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            fs::read_to_string(dir.path().join("global-config")).unwrap(),
            config
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn standard_github_ssh_urls_use_https_without_modifying_remote_configuration() {
        let dir = tempfile::tempdir().unwrap();
        for url in [
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "https://github.com/owner/repo.git",
        ] {
            let output = git(dir.path(), &["ls-remote", "--get-url", url])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                "https://github.com/owner/repo.git"
            );
        }
        let url = "git@example.test:owner/repo.git";
        let output = git(dir.path(), &["ls-remote", "--get-url", url])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), url);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
