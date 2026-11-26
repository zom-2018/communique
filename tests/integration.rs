use std::path::Path;
use std::process::Command;

use serde_json::json;

async fn gitlab_identification_mock() -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": null, "tool_calls": [{"id": "1", "type": "function", "function": {"name": "submit_release_notes", "arguments": "{\"changelog\":\"Changes\",\"release_title\":\"Release\",\"release_body\":\"Changes\"}"}}]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })))
        .mount(&server).await;
    server
}

fn gitlab_identification_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("README"), "initial").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "initial"]);
    git(repo, &["tag", "v1"]);
    std::fs::write(repo.join("README"), "changed").unwrap();
    git(repo, &["commit", "-am", "change"]);
    git(repo, &["tag", "v2"]);
    dir
}

#[tokio::test]
async fn gitlab_remote_and_ci_identification() {
    let server = gitlab_identification_mock().await;
    for (remote, ci) in [
        ("https://gitlab.example.com/group/sub/project.git", false),
        ("git@gitlab.example.com:group/sub/project.git", false),
        ("git@code.example.com:other/repo.git", true),
    ] {
        let dir = gitlab_identification_repo();
        let repo = dir.path();
        git(repo, &["remote", "add", "origin", remote]);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_communique"));
        cmd.current_dir(repo)
            .args([
                "generate",
                "v2",
                "v1",
                "--dry-run",
                "--provider",
                "openai",
                "--model",
                "test",
                "--base-url",
                &server.uri(),
                "--output",
                "out.md",
            ])
            .env("OPENAI_API_KEY", "test")
            .env("CLX_NO_PROGRESS", "1")
            .env_remove("GITHUB_TOKEN")
            .env_remove("CI_SERVER_URL")
            .env_remove("CI_PROJECT_PATH");
        if ci {
            cmd.env("CI_SERVER_URL", "https://code.example.com")
                .env("CI_PROJECT_PATH", "group/sub/project");
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "remote {remote}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::fs::read_to_string(repo.join("out.md"))
                .unwrap()
                .contains("Changes")
        );
        let requests = server.received_requests().await.unwrap();
        let request = requests.last().unwrap();
        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("group/sub/project"), "{body}");
    }
}

#[test]
fn test_native_completions_without_project_configuration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("communique.toml"), "not valid toml [").unwrap();
    let bin = env!("CARGO_BIN_EXE_communique");
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let output = Command::new(bin)
            .current_dir(dir.path())
            .args(["completion", shell])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("__complete_word__"));
        let candidates = Command::new(bin)
            .current_dir(dir.path())
            .args([
                "__complete_word__",
                "--shell",
                shell,
                "--line",
                "communique ge",
            ])
            .output()
            .unwrap();
        assert!(candidates.status.success(), "{:?}", candidates);
        assert!(String::from_utf8_lossy(&candidates.stdout).contains("generate"));
    }
    let invalid = Command::new(bin)
        .current_dir(dir.path())
        .args(["completion", "unknown"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("unsupported shell"));
}

#[test]
fn test_sponsors_command() {
    let bin = env!("CARGO_BIN_EXE_communique");

    let result = Command::new(bin)
        .arg("sponsors")
        .env("CLX_NO_PROGRESS", "1")
        .output()
        .expect("failed to run communique");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "communique failed with status {}:\n{}",
        result.status,
        stderr
    );
    assert!(stdout.contains("entire.io"), "stdout: {stdout}");
    assert!(stdout.contains("Omacom Foundation"), "stdout: {stdout}");
    assert!(
        stdout.contains("https://jdx.dev/sponsors.html"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("https://en.dev/sponsor.html"),
        "stdout: {stdout}"
    );
}

#[test]
fn test_init_command() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init"]);

    let result = Command::new(env!("CARGO_BIN_EXE_communique"))
        .current_dir(dir.path())
        .arg("init")
        .env("CLX_NO_PROGRESS", "1")
        .output()
        .expect("failed to run communique init");

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "communique init failed: {stderr}");
    assert!(dir.path().join("communique.toml").is_file());
}

/// Spin up a wiremock server that responds to OpenAI chat completions
/// with a `submit_release_notes` tool call, then run the full CLI
/// pipeline against a temporary git repo.
#[tokio::test]
async fn test_generate_end_to_end() {
    generate_end_to_end(false).await;
}

#[tokio::test]
async fn test_cli_headers_override_environment_case_insensitively() {
    generate_end_to_end(true).await;
}

async fn generate_end_to_end(custom_headers: bool) {
    let server = wiremock::MockServer::start().await;

    // Mock the OpenAI chat completions endpoint
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_release_notes",
                            "arguments": serde_json::to_string(&json!({
                                "changelog": "### Added\n- New feature",
                                "release_title": "First Release",
                                "release_body": "This is the first release.\n\n### Added\n- New feature"
                            })).unwrap()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50}
        })))
        .mount(&server)
        .await;

    // Set up a temp git repo with commits and tags
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init"]);
    git(repo, &["config", "user.email", "test@test.com"]);
    git(repo, &["config", "user.name", "Test"]);

    std::fs::write(repo.join("README.md"), "# hello").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", "initial commit"]);
    git(repo, &["tag", "v0.1.0"]);

    std::fs::write(repo.join("src.rs"), "fn main() {}").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", "add feature"]);
    git(repo, &["tag", "v0.2.0"]);

    let output_file = repo.join("output.md");

    // Build the binary path
    let bin = env!("CARGO_BIN_EXE_communique");

    let mut command = Command::new(bin);
    command
        .current_dir(repo)
        .args([
            "generate",
            "v0.2.0",
            "v0.1.0",
            "--dry-run",
            "--repo",
            "test/repo",
            "--provider",
            "openai",
            "--model",
            "test-model",
            "--base-url",
            &server.uri(),
            "--output",
            output_file.to_str().unwrap(),
        ])
        .env("OPENAI_API_KEY", "test-key")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN");
    if custom_headers {
        // Set the environment only on the CLI subprocess so parallel tests stay isolated.
        command
            .env(
                "OPENAI_CUSTOM_HEADERS",
                json!({
                    "COPILOT-INTEGRATION-ID": "environment-value",
                    "authorization": "environment-token",
                    "X-Environment-Only": "preserved",
                })
                .to_string(),
            )
            .args([
                "--header",
                "copilot-integration-id: cli-value",
                "--header",
                "Authorization: cli-token",
                "--header",
                "X-Cli-Only: added",
            ]);
    } else {
        command.env_remove("OPENAI_CUSTOM_HEADERS");
    }

    let result = command.output().expect("failed to run communique");

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "communique failed with status {}:\n{}",
        result.status,
        stderr
    );

    if custom_headers {
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let headers = &requests[0].headers;
        // Compare every value, catching accidental appending of the environment value too.
        for (name, expected) in [
            ("copilot-integration-id", "cli-value"),
            ("authorization", "cli-token"),
            ("x-environment-only", "preserved"),
            ("x-cli-only", "added"),
        ] {
            let values: Vec<_> = headers
                .get_all(name)
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect();
            assert_eq!(values, [expected], "unexpected values for {name}");
        }
    }

    let output = std::fs::read_to_string(&output_file).expect("output file should exist");
    assert!(output.contains("First Release"), "output: {output}");
    assert!(
        output.contains("### Added"),
        "output should contain changelog sections: {output}"
    );
}

#[tokio::test]
async fn test_generate_head_changelog_updates_unreleased() {
    let server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_release_notes",
                            "arguments": serde_json::to_string(&json!({
                                "changelog": "### Added\n- Unreleased feature",
                                "release_title": "HEAD - Draft Feature",
                                "release_body": "Draft feature release notes."
                            })).unwrap()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 80, "completion_tokens": 40}
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init"]);
    git(repo, &["config", "user.email", "test@test.com"]);
    git(repo, &["config", "user.name", "Test"]);

    std::fs::write(repo.join("README.md"), "# hello").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", "initial"]);
    git(repo, &["tag", "v0.1.0"]);

    std::fs::write(repo.join("feature.rs"), "// feature").unwrap();
    std::fs::write(
        repo.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n### Changed\n- Old draft\n\n## [0.1.0] - 2026-01-01\n### Added\n- Initial release\n",
    )
    .unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", "add unreleased feature"]);

    let output_file = repo.join("output.md");
    let bin = env!("CARGO_BIN_EXE_communique");

    let result = Command::new(bin)
        .current_dir(repo)
        .args([
            "generate",
            "HEAD",
            "--changelog",
            "--repo",
            "test/repo",
            "--provider",
            "openai",
            "--model",
            "test-model",
            "--base-url",
            &server.uri(),
            "--output",
            output_file.to_str().unwrap(),
        ])
        .env("OPENAI_API_KEY", "test-key")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN")
        .output()
        .expect("failed to run communique");

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "communique failed: {}", stderr);

    let changelog = std::fs::read_to_string(repo.join("CHANGELOG.md")).unwrap();
    assert!(
        changelog.contains("## [Unreleased]\n\n### Added\n- Unreleased feature"),
        "changelog: {changelog}"
    );
    assert!(
        changelog.contains("## [0.1.0] - 2026-01-01\n### Added\n- Initial release"),
        "changelog should preserve released sections: {changelog}"
    );
    assert!(!changelog.contains("## [HEAD]"));
    assert!(!changelog.contains("## HEAD"));

    let output = std::fs::read_to_string(&output_file).unwrap();
    assert!(
        output.contains("# Unreleased: Draft Feature"),
        "output: {output}"
    );
    assert!(
        !output.contains("HEAD:"),
        "output should not expose HEAD label: {output}"
    );
}

/// Same as above but with `--concise` to verify changelog-only output.
#[tokio::test]
async fn test_generate_concise() {
    let server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_release_notes",
                            "arguments": serde_json::to_string(&json!({
                                "changelog": "### Fixed\n- Bug fix",
                                "release_title": "Patch Release",
                                "release_body": "Fixed a bug.\n\n### Fixed\n- Bug fix"
                            })).unwrap()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 50, "completion_tokens": 25}
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init"]);
    git(repo, &["config", "user.email", "test@test.com"]);
    git(repo, &["config", "user.name", "Test"]);

    std::fs::write(repo.join("README.md"), "# hello").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", "initial"]);
    git(repo, &["tag", "v1.0.0"]);

    std::fs::write(repo.join("fix.rs"), "// fix").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", "fix bug"]);
    git(repo, &["tag", "v1.0.1"]);

    let output_file = repo.join("output.md");
    let bin = env!("CARGO_BIN_EXE_communique");

    let result = Command::new(bin)
        .current_dir(repo)
        .args([
            "generate",
            "v1.0.1",
            "--dry-run",
            "--concise",
            "--repo",
            "test/repo",
            "--provider",
            "openai",
            "--model",
            "test-model",
            "--base-url",
            &server.uri(),
            "--output",
            output_file.to_str().unwrap(),
        ])
        .env("OPENAI_API_KEY", "test-key")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN")
        .output()
        .expect("failed to run communique");

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "communique failed: {}", stderr);

    let output = std::fs::read_to_string(&output_file).unwrap();
    // Concise mode should output only the changelog, not the title
    assert!(output.contains("### Fixed"), "output: {output}");
    assert!(
        !output.contains("Patch Release"),
        "concise output should not contain release title: {output}"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn github_agent_and_draft_safety_net() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let server = wiremock::MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let turn = calls.clone();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(move |_: &wiremock::Request| {
            let tools = if turn.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    json!({"id":"pr", "type":"function", "function":{"name":"get_pr", "arguments":"{\"number\":42}"}}),
                    json!({"id":"diff", "type":"function", "function":{"name":"get_pr_diff", "arguments":"{\"number\":42}"}}),
                    json!({"id":"issue", "type":"function", "function":{"name":"get_issue", "arguments":"{\"number\":7}"}}),
                ]
            } else {
                vec![json!({"id":"submit", "type":"function", "function":{"name":"submit_release_notes", "arguments":json!({"release_title":"Safety release", "release_body":"Reviewed release body.", "changelog":"### Added\n- Safety feature"}).to_string()}})]
            };
            wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "choices":[{"message":{"content":null,"tool_calls":tools},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":10,"completion_tokens":10}
            }))
        }).mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init"]);
    git(repo, &["config", "user.email", "test@test.com"]);
    git(repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("file"), "old").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "initial"]);
    git(repo, &["tag", "v1.0.0"]);
    std::fs::write(repo.join("file"), "new").unwrap();
    git(repo, &["commit", "-am", "feature"]);
    git(repo, &["tag", "v2.0.0"]);
    let output = Command::new(env!("CARGO_BIN_EXE_communique"))
        .current_dir(repo)
        .args([
            "generate",
            "v2.0.0",
            "v1.0.0",
            "--repo",
            "owner/repo",
            "--provider",
            "openai",
            "--model",
            "test",
            "--base-url",
            &server.uri(),
            "--dry-run",
            "--draft",
            "draft.json",
        ])
        .env("OPENAI_API_KEY", "test")
        .env("CLX_NO_PROGRESS", "1")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let request: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let messages = request["messages"].as_array().unwrap();
    for (id, expected) in [
        ("pr", "get_pr requires GITHUB_TOKEN to be set"),
        ("diff", "get_pr_diff requires GITHUB_TOKEN to be set"),
        ("issue", "get_issue requires GITHUB_TOKEN to be set"),
    ] {
        let result = messages.iter().find(|m| m["tool_call_id"] == id).unwrap();
        assert!(
            result["content"].as_str().unwrap().contains(expected),
            "{result}"
        );
    }
    let draft: serde_json::Value =
        serde_json::from_slice(&std::fs::read(repo.join("draft.json")).unwrap()).unwrap();
    assert_eq!(draft["repo"], "owner/repo");
    assert_eq!(draft["tag"], "v2.0.0");
    assert_eq!(draft["previous_ref"], "v1.0.0");
    assert_eq!(draft["release_body"], "Reviewed release body.");
}
