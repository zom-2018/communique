use std::path::{Path, PathBuf};

use regex::Regex;
use xx::git::Git;
use xx::process;

use crate::error::{Error, Result};

pub fn repo_root() -> Result<PathBuf> {
    let path = process::cmd("git", ["rev-parse", "--show-toplevel"]).read()?;
    Ok(PathBuf::from(path))
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProjectProvider {
    GitHub,
    GitLab,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TokenSource {
    Environment(String),
    JobToken,
}

#[allow(dead_code)] // Fields are consumed by the GitLab API integration in the next step.
#[derive(Debug)]
pub struct ProjectIdentity {
    pub provider: ProjectProvider,
    pub web_url: String,
    pub api_url: String,
    pub path: String,
    pub project_id: Option<String>,
    pub token_source: TokenSource,
}

#[derive(Default)]
pub struct ProjectEnvironment {
    pub server_url: Option<String>,
    pub api_v4_url: Option<String>,
    pub project_path: Option<String>,
    pub project_id: Option<String>,
    pub gitlab_token_available: bool,
    pub job_token_available: bool,
}

/// Identify the project without retaining credentials from remote URLs or environment values.
pub fn detect_project(
    repo_root: &Path,
    repo: Option<&str>,
    config: Option<&crate::config::GitConfig>,
) -> Result<ProjectIdentity> {
    let env = ProjectEnvironment {
        server_url: std::env::var("CI_SERVER_URL").ok(),
        api_v4_url: std::env::var("CI_API_V4_URL").ok(),
        project_path: std::env::var("CI_PROJECT_PATH").ok(),
        project_id: std::env::var("CI_PROJECT_ID").ok(),
        gitlab_token_available: std::env::var("GITLAB_TOKEN").is_ok_and(|s| !s.is_empty()),
        job_token_available: std::env::var("CI_JOB_TOKEN").is_ok_and(|s| !s.is_empty()),
    };
    let remote = Git::new(repo_root.to_path_buf()).get_remote_url();
    resolve_project(remote.as_deref(), repo, config, &env)
}

fn clean_web_url(value: &str) -> Result<String> {
    let mut url =
        reqwest::Url::parse(value).map_err(|_| Error::Config("invalid git server URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Config("invalid git server URL".into()));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn remote_parts(remote: &str) -> Option<(String, String)> {
    if remote.contains("://") {
        let url = reqwest::Url::parse(remote).ok()?;
        let host = url.host_str()?;
        let scheme = if url.scheme() == "ssh" {
            "https"
        } else {
            url.scheme()
        };
        if !matches!(scheme, "http" | "https") {
            return None;
        }
        let port = if url.scheme() == "ssh" {
            String::new()
        } else {
            url.port().map(|p| format!(":{p}")).unwrap_or_default()
        };
        Some((
            format!("{scheme}://{host}{port}"),
            url.path()
                .trim_start_matches('/')
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_owned(),
        ))
    } else {
        let (host, path) = remote.split_once(':')?;
        Some((
            format!("https://{}", host.rsplit('@').next()?),
            path.trim_end_matches('/')
                .trim_end_matches(".git")
                .to_owned(),
        ))
    }
}

fn relative_project_path<'a>(remote_path: &'a str, remote_origin: &str, web_url: &str) -> &'a str {
    web_url
        .strip_prefix(remote_origin)
        .and_then(|path| path.strip_prefix('/'))
        .filter(|path| !path.is_empty())
        .and_then(|prefix| remote_path.strip_prefix(prefix))
        .and_then(|suffix| suffix.strip_prefix('/'))
        .unwrap_or(remote_path)
}

pub fn resolve_project(
    remote: Option<&str>,
    repo: Option<&str>,
    config: Option<&crate::config::GitConfig>,
    env: &ProjectEnvironment,
) -> Result<ProjectIdentity> {
    let configured = config.and_then(|c| c.provider.as_deref());
    if configured.is_some_and(|p| p != "github" && p != "gitlab") {
        return Err(Error::Config(
            "git.provider must be github or gitlab".into(),
        ));
    }
    let http_remote =
        remote.is_some_and(|url| url.starts_with("https://") || url.starts_with("http://"));
    let remote = remote.and_then(remote_parts);
    let ci = env.server_url.as_deref().zip(env.project_path.as_deref());
    let is_gitlab = configured == Some("gitlab")
        || (configured.is_none()
            && (ci.is_some()
                || remote.as_ref().is_some_and(|(url, _)| {
                    reqwest::Url::parse(url)
                        .ok()
                        .and_then(|u| {
                            u.host_str()
                                .map(|h| h == "gitlab.com" || h.starts_with("gitlab."))
                        })
                        .unwrap_or(false)
                })));
    if !is_gitlab
        && configured.is_none()
        && repo.is_none()
        && remote
            .as_ref()
            .is_some_and(|(url, _)| url != "https://github.com" && url != "http://github.com")
    {
        return Err(Error::Git(
            "cannot identify git provider or project from remote".into(),
        ));
    }
    let configured_url = config.and_then(|c| c.base_url.as_deref());
    let ci_url = ci.map(|(url, _)| url);
    let base = if is_gitlab {
        configured_url
            .or(ci_url)
            .or_else(|| remote.as_ref().map(|(url, _)| url.as_str()))
            .unwrap_or("https://gitlab.com")
    } else {
        "https://github.com"
    };
    let web_url = clean_web_url(base)?;
    let remote_path = remote.as_ref().map(|(origin, path)| {
        if is_gitlab && http_remote {
            relative_project_path(path, origin, &web_url)
        } else {
            path.as_str()
        }
    });
    let ci_path = if is_gitlab {
        ci.map(|(_, path)| path)
    } else {
        None
    };
    let path = repo
        .or(ci_path)
        .or(remote_path)
        .ok_or_else(|| Error::Git("no origin remote found".into()))?;
    if !valid_project_path(path) {
        return Err(Error::Git("invalid project path".into()));
    }
    let use_ci = is_gitlab
        && ci_url.and_then(|url| clean_web_url(url).ok()).as_deref() == Some(web_url.as_str());
    let ci_origin = ci_url
        .and_then(|url| reqwest::Url::parse(url).ok())
        .map(|url| url.origin());
    let ci_api_url = if use_ci {
        env.api_v4_url.as_deref().map(clean_web_url).transpose()?
    } else {
        None
    };
    let trusted_ci_api = ci_api_url
        .as_deref()
        .is_none_or(|url| reqwest::Url::parse(url).ok().map(|url| url.origin()) == ci_origin);
    let api_url = if is_gitlab {
        ci_api_url
            .filter(|_| trusted_ci_api)
            .unwrap_or_else(|| format!("{web_url}/api/v4"))
    } else {
        "https://api.github.com".into()
    };
    let project_id = if use_ci && env.project_path.as_deref() == Some(path) {
        env.project_id
            .clone()
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
    } else {
        None
    };
    let remote_origin = remote
        .as_ref()
        .and_then(|(url, _)| reqwest::Url::parse(url).ok())
        .map(|url| url.origin());
    let job_token_allowed = use_ci
        && ci_origin.is_some()
        && trusted_ci_api
        && remote_origin.is_none_or(|origin| Some(origin) == ci_origin);
    let token_source = if is_gitlab {
        if let Some(name) = config.and_then(|c| c.token_env.as_ref()) {
            if name == "CI_JOB_TOKEN" {
                if !job_token_allowed {
                    return Err(Error::Config(
                        "CI_JOB_TOKEN requires matching GitLab CI origins".into(),
                    ));
                }
                TokenSource::JobToken
            } else {
                TokenSource::Environment(name.clone())
            }
        } else if env.job_token_available && !env.gitlab_token_available && job_token_allowed {
            TokenSource::JobToken
        } else {
            TokenSource::Environment("GITLAB_TOKEN".into())
        }
    } else {
        TokenSource::Environment("GITHUB_TOKEN".into())
    };
    Ok(ProjectIdentity {
        provider: if is_gitlab {
            ProjectProvider::GitLab
        } else {
            ProjectProvider::GitHub
        },
        web_url,
        api_url,
        path: path.to_owned(),
        project_id,
        token_source,
    })
}

fn valid_project_path(path: &str) -> bool {
    path.contains('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
pub fn previous_tag(repo_root: &Path, current_tag: &str) -> Result<String> {
    previous_tag_filtered(repo_root, current_tag, None, crate::workflow::Channel::All)
}

pub fn previous_tag_filtered(
    repo_root: &Path,
    current_tag: &str,
    pattern: Option<&str>,
    channel: crate::workflow::Channel,
) -> Result<String> {
    let target = resolve_ref(repo_root, current_tag)?;
    let stdout = process::cmd(
        "git",
        [
            "tag",
            "--merged",
            &target,
            "--sort=-v:refname",
            "--list",
            pattern.unwrap_or("*"),
        ],
    )
    .cwd(repo_root)
    .read()?;
    let target_is_tag = verify_ref(repo_root, &format!("refs/tags/{current_tag}"))
        .is_ok_and(|commit| commit == target);
    let stable = matches!(channel, crate::workflow::Channel::Stable);
    let prerelease = Regex::new(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?-").unwrap();
    for tag in stdout.lines().map(str::trim).filter(|tag| !tag.is_empty()) {
        if tag == current_tag || (stable && prerelease.is_match(tag)) {
            continue;
        }
        // A different tag on the target commit is not a useful baseline.
        if target_is_tag && verify_ref(repo_root, tag)? == target {
            continue;
        }
        return Ok(tag.into());
    }
    root_commit(repo_root, &target)
}

pub fn verify_ref(repo_root: &Path, git_ref: &str) -> Result<String> {
    let commit = format!("{git_ref}^{{commit}}");
    process::cmd(
        "git",
        ["rev-parse", "--verify", "--end-of-options", &commit],
    )
    .cwd(repo_root)
    .stderr_capture()
    .read()
    .map(|s| s.trim().to_string())
    .map_err(|_| Error::Git(format!("Cannot resolve git reference '{git_ref}'")))
}

pub fn is_shallow(repo_root: &Path) -> Result<bool> {
    Ok(
        process::cmd("git", ["rev-parse", "--is-shallow-repository"])
            .cwd(repo_root)
            .read()?
            .trim()
            == "true",
    )
}

fn root_commit(repo_root: &Path, target: &str) -> Result<String> {
    let sha = process::cmd("git", ["rev-list", "--max-parents=0", target])
        .cwd(repo_root)
        .read()?;
    sha.lines()
        .next()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| Error::Git("no commits found".into()))
}

/// Resolve a ref to a commit, falling back to HEAD if the ref doesn't exist
/// (e.g. a version tag that hasn't been created yet).
pub fn resolve_ref(repo_root: &Path, git_ref: &str) -> Result<String> {
    verify_ref(repo_root, git_ref).or_else(|_| verify_ref(repo_root, "HEAD"))
}

#[cfg(test)]
pub fn log_between(repo_root: &Path, from: &str, to: &str) -> Result<String> {
    log_between_paths(repo_root, from, to, &[])
}

pub fn log_between_paths(
    repo_root: &Path,
    from: &str,
    to: &str,
    paths: &[String],
) -> Result<String> {
    let from = verify_ref(repo_root, from)?;
    let to = resolve_ref(repo_root, to)?;
    let range = format!("{from}..{to}");
    let mut args = vec![
        "log".to_string(),
        range,
        "--pretty=format:%h %s".into(),
        "--reverse".into(),
        "--".into(),
    ];
    args.extend(paths.iter().map(|p| format!(":(literal){p}")));
    process::cmd("git", args)
        .cwd(repo_root)
        .read()
        .map_err(Into::into)
}

/// GitHub logins credited on one commit: its author and any `Co-authored-by`
/// trailers, in that order, without duplicates.
pub type CommitCredit = (String, Vec<String>);

/// List the GitHub logins credited on each commit in `from..to`, so release
/// notes can credit people who appear only as a commit author or co-author
/// (for example a contributor whose PR was recreated by a maintainer).
pub fn commit_credits(
    repo_root: &Path,
    from: &str,
    to: &str,
    paths: &[String],
) -> Result<Vec<CommitCredit>> {
    let from = verify_ref(repo_root, from)?;
    let to = resolve_ref(repo_root, to)?;
    let mut args = vec![
        "log".to_string(),
        format!("{from}..{to}"),
        "--pretty=format:%h%x1f%ae%x1f%(trailers:key=Co-authored-by,valueonly,unfold,separator=%x1e)%x1d"
            .into(),
        "--reverse".into(),
        "--".into(),
    ];
    args.extend(paths.iter().map(|p| format!(":(literal){p}")));
    let raw = process::cmd("git", args).cwd(repo_root).read()?;
    Ok(parse_commit_credits(&raw))
}

fn parse_commit_credits(raw: &str) -> Vec<CommitCredit> {
    raw.split('\x1d')
        .filter_map(|record| {
            let mut fields = record.trim().splitn(3, '\x1f');
            let hash = fields.next().filter(|h| !h.is_empty())?;
            let author = fields.next().unwrap_or_default();
            let co_authors = fields.next().unwrap_or_default();
            let mut logins: Vec<String> = Vec::new();
            for login in std::iter::once(author)
                .chain(co_authors.split('\x1e'))
                .filter_map(login_from_identity)
            {
                if !logins.iter().any(|l| l.eq_ignore_ascii_case(&login)) {
                    logins.push(login);
                }
            }
            (!logins.is_empty()).then(|| (hash.to_string(), logins))
        })
        .collect()
}

/// Extract a GitHub login from an email or a `Name <email>` trailer value.
/// Only GitHub noreply addresses (`<id>+<login>@users.noreply.github.com` or
/// `<login>@users.noreply.github.com`) identify a login; bots are skipped.
fn login_from_identity(identity: &str) -> Option<String> {
    let email = identity
        .rsplit_once('<')
        .map_or(identity, |(_, rest)| rest.trim_end_matches('>'))
        .trim();
    let local = email.strip_suffix("@users.noreply.github.com")?;
    let login = local.rsplit_once('+').map_or(local, |(_, login)| login);
    (is_valid_login(login) && !is_automation_login(login)).then(|| login.to_string())
}

/// GitHub logins are 1-39 ASCII letters, digits and hyphens; Enterprise Managed
/// User logins (`name_shortcode`) also contain an underscore. `[bot]` accounts
/// fail this check on purpose; they are automation and are never credited.
fn is_valid_login(login: &str) -> bool {
    (1..=39).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// AI assistants and automation are tools, not contributors; never credit them.
fn is_automation_login(login: &str) -> bool {
    const SKIPPED: &[&str] = &[
        "claude",
        "copilot",
        "codex",
        "cursoragent",
        "github-actions",
        "dependabot",
        "renovate",
    ];
    let login = login.to_ascii_lowercase();
    login.ends_with("[bot]") || SKIPPED.contains(&login.as_str())
}

pub fn extract_pr_numbers(log: &str) -> Vec<u64> {
    let re = Regex::new(r"\(#(\d+)\)").unwrap();
    re.captures_iter(log)
        .filter_map(|cap| cap[1].parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_respects_ancestry_pattern_and_channel() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("cli/file", "a");
        repo.commit("base");
        repo.tag("cli/v1.0.0");
        repo.write_file("cli/file", "b");
        repo.commit("beta");
        repo.tag("cli/v2.0.0-beta.1");
        repo.write_file("cli/file", "c");
        repo.commit("target");
        repo.tag("cli/v2.0.0");
        repo.write_file("cli/file", "d");
        repo.commit("future");
        repo.tag("cli/v3.0.0");
        assert_eq!(
            previous_tag_filtered(
                repo.path(),
                "cli/v2.0.0",
                Some("cli/*"),
                crate::workflow::Channel::Stable
            )
            .unwrap(),
            "cli/v1.0.0"
        );
        assert_eq!(
            previous_tag_filtered(
                repo.path(),
                "cli/v2.0.0",
                Some("cli/*"),
                crate::workflow::Channel::All
            )
            .unwrap(),
            "cli/v2.0.0-beta.1"
        );
    }

    #[test]
    fn path_filter_omits_other_packages_and_rejects_bad_baseline() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("cli/file", "a");
        repo.commit("base");
        repo.tag("v1");
        repo.write_file("server/file", "a");
        repo.commit("server feature");
        repo.write_file("cli/file", "b");
        repo.commit("cli feature");
        repo.tag("v2");
        let log = log_between_paths(repo.path(), "v1", "v2", &["cli".into()]).unwrap();
        assert!(log.contains("cli feature"));
        assert!(!log.contains("server feature"));
        assert!(log_between_paths(repo.path(), "typo", "v2", &[]).is_err());
    }

    fn project_path_for_remote(remote: &str) -> String {
        resolve_project(Some(remote), None, None, &ProjectEnvironment::default())
            .unwrap()
            .path
    }

    #[test]
    fn test_parse_owner_repo_ssh() {
        assert_eq!(
            project_path_for_remote("git@github.com:jdx/pitchfork.git"),
            "jdx/pitchfork"
        );
    }

    #[test]
    fn test_parse_owner_repo_https() {
        assert_eq!(
            project_path_for_remote("https://github.com/jdx/pitchfork.git"),
            "jdx/pitchfork"
        );
    }

    #[test]
    fn test_extract_pr_numbers() {
        let log =
            "abc1234 feat: add feature (#123)\ndef5678 fix: bug (#456)\nghi9012 chore: update deps";
        assert_eq!(extract_pr_numbers(log), vec![123, 456]);
    }

    #[test]
    fn test_resolve_ref_existing() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("f.txt", "a");
        repo.commit("first");
        repo.tag("v1.0.0");

        let sha = resolve_ref(repo.path(), "v1.0.0").unwrap();
        assert!(!sha.is_empty());
        assert_eq!(sha.len(), 40);
    }

    #[test]
    fn test_resolve_ref_fallback_to_head() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("f.txt", "a");
        repo.commit("first");

        let sha = resolve_ref(repo.path(), "nonexistent-tag").unwrap();
        assert_eq!(sha.len(), 40);
    }

    #[test]
    fn test_previous_tag() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("f.txt", "a");
        repo.commit("first");
        repo.tag("v1.0.0");
        repo.write_file("f.txt", "b");
        repo.commit("second");
        repo.tag("v2.0.0");

        let prev = previous_tag(repo.path(), "v2.0.0").unwrap();
        assert_eq!(prev, "v1.0.0");
    }

    #[test]
    fn test_previous_tag_first_release() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("f.txt", "a");
        repo.commit("first");
        repo.tag("v1.0.0");

        // No previous tag — should fall back to root commit SHA
        let prev = previous_tag(repo.path(), "v1.0.0").unwrap();
        assert_eq!(prev.len(), 40);
    }

    #[test]
    fn test_log_between() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("f.txt", "a");
        repo.commit("first commit");
        repo.tag("v1.0.0");
        repo.write_file("f.txt", "b");
        repo.commit("second commit");
        repo.tag("v2.0.0");

        let log = log_between(repo.path(), "v1.0.0", "v2.0.0").unwrap();
        assert!(log.contains("second commit"));
        assert!(!log.contains("first commit"));
    }

    #[test]
    fn login_from_identity_accepts_only_github_noreply() {
        assert_eq!(
            login_from_identity("Navid EMAD <66868650+navidemad@users.noreply.github.com>"),
            Some("navidemad".into())
        );
        assert_eq!(
            login_from_identity("octo@users.noreply.github.com"),
            Some("octo".into())
        );
        assert_eq!(
            login_from_identity("Ada <ada,bob@users.noreply.github.com>"),
            None
        );
        assert_eq!(
            login_from_identity(&format!("{}@users.noreply.github.com", "a".repeat(40))),
            None
        );
        assert_eq!(
            login_from_identity("Ann <9+ann_acme@users.noreply.github.com>"),
            Some("ann_acme".into())
        );
        assert_eq!(login_from_identity("Claude <noreply@anthropic.com>"), None);
        assert_eq!(
            login_from_identity("bot <1+renovate[bot]@users.noreply.github.com>"),
            None
        );
        assert_eq!(
            login_from_identity("Copilot <175728472+Copilot@users.noreply.github.com>"),
            None
        );
    }

    #[test]
    fn commit_credits_include_author_and_coauthor_trailers() {
        let repo = crate::test_helpers::TempRepo::new();
        repo.write_file("a", "a");
        repo.commit("base");
        repo.tag("v1");
        repo.write_file("a", "b");
        repo.commit_as(
            "fix: recreated (#1)\n\nCo-authored-by: Ada <7+ada@users.noreply.github.com>\nCo-authored-by: Claude <noreply@anthropic.com>\nCo-authored-by: Ada <7+ada@users.noreply.github.com>",
            "Maintainer <1+Maint@users.noreply.github.com>",
        );
        repo.write_file("a", "c");
        repo.commit_as("chore: plain", "Bob <2+bob@users.noreply.github.com>");
        repo.write_file("a", "d");
        repo.commit_as("chore: ignored", "Test <test@test.com>");
        let credits = commit_credits(repo.path(), "v1", "HEAD", &[]).unwrap();
        let logins: Vec<_> = credits.iter().map(|(_, l)| l.clone()).collect();
        assert_eq!(
            logins,
            vec![
                vec!["Maint".to_string(), "ada".into()],
                vec!["bob".to_string()]
            ]
        );
    }

    #[test]
    fn parse_commit_credits_dedupes_logins_ignoring_case() {
        let raw =
            "abc\x1f7+Ada@users.noreply.github.com\x1fAda <7+ada@users.noreply.github.com>\x1d";
        assert_eq!(
            parse_commit_credits(raw),
            vec![("abc".to_string(), vec!["Ada".to_string()])]
        );
    }
}

#[cfg(test)]
mod project_identification_tests {
    use super::*;

    #[test]
    fn gitlab_job_token_requires_matching_ci_origins() {
        let env = ProjectEnvironment {
            server_url: Some("https://ci.example.com".into()),
            api_v4_url: Some("https://ci.example.com/api/v4".into()),
            project_path: Some("group/project".into()),
            job_token_available: true,
            ..Default::default()
        };
        let config = crate::config::GitConfig {
            provider: Some("gitlab".into()),
            base_url: Some("https://other.example.com".into()),
            ..Default::default()
        };
        let project = resolve_project(None, None, Some(&config), &env).unwrap();
        assert_ne!(project.token_source, TokenSource::JobToken);

        let env = ProjectEnvironment {
            api_v4_url: Some("https://attacker.example.com/api/v4".into()),
            ..env
        };
        let project = resolve_project(None, None, None, &env).unwrap();
        assert_ne!(project.token_source, TokenSource::JobToken);
        assert_eq!(project.api_url, "https://ci.example.com/api/v4");
    }

    #[test]
    fn gitlab_job_token_rejects_explicit_token_for_foreign_origin() {
        let env = ProjectEnvironment {
            server_url: Some("https://ci.example.com".into()),
            project_path: Some("group/project".into()),
            job_token_available: true,
            ..Default::default()
        };
        let config = crate::config::GitConfig {
            provider: Some("gitlab".into()),
            base_url: Some("https://other.example.com".into()),
            token_env: Some("CI_JOB_TOKEN".into()),
        };
        assert!(resolve_project(None, None, Some(&config), &env).is_err());
        let project = resolve_project(
            Some("https://other.example.com/group/project.git"),
            None,
            None,
            &env,
        )
        .unwrap();
        assert_ne!(project.token_source, TokenSource::JobToken);
    }

    #[test]
    fn gitlab_resolution_preserves_instance_project_and_token_source() {
        let ci = ProjectEnvironment {
            server_url: Some("https://gitlab.example.com".into()),
            api_v4_url: Some("https://gitlab.example.com/custom/api/v4".into()),
            project_path: Some("group/sub/project".into()),
            project_id: Some("123".into()),
            job_token_available: true,
            ..Default::default()
        };
        let project = resolve_project(
            Some("git@gitlab.example.com:group/sub/project.git"),
            None,
            None,
            &ci,
        )
        .unwrap();
        assert_eq!(project.provider, ProjectProvider::GitLab);
        assert_eq!(project.web_url, "https://gitlab.example.com");
        assert_eq!(project.api_url, "https://gitlab.example.com/custom/api/v4");
        assert_eq!(project.path, "group/sub/project");
        assert_eq!(project.project_id.as_deref(), Some("123"));
        assert_eq!(project.token_source, TokenSource::JobToken);

        let config = crate::config::GitConfig {
            provider: Some("gitlab".into()),
            base_url: Some("https://gitlab.other.test/root".into()),
            token_env: Some("MY_TOKEN".into()),
        };
        let project = resolve_project(
            Some("ssh://git@gitlab.example.com/group/sub/project.git"),
            Some("another/nested/repo"),
            Some(&config),
            &ci,
        )
        .unwrap();
        assert_eq!(project.path, "another/nested/repo");
        assert_eq!(project.web_url, "https://gitlab.other.test/root");
        assert_eq!(project.api_url, "https://gitlab.other.test/root/api/v4");
        assert_eq!(project.project_id, None);
        assert_eq!(
            project.token_source,
            TokenSource::Environment("MY_TOKEN".into())
        );
    }

    #[test]
    fn gitlab_repo_override_keeps_remote_provider_and_never_reuses_other_project_id() {
        let ci = ProjectEnvironment {
            project_path: Some("old/project".into()),
            project_id: Some("123".into()),
            ..Default::default()
        };
        let project = resolve_project(
            Some("https://user:secret@gitlab.example.com/group/sub/project.git"),
            Some("new/project"),
            None,
            &ci,
        )
        .unwrap();
        assert_eq!(project.provider, ProjectProvider::GitLab);
        assert_eq!(project.web_url, "https://gitlab.example.com");
        assert_eq!(project.api_url, "https://gitlab.example.com/api/v4");
        assert_eq!(project.project_id, None);
        assert_eq!(project.path, "new/project");
        assert_eq!(
            project.token_source,
            TokenSource::Environment("GITLAB_TOKEN".into())
        );
    }

    #[test]
    fn gitlab_http_remote_uses_project_path_relative_to_instance() {
        let config = crate::config::GitConfig {
            provider: Some("gitlab".into()),
            base_url: Some("https://code.example.com/gitlab".into()),
            ..Default::default()
        };
        let environment = ProjectEnvironment::default();
        let project = resolve_project(
            Some("https://code.example.com/gitlab/group/sub/project.git"),
            None,
            Some(&config),
            &environment,
        )
        .unwrap();
        assert_eq!(project.path, "group/sub/project");
        assert_eq!(project.api_url, "https://code.example.com/gitlab/api/v4");

        let ssh_project = resolve_project(
            Some("git@code.example.com:gitlab/sub/project.git"),
            None,
            Some(&config),
            &environment,
        )
        .unwrap();
        assert_eq!(ssh_project.path, "gitlab/sub/project");
    }

    #[test]
    fn github_repo_override_uses_github_web_url() {
        let project = resolve_project(
            Some("https://other.example.com/group/project.git"),
            Some("owner/repo"),
            None,
            &ProjectEnvironment::default(),
        )
        .unwrap();
        assert_eq!(project.provider, ProjectProvider::GitHub);
        assert_eq!(project.web_url, "https://github.com");
    }
}
