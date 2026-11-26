use log::warn;
use regex::Regex;

/// Extract all markdown links and bare URLs from text.
fn extract_urls(text: &str) -> Vec<String> {
    let re = Regex::new(r"https?://[^\s\)\]>]+").unwrap();
    re.find_iter(text)
        .map(|m| m.as_str().trim_end_matches(['.', ',', ';']).to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

/// Verify all URLs in the given texts return non-404 responses.
/// Returns a list of broken URLs.
#[cfg(test)]
pub async fn verify(texts: &[&str]) -> Vec<(String, String)> {
    verify_for_project(texts, None, None).await
}

pub async fn verify_for_project(
    texts: &[&str],
    project: Option<&str>,
    provider: Option<&dyn crate::git_provider::GitProvider>,
) -> Vec<(String, String)> {
    let mut all_urls = Vec::new();
    for text in texts {
        all_urls.extend(extract_urls(text));
    }

    if all_urls.is_empty() {
        return Vec::new();
    }

    let client = reqwest::Client::builder()
        .user_agent("communique/0.1 link-checker")
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap();

    let mut broken = Vec::new();
    for url in &all_urls {
        let requested = reqwest::Url::parse(url).ok();
        let resource = project
            .and_then(|p| reqwest::Url::parse(p).ok())
            .zip(requested.as_ref())
            .and_then(|(base, candidate)| {
                if base.origin() != candidate.origin() {
                    return None;
                }
                candidate
                    .path()
                    .strip_prefix(&format!("{}/-/", base.path().trim_end_matches('/')))
                    .map(str::to_owned)
            });
        let response = match client.head(url).send().await {
            Ok(resp) if resp.status() == 405 => client.get(url).send().await,
            result => result,
        };
        let needs_project_api = response.as_ref().map_or(true, |resp| {
            matches!(resp.status().as_u16(), 401 | 403 | 404)
                || requested.as_ref().is_some_and(|initial| {
                    resp.url().origin() != initial.origin() || resp.url().path() != initial.path()
                })
        });
        if resource.is_some() && needs_project_api {
            if !verify_resource(provider, resource.as_deref()).await {
                warn!("GitLab project link is unverifiable without authenticated access");
            }
            continue;
        }
        match response {
            Ok(resp) if resp.status() == 404 => {
                warn!("broken link: {url} (404)");
                broken.push((url.clone(), "404".into()));
            }
            Err(e) => {
                warn!("broken link: {url} ({e})");
                broken.push((url.clone(), e.to_string()));
            }
            _ => {}
        }
    }
    broken
}

async fn verify_resource(
    provider: Option<&dyn crate::git_provider::GitProvider>,
    path: Option<&str>,
) -> bool {
    match (provider, path) {
        (Some(provider), Some(path)) => provider
            .verify_project_resource(path)
            .await
            .unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_urls() {
        let text =
            "Check [docs](https://example.com/docs) and https://github.com/jdx/communique/pull/1.";
        let urls = extract_urls(text);
        assert!(urls.contains(&"https://example.com/docs".to_string()));
        assert!(urls.contains(&"https://github.com/jdx/communique/pull/1".to_string()));
    }

    #[test]
    fn test_extract_urls_dedup() {
        let text = "See https://example.com and https://example.com again.";
        let urls = extract_urls(text);
        assert_eq!(urls.len(), 1);
    }

    #[tokio::test]
    async fn test_verify_all_ok() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let url = format!("{}/page", server.uri());
        let text = format!("Check {url}");
        let broken = verify(&[&text]).await;
        assert!(broken.is_empty());
    }

    #[tokio::test]
    async fn test_verify_broken_404() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let url = format!("{}/broken", server.uri());
        let text = format!("See {url}");
        let broken = verify(&[&text]).await;
        assert_eq!(broken.len(), 1);
        assert!(broken[0].1.contains("404"));
    }

    #[tokio::test]
    async fn test_verify_empty_text() {
        let broken = verify(&["no urls here"]).await;
        assert!(broken.is_empty());
    }

    #[tokio::test]
    async fn test_verify_405_fallback_to_get_ok() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .respond_with(wiremock::ResponseTemplate::new(405))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let url = format!("{}/page", server.uri());
        let text = format!("Check {url}");
        let broken = verify(&[&text]).await;
        assert!(broken.is_empty());
    }

    #[tokio::test]
    async fn test_verify_405_fallback_to_get_404() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .respond_with(wiremock::ResponseTemplate::new(405))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let url = format!("{}/broken", server.uri());
        let text = format!("See {url}");
        let broken = verify(&[&text]).await;
        assert_eq!(broken.len(), 1);
        assert!(broken[0].1.contains("404"));
    }
}

#[cfg(test)]
mod gitlab_link_tests {
    use super::*;

    #[tokio::test]
    async fn gitlab_link_uses_api_when_web_connection_fails() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/api/v4/projects/group%2Frepo/issues/7",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::gitlab::GitLabProvider::new(
            &format!("{}/api/v4", server.uri()),
            "group/repo",
            "secret".into(),
            false,
        )
        .unwrap();
        let project = "http://127.0.0.1:1/group/repo";
        let url = format!("{project}/-/issues/7");
        assert!(
            verify_for_project(&[&url], Some(project), Some(&client))
                .await
                .is_empty()
        );
    }
}
