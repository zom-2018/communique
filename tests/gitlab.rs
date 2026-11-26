use serde_json::json;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use wiremock::matchers::{header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn gitlab_ci_example_contract() {
    let example = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/gitlab-ci/.gitlab-ci.yml"
    ))
    .expect("GitLab CI example must exist");
    let script = example
        .lines()
        .find_map(|line| line.trim().strip_prefix("- communique generate "))
        .expect("example must contain the generation command");
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    let config = readme
        .split("```toml\n[git]\n")
        .nth(1)
        .expect("documented GitLab configuration")
        .split("```")
        .next()
        .unwrap();
    let llm = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"s","type":"function","function":{"name":"submit_release_notes","arguments":"{\"changelog\":\"Example changes\",\"release_title\":\"Release\",\"release_body\":\"Example changes\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}})))
        .expect(1).mount(&llm).await;
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("file"), "before").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "base"]);
    git(&["tag", "v1"]);
    std::fs::write(repo.join("file"), "after").unwrap();
    git(&["commit", "-am", "feature"]);
    git(&["tag", "v2"]);
    std::fs::write(repo.join("communique.toml"), format!("[git]\n{config}")).unwrap();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_communique"));
    let out = Command::new("sh")
        .current_dir(repo)
        .arg("-c")
        .arg(format!("\"{}\" generate {script}", binary.display()))
        .env("CI_COMMIT_TAG", "v2")
        .env("CI_SERVER_URL", "https://gitlab.example.com")
        .env("CI_PROJECT_PATH", "group/sub/repo")
        .env("OPENAI_BASE_URL", llm.uri())
        .env("OPENAI_API_KEY", "test-llm-secret")
        .env("LLM_MODEL", "test-model")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITLAB_TOKEN")
        .env_remove("CI_JOB_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("Release range: v1..v2"));
    assert!(
        std::fs::read_to_string(repo.join("release-notes.md"))
            .unwrap()
            .contains("Example changes")
    );
    assert!(!example.contains("test-llm-secret"));
}

#[tokio::test]
async fn gitlab_secrets_never_leave_process() {
    run_gitlab_agent(404, false, false, false, false, false, 15).await;
}

#[tokio::test]
async fn gitlab_llm_error_does_not_expose_tokens() {
    run_gitlab_agent(404, false, false, false, false, false, 17).await;
}

#[tokio::test]
async fn gitlab_retry_does_not_log_token_in_url() {
    run_gitlab_agent(404, false, false, false, false, false, 18).await;
}

#[tokio::test]
async fn gitlab_publication_error_does_not_log_token_in_url() {
    run_gitlab_agent(404, false, false, false, false, false, 19).await;
}

#[tokio::test]
async fn gitlab_custom_openai_endpoint_only() {
    run_gitlab_agent(404, false, false, false, false, false, 16).await;
}

#[tokio::test]
async fn gitlab_mr_is_read_through_agent() {
    run_gitlab_agent(404, false, false, false, false, false, 0).await;
}

#[tokio::test]
async fn gitlab_release_read_denied_keeps_generation_available() {
    run_gitlab_agent(403, true, false, false, false, false, 0).await;
}

#[tokio::test]
async fn gitlab_job_token_partial_permissions_through_cli() {
    run_gitlab_agent(404, false, true, false, false, false, 0).await;
}

#[tokio::test]
async fn gitlab_context_combines_diff_commits_and_mr() {
    run_gitlab_agent(404, false, false, true, false, false, 0).await;
}

#[tokio::test]
async fn gitlab_links_are_instance_scoped() {
    run_gitlab_agent(404, false, false, false, true, false, 0).await;
}

#[tokio::test]
async fn gitlab_private_link_is_not_called_broken() {
    run_gitlab_agent(404, false, false, false, false, true, 0).await;
}

#[tokio::test]
async fn gitlab_context_label_rules_without_token_fall_back() {
    run_gitlab_agent(404, false, false, true, false, false, 1).await;
}

#[tokio::test]
async fn gitlab_context_exclude_label() {
    run_gitlab_agent(404, false, false, true, false, false, 2).await;
}

#[tokio::test]
async fn gitlab_context_unavailable_mr_keeps_diff() {
    run_gitlab_agent(404, false, false, true, false, false, 3).await;
}

#[tokio::test]
async fn gitlab_links_authenticated_api_verifies_private_resources() {
    run_gitlab_agent(404, false, false, false, false, true, 4).await;
}

#[tokio::test]
async fn gitlab_publication_creates_missing_release() {
    run_gitlab_agent(404, false, false, false, false, false, 5).await;
}

#[tokio::test]
async fn gitlab_publication_rejects_moved_tag() {
    run_gitlab_agent(404, false, false, false, false, false, 6).await;
}

#[tokio::test]
async fn gitlab_publication_is_idempotent_and_race_safe() {
    run_gitlab_agent(404, false, false, false, false, false, 7).await;
}

#[tokio::test]
async fn gitlab_publication_of_edited_draft() {
    run_gitlab_agent(200, false, false, false, false, false, 8).await;
}

#[tokio::test]
async fn gitlab_changelog_rerun_does_not_duplicate() {
    run_gitlab_agent(404, false, false, false, false, false, 9).await;
}

#[tokio::test]
async fn gitlab_changelog_rerun_with_named_tag_does_not_duplicate() {
    run_gitlab_agent(404, false, false, false, false, false, 14).await;
}

#[tokio::test]
async fn gitlab_publication_race_preserves_existing_sections() {
    run_gitlab_agent(404, false, false, false, false, false, 10).await;
}

#[tokio::test]
async fn gitlab_publication_reports_denied_job_token() {
    run_gitlab_agent(404, false, true, false, false, false, 11).await;
}

#[tokio::test]
async fn gitlab_publication_reconciles_server_failure_after_creation() {
    run_gitlab_agent(404, false, false, false, false, false, 12).await;
}

#[tokio::test]
async fn gitlab_publication_recovers_lost_response() {
    run_gitlab_agent(404, false, false, false, false, false, 13).await;
}

async fn run_gitlab_agent(
    release_status: u16,
    expect_warning: bool,
    restricted: bool,
    check_context: bool,
    check_links: bool,
    check_private: bool,
    mode: u8,
) {
    let api = MockServer::start().await;
    if [5, 6, 7, 10, 11, 12, 13].contains(&mode) {
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/group%2Fsub%2Frepo/releases"))
            .and(header(if restricted { "JOB-TOKEN" } else { "PRIVATE-TOKEN" }, "test-token"))
            .and(wiremock::matchers::body_json(
                json!({"tag_name":"v2","name":"v2: Release","description":if mode == 10 { "<!-- communique:start -->\nChanges\n<!-- communique:end -->" } else { "Changes" }}),
            ))
            .respond_with(if mode == 13 { ResponseTemplate::new(201).set_delay(std::time::Duration::from_secs(11)) } else { ResponseTemplate::new(match mode { 7 | 10 => 409, 11 => 403, 12 => 500, _ => 201 }) })
            .expect(if mode == 6 { 0 } else { 1 })
            .mount(&api)
            .await;
    }
    if mode == 10 {
        Mock::given(method("PUT")).and(path("/api/v4/projects/group%2Fsub%2Frepo/releases/v2"))
            .and(wiremock::matchers::body_json(json!({"description":"Old\n\n<!-- communique:start -->\nChanges\n<!-- communique:end -->"})))
            .respond_with(ResponseTemplate::new(200)).expect(1).mount(&api).await;
    }
    if mode == 7 || mode == 8 || mode == 12 || mode == 13 {
        Mock::given(method("PUT"))
            .and(path("/api/v4/projects/group%2Fsub%2Frepo/releases/v2"))
            .and(wiremock::matchers::body_json(
                json!({"name":"v2: Release","description":"Changes"}),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(if mode == 7 { 2 } else { 1 })
            .mount(&api)
            .await;
    }
    let release_reads = AtomicUsize::new(0);
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/releases/v2"))
        .respond_with(move |_: &wiremock::Request| {
            if mode == 8
                || ((mode == 7 || mode == 10 || mode == 12 || mode == 13)
                    && release_reads.fetch_add(1, Ordering::SeqCst) >= 2)
            {
                ResponseTemplate::new(200)
                    .set_body_json(json!({"tag_name":"v2","name":"Old","description":"Old"}))
            } else {
                ResponseTemplate::new(release_status)
            }
        })
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/merge_requests/42"))
        .and(header(if restricted { "JOB-TOKEN" } else { "PRIVATE-TOKEN" }, "test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"iid":42,"title":"Feature","description":"Details","author":{"username":"alice"},"labels":["feature"]})))
        .mount(&api).await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/merge_requests/42/diffs"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-next-page", "2").set_body_json(json!([{ "old_path":"file", "new_path":"file", "diff":"@@ -1 +1 @@\n-old\n+new\n" }])))
        .mount(&api).await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/merge_requests/42/diffs"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "old_path":"other", "new_path":"other", "diff":format!("@@ -0,0 +1 @@\n+second-page\n{}", "é".repeat(40000)) }])))
        .mount(&api).await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/issues/7"))
        .respond_with(if restricted { ResponseTemplate::new(403) } else { ResponseTemplate::new(200).set_body_json(json!({"iid":7,"title":"Issue title","description":"Issue details","state":"opened","author":{"username":"bob"},"labels":["bug"]})) })
        .mount(&api).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v4/projects/group%2Fsub%2Frepo/repository/commits/[0-9a-f]{40}/merge_requests$"))
        .respond_with(if mode == 3 { ResponseTemplate::new(403) } else { ResponseTemplate::new(200).set_body_json(json!([{"iid":42,"title":"Feature","description":"Details","author":{"username":"alice"},"labels":["feature"]}])) })
        .mount(&api).await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fsub%2Frepo/merge_requests/42/related_issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"iid":7,"title":"Issue title","description":"Issue details","state":"opened","author":{"username":"bob"},"labels":["bug"]}])))
        .mount(&api).await;
    let llm = MockServer::start().await;
    let turn = AtomicUsize::new(0);
    for endpoint in [
        "issues/7",
        "repository/commits/abc123",
        "repository/tags/v1",
        "repository/tags/v1%2Fbeta",
        "releases/v1",
    ] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v4/projects/group%2Fsub%2Frepo/{endpoint}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&api)
            .await;
    }
    if mode == 4 {
        Mock::given(method("HEAD"))
            .and(path("/group/sub/repo/-/blob/release%2Fv1/docs/guide.md"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/users/sign_in"))
            .with_priority(1)
            .mount(&api)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/users/sign_in"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&api)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/group%2Fsub%2Frepo/repository/files/docs%2Fguide.md/raw",
            ))
            .and(query_param("ref", "release/v1"))
            .and(header("PRIVATE-TOKEN", "test-token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&api)
            .await;
    }
    let private_url = if mode == 4 {
        [
            "merge_requests/42",
            "issues/7",
            "commit/abc123",
            "tags/v1%2Fbeta",
            "releases/v1",
            "blob/release%2Fv1/docs/guide.md",
        ]
        .iter()
        .map(|p| format!("{}/group/sub/repo/-/{p}", api.uri()))
        .collect::<Vec<_>>()
        .join(" ")
    } else {
        format!("{}/group/sub/repo/-/blob/main/file", api.uri())
    };
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(move |_: &wiremock::Request| {
            if mode == 17 { return ResponseTemplate::new(401).set_body_string("test-token llm-private-secret"); }
            let call = if turn.fetch_add(1, Ordering::SeqCst) == 0 && mode == 15 {
                json!({"id":"file","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"file\"}"}})
            } else if turn.load(Ordering::SeqCst) == 1 {
                json!({"id":"mr","type":"function","function":{"name":"get_pr","arguments":"{\"number\":42}"}})
            } else if turn.load(Ordering::SeqCst) == 2 {
                json!({"id":"diff","type":"function","function":{"name":"get_pr_diff","arguments":"{\"number\":42}"}})
            } else if turn.load(Ordering::SeqCst) == 3 {
                json!({"id":"issue","type":"function","function":{"name":"get_issue","arguments":"{\"number\":7}"}})
            } else {
                json!({"id":"submit","type":"function","function":{"name":"submit_release_notes","arguments":json!({"changelog":"Changes","release_title":"Release","release_body":if check_private { private_url.as_str() } else if mode == 15 { "test-token llm-private-secret" } else { "Changes" }}).to_string()}})
            };
            ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":null,"tool_calls":[call]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}))
        }).mount(&llm).await;
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    std::fs::write(repo.join("file"), "one").unwrap();
    for args in [
        vec!["add", "."],
        vec!["commit", "-m", "initial"],
        vec!["tag", "v1"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    std::fs::write(
        repo.join("file"),
        if mode == 15 {
            "two test-token llm-private-secret origin-private-secret https://someone:unregistered-url-password@example.invalid/path encoded=%74est-token".to_owned()
        } else if check_context {
            format!("two{}", "é".repeat(40000))
        } else {
            "two".to_owned()
        },
    )
    .unwrap();
    for args in [vec!["commit", "-am", "feature"], vec!["tag", "v2"]] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    if mode == 15 {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://oauth2:origin-private-secret@gitlab.example/group/sub/repo.git"
                ])
                .status()
                .unwrap()
                .success()
        );
    }
    if mode == 14 {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(["tag", "release-candidate"])
                .status()
                .unwrap()
                .success()
        );
    }
    if [5, 6, 7, 8, 10, 11, 12, 13].contains(&mode) {
        let sha = String::from_utf8(
            Command::new("git")
                .current_dir(repo)
                .args(["rev-parse", "v2"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/group%2Fsub%2Frepo/repository/tags/v2",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"commit":{"id":if mode == 6 { "moved" } else { sha.trim() }}}),
            ))
            .mount(&api)
            .await;
    }
    std::fs::write(
        repo.join("communique.toml"),
        format!(
            "system_extra = \"Keep GitHub examples unchanged.\"\n[git]\nprovider = \"gitlab\"\nbase_url = \"{}\"\n{}",
            if mode == 18 || mode == 19 { "http://127.0.0.1:1/test-token".to_owned() } else { api.uri() }, if mode == 15 { "token_env = \"CUSTOM_GITLAB_SECRET\"\n" } else if mode == 2 { "[rules]\nexclude_labels = [\"feature\"]\n" } else if check_context { "[rules]\ninclude_labels = [\"feature\"]\n" } else { "" }
        ),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_communique"));
    command
        .current_dir(repo)
        .args([
            "generate",
            if mode == 14 {
                "release-candidate"
            } else {
                "v2"
            },
            "v1",
            "--repo",
            "group/sub/repo",
            "--provider",
            "openai",
            "--model",
            "test",
            "--base-url",
            &llm.uri(),
            "--output",
            "out.md",
        ])
        .args(if mode == 9 || mode == 14 {
            vec!["--changelog"]
        } else if mode == 8 {
            vec!["--draft", "draft.json"]
        } else if mode == 10 {
            vec!["--gitlab-release", "--preserve-sections"]
        } else if [5, 6, 7, 11, 12, 13, 19].contains(&mode) {
            vec!["--gitlab-release"]
        } else if mode == 15 {
            vec!["--dry-run", "--draft", "draft.json"]
        } else if check_private {
            vec![]
        } else {
            vec!["--dry-run"]
        })
        .env(
            if restricted {
                "CI_JOB_TOKEN"
            } else {
                "GITLAB_TOKEN"
            },
            "test-token",
        )
        .env_remove(if restricted {
            "GITLAB_TOKEN"
        } else {
            "CI_JOB_TOKEN"
        })
        .env_remove("GITHUB_TOKEN")
        .env("OPENAI_API_KEY", "llm-private-secret")
        .env("CLX_NO_PROGRESS", "1")
        .env("CI_SERVER_URL", api.uri())
        .env("CI_PROJECT_PATH", "group/sub/repo")
        .env_remove("CI_PROJECT_ID")
        .env_remove("CI_API_V4_URL");
    if mode == 15 {
        command
            .env("CUSTOM_GITLAB_SECRET", "test-token")
            .env_remove("GITLAB_TOKEN");
    }
    if mode == 1 {
        command
            .env_remove("GITLAB_TOKEN")
            .env_remove("CI_JOB_TOKEN");
    }
    let output = command.output().unwrap();
    if mode == 18 || mode == 19 {
        assert_eq!(output.status.success(), mode == 18);
        assert!(String::from_utf8_lossy(&output.stderr).contains("retrying"));
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("test-token"),
            "token leaked during retry"
        );
        return;
    }
    if mode == 17 {
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("test-token"),
            "GitLab token leaked in error"
        );
        assert!(
            !stderr.contains("llm-private-secret"),
            "LLM token leaked in error"
        );
        return;
    }
    if mode == 6 {
        assert!(
            !output.status.success(),
            "moved tag must prevent publication"
        );
        return;
    }
    if mode == 11 {
        assert!(!output.status.success(), "denied publication must fail");
        assert!(String::from_utf8_lossy(&output.stderr).contains("403"));
        return;
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if mode == 7 || mode == 9 || mode == 14 {
        let second = command.output().unwrap();
        assert!(
            second.status.success(),
            "{}",
            String::from_utf8_lossy(&second.stderr)
        );
    }
    if mode == 9 || mode == 14 {
        let changelog = std::fs::read_to_string(repo.join("CHANGELOG.md")).unwrap();
        assert_eq!(
            changelog
                .matches(if mode == 14 {
                    "## [release-candidate]("
                } else {
                    "## [2]("
                })
                .count(),
            1,
            "{changelog}"
        );
        assert_eq!(changelog.matches("Changes").count(), 1, "{changelog}");
    }
    if mode == 8 {
        let draft_path = repo.join("draft.json");
        let original = std::fs::read_to_string(&draft_path).unwrap();
        let mut edited: serde_json::Value = serde_json::from_str(&original).unwrap();
        let mut legacy = edited.clone();
        legacy.as_object_mut().unwrap().remove("gitlab_project_url");
        legacy["repo"] = json!("owner/repo");
        std::fs::write(&draft_path, serde_json::to_string(&legacy).unwrap()).unwrap();
        let legacy_result = Command::new(env!("CARGO_BIN_EXE_communique"))
            .current_dir(repo)
            .args(["publish", "draft.json"])
            .env("CI_SERVER_URL", api.uri())
            .env("CI_PROJECT_PATH", "group/sub/repo")
            .env_remove("GITHUB_TOKEN")
            .env_remove("CI_PROJECT_ID")
            .env_remove("CI_API_V4_URL")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&legacy_result.stderr).contains("GITHUB_TOKEN"));
        assert_eq!(
            edited["gitlab_project_url"],
            format!("{}/group/sub/repo", api.uri())
        );
        edited["gitlab_project_url"] = json!("https://other.example/group/sub/repo");
        std::fs::write(&draft_path, serde_json::to_string(&edited).unwrap()).unwrap();
        let wrong_server = Command::new(env!("CARGO_BIN_EXE_communique"))
            .current_dir(repo)
            .args(["publish", "draft.json"])
            .env("CI_SERVER_URL", api.uri())
            .env("CI_PROJECT_PATH", "group/sub/repo")
            .env("GITLAB_TOKEN", "test-token")
            .env_remove("GITHUB_TOKEN")
            .env_remove("CI_JOB_TOKEN")
            .env_remove("CI_PROJECT_ID")
            .env_remove("CI_API_V4_URL")
            .output()
            .unwrap();
        assert!(!wrong_server.status.success());
        edited["repo"] = json!("other/project");
        std::fs::write(&draft_path, serde_json::to_string(&edited).unwrap()).unwrap();
        let rejected = Command::new(env!("CARGO_BIN_EXE_communique"))
            .current_dir(repo)
            .args(["publish", "draft.json"])
            .env("CI_SERVER_URL", api.uri())
            .env("CI_PROJECT_PATH", "group/sub/repo")
            .env("GITLAB_TOKEN", "test-token")
            .env_remove("GITHUB_TOKEN")
            .env_remove("CI_JOB_TOKEN")
            .env_remove("CI_PROJECT_ID")
            .env_remove("CI_API_V4_URL")
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        std::fs::write(&draft_path, original).unwrap();
        let preview = Command::new(env!("CARGO_BIN_EXE_communique"))
            .current_dir(repo)
            .args(["publish", "draft.json", "--dry-run"])
            .env("GITLAB_TOKEN", "test-token")
            .output()
            .unwrap();
        assert!(preview.status.success());
        let published = Command::new(env!("CARGO_BIN_EXE_communique"))
            .current_dir(repo)
            .args(["publish", "draft.json"])
            .env("CI_SERVER_URL", api.uri())
            .env("CI_PROJECT_PATH", "group/sub/repo")
            .env("GITLAB_TOKEN", "test-token")
            .env_remove("GITHUB_TOKEN")
            .env_remove("CI_JOB_TOKEN")
            .env_remove("CI_PROJECT_ID")
            .env_remove("CI_API_V4_URL")
            .output()
            .unwrap();
        assert!(
            published.status.success(),
            "{}",
            String::from_utf8_lossy(&published.stderr)
        );
    }
    if check_private && mode != 4 {
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unverifiable"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if mode == 4 {
        assert!(!String::from_utf8_lossy(&output.stderr).contains("unverifiable"));
        let requests = api.received_requests().await.unwrap();
        for suffix in [
            "issues/7",
            "repository/commits/abc123",
            "repository/tags/v1%2Fbeta",
            "releases/v1",
        ] {
            assert!(
                requests.iter().any(|r| r.method == "GET"
                    && r.url.path().ends_with(suffix)
                    && r.headers.get("private-token").is_some()),
                "missing authenticated {suffix}"
            );
        }
        assert!(
            requests
                .iter()
                .filter(|r| r.method == "HEAD")
                .all(|r| r.headers.get("private-token").is_none())
        );
    }
    if expect_warning {
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("release"),
            "expected warning about incomplete release context"
        );
    }
    let requests = llm.received_requests().await.unwrap();
    if mode == 15 {
        assert!(requests.len() >= 2);
        for request in &requests {
            let body = String::from_utf8_lossy(&request.body);
            assert!(!body.contains("test-token"), "GitLab token leaked to LLM");
            assert!(
                !body.contains("%74est-token"),
                "URL-encoded GitLab token leaked to LLM"
            );
            assert!(
                !body.contains("origin-private-secret"),
                "origin password leaked to LLM"
            );
            assert!(
                !body.contains("unregistered-url-password"),
                "URL userinfo leaked to LLM"
            );
            assert!(
                !body.contains("llm-private-secret"),
                "LLM token leaked to LLM context"
            );
        }
        assert!(!String::from_utf8_lossy(&output.stderr).contains("test-token"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("llm-private-secret"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("test-token"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("llm-private-secret"));
        let artifact = std::fs::read_to_string(repo.join("out.md")).unwrap();
        let draft = std::fs::read_to_string(repo.join("draft.json")).unwrap();
        for secret in [
            "test-token",
            "%74est-token",
            "llm-private-secret",
            "origin-private-secret",
        ] {
            assert!(!draft.contains(secret), "secret leaked to draft");
        }
        assert!(
            !artifact.contains("test-token"),
            "GitLab token leaked to output file"
        );
        assert!(
            !artifact.contains("llm-private-secret"),
            "LLM token leaked to output file"
        );
        let tool_body = String::from_utf8_lossy(&requests[1].body);
        assert!(tool_body.contains("two [REDACTED] [REDACTED] [REDACTED]"));
        return;
    }
    if mode == 16 {
        assert!(requests.iter().all(|r| r.url.path() == "/chat/completions"));
        assert!(requests.len() >= 2);
        return;
    }
    let initial: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = initial["messages"].as_array().unwrap();
    let system = messages.iter().find(|m| m["role"] == "system").unwrap()["content"]
        .as_str()
        .unwrap();
    let user = messages.iter().find(|m| m["role"] == "user").unwrap()["content"]
        .as_str()
        .unwrap();
    if mode == 1 {
        assert!(user.contains("Final diff between refs"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("incomplete"));
        return;
    }
    if mode == 2 {
        assert!(user.contains("excluded by label feature"), "{user}");
        assert!(
            !user.contains("## Merge request !42"),
            "excluded MR should not be included"
        );
        assert!(user.contains("Final diff between refs"));
        return;
    }
    if mode == 3 {
        assert!(user.contains("Final diff between refs"));
        assert!(user.contains("+two"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("context is incomplete"));
        return;
    }
    if check_context {
        let api_requests = api.received_requests().await.unwrap();
        assert_eq!(
            api_requests
                .iter()
                .filter(|r| r.url.path().contains("/repository/commits/"))
                .count(),
            1,
            "MR should be fetched once per commit"
        );
        assert!(
            user.matches("[truncated]").count() >= 2,
            "both diffs should be truncated"
        );
        for expected in [
            "feature",
            "-one",
            "+two",
            "Feature",
            "Details",
            "Issue title",
            "+second-page",
            "MUST appear",
        ] {
            assert!(user.contains(expected), "missing {expected} in {user}");
        }
    }
    if check_links {
        assert!(
            system.contains("Keep GitHub examples unchanged."),
            "{system}"
        );
        let repo_url = format!("{}/group/sub/repo", api.uri());
        assert!(user.contains(&repo_url), "{user}");
        assert!(
            system.contains(&format!("{repo_url}/-/merge_requests/123")),
            "{system}"
        );
        assert!(!system.contains("github.com/OWNER/REPO"), "{system}");
        let tools = initial["tools"].as_array().unwrap();
        let pr = tools
            .iter()
            .find(|t| t["function"]["name"] == "get_pr")
            .unwrap();
        assert!(
            pr["function"]["description"]
                .as_str()
                .unwrap()
                .contains("GitLab"),
            "{pr}"
        );
    }
    let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let message = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["tool_call_id"] == "mr")
        .unwrap();
    let last_body: serde_json::Value = serde_json::from_slice(&requests[3].body).unwrap();
    let messages = last_body["messages"].as_array().unwrap();
    for (id, expected) in [
        ("diff", "+new"),
        ("diff", "+second-page"),
        (
            "issue",
            if restricted {
                "403"
            } else {
                "Issue #7: Issue title"
            },
        ),
    ] {
        let content = messages.iter().find(|m| m["tool_call_id"] == id).unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(content.contains(expected), "{id}: {content}");
    }
    assert!(
        message["content"]
            .as_str()
            .unwrap()
            .contains("PR #42: Feature"),
        "{message}"
    );
}

#[tokio::test]
async fn previous_ref_reachable_detached_and_filtered() {
    let llm = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"s","type":"function","function":{"name":"submit_release_notes","arguments":"{\"changelog\":\"Changes\",\"release_title\":\"Release\",\"release_body\":\"Changes\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}})))
        .mount(&llm).await;
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("file"), "one").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "base"]);
    git(&["tag", "cli/v1.0.0"]);
    std::fs::write(repo.join("file"), "beta").unwrap();
    git(&["commit", "-am", "beta"]);
    git(&["tag", "cli/v1.8.0-beta.1"]);
    std::fs::write(repo.join("file"), "two").unwrap();
    git(&["commit", "-am", "target"]);
    git(&["tag", "v2.0.0"]);
    git(&["tag", "cli/v1.9.0"]);
    std::fs::write(repo.join("file"), "future").unwrap();
    git(&["commit", "-am", "future"]);
    git(&["tag", "cli/v9.0.0"]);
    git(&["checkout", "--detach", "v2.0.0"]);
    let out = Command::new(env!("CARGO_BIN_EXE_communique"))
        .current_dir(repo)
        .args([
            "generate",
            "v2.0.0",
            "--tag-pattern",
            "cli/*",
            "--channel",
            "stable",
            "--dry-run",
            "--repo",
            "group/repo",
            "--provider",
            "openai",
            "--model",
            "test",
            "--base-url",
            &llm.uri(),
        ])
        .env("OPENAI_API_KEY", "test")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Release range: cli/v1.0.0..v2.0.0"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn gitlab_ci_shallow_history_is_reported() {
    let llm = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"s","type":"function","function":{"name":"submit_release_notes","arguments":"{\"changelog\":\"Changes\",\"release_title\":\"Release\",\"release_body\":\"Changes\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}))).mount(&llm).await;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&source)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.com"]);
    std::fs::write(source.join("file"), "one").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "base"]);
    git(&["tag", "v1"]);
    std::fs::write(source.join("file"), "two").unwrap();
    git(&["commit", "-am", "feature"]);
    git(&["tag", "v2"]);
    let clone = dir.path().join("clone");
    assert!(
        Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                "v2",
                &format!("file://{}", source.display()),
                clone.to_str().unwrap()
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_communique"))
        .current_dir(&clone)
        .args([
            "generate",
            "--dry-run",
            "--provider",
            "openai",
            "--model",
            "test",
            "--base-url",
            &llm.uri(),
        ])
        .env("CI_SERVER_URL", "https://gitlab.example.com")
        .env("CI_PROJECT_PATH", "group/repo")
        .env("CI_COMMIT_TAG", "v2")
        .env("OPENAI_API_KEY", "test")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITLAB_TOKEN")
        .env_remove("CI_JOB_TOKEN")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("shallow"), "{stderr}");
    assert!(stderr.contains("incomplete"), "{stderr}");

    let out = Command::new(env!("CARGO_BIN_EXE_communique"))
        .current_dir(&source)
        .args([
            "generate",
            "HEAD",
            "HEAD^1",
            "--dry-run",
            "--repo",
            "group/repo",
            "--provider",
            "openai",
            "--model",
            "test",
            "--base-url",
            &llm.uri(),
        ])
        .env("CI_SERVER_URL", "https://gitlab.example.com")
        .env("CI_PROJECT_PATH", "group/repo")
        .env("OPENAI_API_KEY", "test")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITLAB_TOKEN")
        .env_remove("CI_JOB_TOKEN")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("Release range: HEAD^1..HEAD"));

    let out = Command::new(env!("CARGO_BIN_EXE_communique"))
        .current_dir(&source)
        .args(["generate", "--dry-run"])
        .env_remove("CI_COMMIT_TAG")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("a target ref is required"));
}
