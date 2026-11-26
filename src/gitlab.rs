use crate::error::{Error, Result};
use crate::git_provider::{GitProvider, Label, ProviderFuture, PullRequest, User};
use serde::Deserialize;

#[derive(Deserialize)]
struct MrAuthor {
    username: String,
}
#[derive(Deserialize)]
struct GitLabIssue {
    iid: u64,
    title: String,
    description: Option<String>,
    state: String,
    author: MrAuthor,
    labels: Vec<String>,
}
impl From<GitLabIssue> for crate::git_provider::Issue {
    fn from(issue: GitLabIssue) -> Self {
        Self {
            number: issue.iid,
            title: issue.title,
            body: issue.description,
            state: issue.state,
            user: User {
                login: issue.author.username,
            },
            labels: issue
                .labels
                .into_iter()
                .map(|name| Label { name })
                .collect(),
        }
    }
}
#[derive(Deserialize)]
struct Mr {
    iid: u64,
    title: String,
    description: Option<String>,
    author: MrAuthor,
    labels: Vec<String>,
}
impl From<Mr> for PullRequest {
    fn from(mr: Mr) -> Self {
        Self {
            number: mr.iid,
            title: mr.title,
            body: mr.description,
            user: User {
                login: mr.author.username,
            },
            labels: mr.labels.into_iter().map(|name| Label { name }).collect(),
        }
    }
}

#[derive(Deserialize)]
struct GitLabRelease {
    tag_name: String,
    name: Option<String>,
    description: Option<String>,
}
impl From<GitLabRelease> for crate::git_provider::Release {
    fn from(value: GitLabRelease) -> Self {
        Self {
            id: 0,
            tag_name: value.tag_name,
            name: value.name,
            body: value.description,
        }
    }
}

pub struct GitLabProvider {
    client: reqwest::Client,
    base: reqwest::Url,
    token: String,
    job_token: bool,
}

fn gitlab_retry_config() -> crate::retry::RetryConfig {
    crate::retry::RetryConfig {
        max_retries: 2,
        initial_delay_ms: 250,
        max_delay_ms: 1_000,
    }
}

impl GitLabProvider {
    fn endpoint(&self, segments: &[&str]) -> reqwest::Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("validated URL")
            .extend(segments.iter().copied());
        url
    }

    async fn get(&self, segments: &[&str], page: Option<u32>) -> Result<reqwest::Response> {
        let mut url = self.endpoint(segments);
        if let Some(page) = page {
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
        }
        self.get_url(url).await
    }

    async fn get_url(&self, url: reqwest::Url) -> Result<reqwest::Response> {
        let response =
            crate::retry::retry_request_with_config("GitLab API", &gitlab_retry_config(), || {
                self.client
                    .get(url.clone())
                    .header(
                        if self.job_token {
                            "JOB-TOKEN"
                        } else {
                            "PRIVATE-TOKEN"
                        },
                        &self.token,
                    )
                    .send()
            })
            .await?;
        if !response.status().is_success() {
            return Err(Error::Tool(format!("GitLab API: {}", response.status())));
        }
        Ok(response)
    }

    async fn verify_blob(&self, path: &str) -> bool {
        let Some((reference, file)) = path.split_once('/') else {
            return false;
        };
        let Ok(reference) = percent_encoding::percent_decode_str(reference).decode_utf8() else {
            return false;
        };
        let Ok(file) = percent_encoding::percent_decode_str(file).decode_utf8() else {
            return false;
        };
        if reference.is_empty() || file.is_empty() {
            return false;
        }
        let mut url = self.endpoint(&["repository", "files", &file, "raw"]);
        url.query_pairs_mut().append_pair("ref", &reference);
        self.get_url(url).await.is_ok()
    }

    async fn pages<T: serde::de::DeserializeOwned>(&self, segments: &[&str]) -> Result<Vec<T>> {
        let mut page = 1;
        let mut values = Vec::new();
        loop {
            let response = self.get(segments, Some(page)).await?;
            let next = response
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
                .map(str::parse::<u32>)
                .transpose()
                .map_err(|_| Error::Tool("invalid GitLab pagination".into()))?;
            values.extend(response.json::<Vec<T>>().await?);
            match next {
                Some(n) if n > page => page = n,
                Some(_) => return Err(Error::Tool("invalid GitLab pagination".into())),
                None => return Ok(values),
            }
        }
    }

    async fn mr_diff(&self, number: u64) -> Result<String> {
        #[derive(Deserialize)]
        struct Diff {
            old_path: String,
            new_path: String,
            diff: String,
        }
        let diffs: Vec<Diff> = self
            .pages(&["merge_requests", &number.to_string(), "diffs"])
            .await?;
        Ok(diffs
            .into_iter()
            .map(|diff| {
                format!(
                    "diff --git a/{} b/{}\n{}\n",
                    diff.old_path, diff.new_path, diff.diff
                )
            })
            .collect())
    }

    async fn issue(&self, number: u64) -> Result<crate::git_provider::Issue> {
        let issue: GitLabIssue = self
            .get(&["issues", &number.to_string()], None)
            .await?
            .json()
            .await?;
        Ok(issue.into())
    }
    pub fn new(api_url: &str, project: &str, token: String, job_token: bool) -> Result<Self> {
        let mut base = reqwest::Url::parse(api_url)
            .map_err(|_| Error::Config("invalid GitLab API URL".into()))?;
        base.path_segments_mut()
            .map_err(|_| Error::Config("invalid GitLab API URL".into()))?
            .pop_if_empty()
            .push("projects")
            .push(project);
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("communique/0.1");
        if let Some(path) = std::env::var_os("COMMUNIQUE_CA_CERT") {
            let pem = std::fs::read(path)
                .map_err(|_| Error::Config("cannot read COMMUNIQUE_CA_CERT".into()))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| Error::Config("invalid COMMUNIQUE_CA_CERT PEM".into()))?;
            if certs.is_empty() {
                return Err(Error::Config(
                    "COMMUNIQUE_CA_CERT contains no certificates".into(),
                ));
            }
            for cert in certs {
                builder = builder.add_root_certificate(cert);
            }
        }
        let client = builder.build()?;
        Ok(Self {
            client,
            base,
            token,
            job_token,
        })
    }

    async fn get_mr(&self, number: u64) -> Result<PullRequest> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("validated URL")
            .push("merge_requests")
            .push(&number.to_string());
        let response =
            crate::retry::retry_request_with_config("GitLab API", &gitlab_retry_config(), || {
                self.client
                    .get(url.clone())
                    .header(
                        if self.job_token {
                            "JOB-TOKEN"
                        } else {
                            "PRIVATE-TOKEN"
                        },
                        &self.token,
                    )
                    .send()
            })
            .await?;
        if !response.status().is_success() {
            return Err(Error::Tool(format!(
                "GitLab merge request: {}",
                response.status()
            )));
        }
        let mr: Mr = response.json().await?;
        Ok(mr.into())
    }

    async fn releases(&self, tag: Option<&str>, count: u8) -> Result<reqwest::Response> {
        let mut url = self.base.clone();
        {
            let mut segments = url.path_segments_mut().expect("validated URL");
            segments.push("releases");
            if let Some(tag) = tag {
                segments.push(tag);
            }
        }
        if tag.is_none() {
            url.query_pairs_mut()
                .append_pair("per_page", &count.to_string());
        }
        crate::retry::retry_request_with_config("GitLab API", &gitlab_retry_config(), || {
            self.client
                .get(url.clone())
                .header(
                    if self.job_token {
                        "JOB-TOKEN"
                    } else {
                        "PRIVATE-TOKEN"
                    },
                    &self.token,
                )
                .send()
        })
        .await
    }
}

impl GitProvider for GitLabProvider {
    fn verify_project_resource<'a>(&'a self, path: &'a str) -> ProviderFuture<'a, bool> {
        Box::pin(async move {
            let Some((kind, id)) = path.split_once('/') else {
                return Ok(false);
            };
            if kind == "blob" {
                return Ok(self.verify_blob(id).await);
            }
            let Ok(id) = percent_encoding::percent_decode_str(id).decode_utf8() else {
                return Ok(false);
            };
            let id = id.as_ref();
            if id.is_empty() {
                return Ok(false);
            }
            let segments: Vec<&str> = match kind {
                "merge_requests" | "issues" if id.parse::<u64>().is_ok() => vec![kind, id],
                "commit" => vec!["repository", "commits", id],
                "tags" => vec!["repository", "tags", id],
                "releases" => vec!["releases", id],
                _ => return Ok(false),
            };
            Ok(self.get(&segments, None).await.is_ok())
        })
    }
    fn related_issues(&self, number: u64) -> ProviderFuture<'_, Vec<crate::git_provider::Issue>> {
        Box::pin(async move {
            let issues: Vec<GitLabIssue> = self
                .pages(&["merge_requests", &number.to_string(), "related_issues"])
                .await?;
            Ok(issues.into_iter().map(Into::into).collect())
        })
    }
    fn merge_requests_for_commit<'a>(
        &'a self,
        sha: &'a str,
    ) -> ProviderFuture<'a, Vec<PullRequest>> {
        Box::pin(async move {
            let mrs: Vec<Mr> = self
                .pages(&["repository", "commits", sha, "merge_requests"])
                .await?;
            Ok(mrs.into_iter().map(Into::into).collect())
        })
    }
    fn get_pr(&self, number: u64) -> ProviderFuture<'_, PullRequest> {
        Box::pin(self.get_mr(number))
    }
    fn get_pr_diff(&self, number: u64) -> ProviderFuture<'_, String> {
        Box::pin(self.mr_diff(number))
    }
    fn get_issue(&self, number: u64) -> ProviderFuture<'_, crate::git_provider::Issue> {
        Box::pin(self.issue(number))
    }
    fn tag_commit<'a>(&'a self, tag: &'a str) -> ProviderFuture<'a, String> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Tag {
                commit: Commit,
            }
            #[derive(Deserialize)]
            struct Commit {
                id: String,
            }
            let tag: Tag = self
                .get(&["repository", "tags", tag], None)
                .await?
                .json()
                .await?;
            Ok(tag.commit.id)
        })
    }
    fn get_release_by_tag<'a>(
        &'a self,
        tag: &'a str,
    ) -> ProviderFuture<'a, Option<crate::git_provider::Release>> {
        Box::pin(async move {
            let response = self.releases(Some(tag), 0).await?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err(Error::Tool(format!(
                    "GitLab release: {}",
                    response.status()
                )));
            }
            Ok(Some(response.json::<GitLabRelease>().await?.into()))
        })
    }
    fn list_recent_releases(
        &self,
        count: u8,
    ) -> ProviderFuture<'_, Vec<crate::git_provider::Release>> {
        Box::pin(async move {
            let response = self.releases(None, count).await?;
            if !response.status().is_success() {
                return Err(Error::Tool(format!(
                    "GitLab releases: {}",
                    response.status()
                )));
            }
            Ok(response
                .json::<Vec<GitLabRelease>>()
                .await?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }
    fn create_release<'a>(
        &'a self,
        tag: &'a str,
        title: &'a str,
        body: &'a str,
        preserve_sections: bool,
    ) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let initial_body = if preserve_sections {
                crate::workflow::merge_body("", body).map_err(|e| Error::Tool(e.to_string()))?
            } else {
                body.to_string()
            };
            let mut url = self.base.clone();
            url.path_segments_mut()
                .expect("validated URL")
                .push("releases");
            let result = self
                .client
                .post(url)
                .header(
                    if self.job_token {
                        "JOB-TOKEN"
                    } else {
                        "PRIVATE-TOKEN"
                    },
                    &self.token,
                )
                .json(&serde_json::json!({"tag_name":tag,"name":title,"description":initial_body}))
                .send()
                .await;
            match result {
                Ok(response) if response.status().is_success() => Ok(()),
                Ok(response)
                    if response.status().as_u16() != 400
                        && response.status().as_u16() != 409
                        && !response.status().is_server_error() =>
                {
                    Err(Error::Tool(format!(
                        "GitLab release creation: {}",
                        response.status()
                    )))
                }
                other => {
                    let failure = match other {
                        Ok(response) => format!("{}", response.status()),
                        Err(_) => "connection failed".to_string(),
                    };
                    match self.get_release_by_tag(tag).await? {
                        Some(release) => {
                            let merged = if preserve_sections {
                                crate::workflow::merge_body(
                                    release.body.as_deref().unwrap_or_default(),
                                    body,
                                )
                                .map_err(|e| Error::Tool(e.to_string()))?
                            } else {
                                body.to_string()
                            };
                            self.update_release(
                                release.id,
                                tag,
                                if preserve_sections { None } else { Some(title) },
                                Some(&merged),
                            )
                            .await
                        }
                        None => Err(Error::Tool(format!("GitLab release creation: {failure}"))),
                    }
                }
            }
        })
    }
    fn update_release<'a>(
        &'a self,
        _: u64,
        tag: &'a str,
        title: Option<&'a str>,
        body: Option<&'a str>,
    ) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let mut url = self.base.clone();
            url.path_segments_mut()
                .expect("validated URL")
                .push("releases")
                .push(tag);
            let mut payload = serde_json::Map::new();
            if let Some(title) = title {
                payload.insert("name".into(), title.into());
            }
            if let Some(body) = body {
                payload.insert("description".into(), body.into());
            }
            let response = crate::retry::retry_request_with_config(
                "GitLab API",
                &gitlab_retry_config(),
                || {
                    self.client
                        .put(url.clone())
                        .header(
                            if self.job_token {
                                "JOB-TOKEN"
                            } else {
                                "PRIVATE-TOKEN"
                            },
                            &self.token,
                        )
                        .json(&payload)
                        .send()
                },
            )
            .await?;
            if !response.status().is_success() {
                return Err(Error::Tool(format!(
                    "GitLab release update: {}",
                    response.status()
                )));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn gitlab_slow_request_times_out_and_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        let api = MockServer::start().await;
        let calls = AtomicUsize::new(0);
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/merge_requests/1"))
            .respond_with(move |_: &wiremock::Request| {
                let response = ResponseTemplate::new(200).set_body_json(json!({"iid":1,"title":"Recovered","description":null,"author":{"username":"alice"},"labels":[]}));
                if calls.fetch_add(1, Ordering::SeqCst) == 0 { response.set_delay(Duration::from_secs(13)) } else { response }
            })
            .expect(2)
            .mount(&api).await;
        let client = GitLabProvider::new(
            &format!("{}/api/v4", api.uri()),
            "42",
            "token".into(),
            false,
        )
        .unwrap();
        let mr = tokio::time::timeout(Duration::from_secs(13), client.get_pr(1))
            .await
            .expect("request should time out and retry")
            .unwrap();
        assert_eq!(mr.title, "Recovered");
    }

    #[tokio::test]
    async fn gitlab_tag_lookup_and_release_update() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/group%2Frepo/repository/tags/release%2Fv1",
            ))
            .and(header("PRIVATE-TOKEN", "token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"commit":{"id":"abc123"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v4/projects/group%2Frepo/releases/release%2Fv1"))
            .and(header("PRIVATE-TOKEN", "token"))
            .and(body_json(json!({"name":"Title","description":"Body"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = GitLabProvider::new(
            &format!("{}/api/v4", server.uri()),
            "group/repo",
            "token".into(),
            false,
        )
        .unwrap();
        assert_eq!(client.tag_commit("release/v1").await.unwrap(), "abc123");
        client
            .update_release(0, "release/v1", Some("Title"), Some("Body"))
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn gitlab_auth_retries_and_release_reads() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let api = MockServer::start().await;
        let calls = AtomicUsize::new(0);
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/releases/v1"))
            .and(header("JOB-TOKEN", "job-secret"))
            .respond_with(move |_: &wiremock::Request| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429).insert_header("retry-after", "0")
                } else {
                    ResponseTemplate::new(200).set_body_json(
                        json!({"tag_name":"v1","name":"Version 1","description":"Notes"}),
                    )
                }
            })
            .expect(2)
            .mount(&api)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/issues/7"))
            .and(header("JOB-TOKEN", "job-secret"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&api)
            .await;
        let client = GitLabProvider::new(
            &format!("{}/api/v4", api.uri()),
            "42",
            "job-secret".into(),
            true,
        )
        .unwrap();
        let release = client.get_release_by_tag("v1").await.unwrap().unwrap();
        assert_eq!(release.body.as_deref(), Some("Notes"));
        let err = client.get_issue(7).await.unwrap_err().to_string();
        assert!(err.contains("403"));
        assert!(!err.contains("job-secret"));
    }

    #[tokio::test]
    async fn gitlab_redirect_does_not_forward_token() {
        let api = MockServer::start().await;
        let other = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/merge_requests/1"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/capture", other.uri())),
            )
            .mount(&api)
            .await;
        let client = GitLabProvider::new(
            &format!("{}/api/v4", api.uri()),
            "42",
            "secret".into(),
            false,
        )
        .unwrap();
        assert!(client.get_pr(1).await.is_err());
        assert!(other.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn gitlab_commit_mr_pagination_and_partial_permissions() {
        use wiremock::matchers::query_param;
        let api = MockServer::start().await;
        for (page, iid, next) in [("1", 11, "2"), ("2", 12, "")] {
            Mock::given(method("GET"))
                .and(path("/api/v4/projects/42/repository/commits/abc123/merge_requests"))
                .and(query_param("page", page))
                .and(header("JOB-TOKEN", "job"))
                .respond_with(ResponseTemplate::new(200).insert_header("x-next-page", next).set_body_json(json!([{"iid":iid,"title":format!("MR {iid}"),"description":"details","author":{"username":"alice"},"labels":["feature"]}])))
                .mount(&api).await;
        }
        let client =
            GitLabProvider::new(&format!("{}/api/v4", api.uri()), "42", "job".into(), true)
                .unwrap();
        let mrs = client.merge_requests_for_commit("abc123").await.unwrap();
        assert_eq!(
            mrs.iter().map(|mr| mr.number).collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(mrs[1].title, "MR 12");
    }

    #[tokio::test]
    async fn gitlab_related_issues_are_returned() {
        let api = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/v4/projects/42/merge_requests/12/related_issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"iid":7,"title":"Problem","description":"Context","state":"opened","author":{"username":"bob"},"labels":[]}]))).mount(&api).await;
        let client = GitLabProvider::new(
            &format!("{}/api/v4", api.uri()),
            "42",
            "token".into(),
            false,
        )
        .unwrap();
        let issues = client.related_issues(12).await.unwrap();
        assert_eq!(issues[0].number, 7);
        assert_eq!(issues[0].title, "Problem");
    }
}
