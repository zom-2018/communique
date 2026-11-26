use std::{future::Future, pin::Pin};

use crate::error::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Release {
    pub id: u64,
    #[allow(dead_code)]
    pub tag_name: String,
    #[allow(dead_code)]
    pub name: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub user: User,
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub user: User,
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct Label {
    pub name: String,
}

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Operations required by the agent and release workflow.
pub trait GitProvider: Sync {
    fn verify_project_resource<'a>(&'a self, _path: &'a str) -> ProviderFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
    #[allow(dead_code)]
    fn merge_requests_for_commit<'a>(
        &'a self,
        _sha: &'a str,
    ) -> ProviderFuture<'a, Vec<PullRequest>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    #[allow(dead_code)]
    fn related_issues(&self, _number: u64) -> ProviderFuture<'_, Vec<Issue>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn get_pr(&self, number: u64) -> ProviderFuture<'_, PullRequest>;
    fn get_pr_diff(&self, number: u64) -> ProviderFuture<'_, String>;
    fn get_issue(&self, number: u64) -> ProviderFuture<'_, Issue>;
    fn tag_commit<'a>(&'a self, tag: &'a str) -> ProviderFuture<'a, String>;
    fn get_release_by_tag<'a>(&'a self, tag: &'a str) -> ProviderFuture<'a, Option<Release>>;
    fn list_recent_releases(&self, count: u8) -> ProviderFuture<'_, Vec<Release>>;
    fn create_release<'a>(
        &'a self,
        _tag: &'a str,
        _title: &'a str,
        _body: &'a str,
        _preserve_sections: bool,
    ) -> ProviderFuture<'a, ()> {
        Box::pin(async {
            Err(crate::error::Error::Tool(
                "Creating releases is unsupported by this provider".into(),
            ))
        })
    }
    fn update_release<'a>(
        &'a self,
        id: u64,
        tag: &'a str,
        title: Option<&'a str>,
        body: Option<&'a str>,
    ) -> ProviderFuture<'a, ()>;
}

impl GitProvider for crate::github::GitHubProvider {
    fn get_pr(&self, number: u64) -> ProviderFuture<'_, PullRequest> {
        Box::pin(self.get_pr(number))
    }
    fn get_pr_diff(&self, number: u64) -> ProviderFuture<'_, String> {
        Box::pin(self.get_pr_diff(number))
    }
    fn get_issue(&self, number: u64) -> ProviderFuture<'_, Issue> {
        Box::pin(self.get_issue(number))
    }
    fn tag_commit<'a>(&'a self, tag: &'a str) -> ProviderFuture<'a, String> {
        Box::pin(self.tag_commit(tag))
    }
    fn get_release_by_tag<'a>(&'a self, tag: &'a str) -> ProviderFuture<'a, Option<Release>> {
        Box::pin(self.get_release_by_tag(tag))
    }
    fn list_recent_releases(&self, count: u8) -> ProviderFuture<'_, Vec<Release>> {
        Box::pin(self.list_recent_releases(count))
    }
    fn update_release<'a>(
        &'a self,
        id: u64,
        tag: &'a str,
        title: Option<&'a str>,
        body: Option<&'a str>,
    ) -> ProviderFuture<'a, ()> {
        Box::pin(self.update_release(id, tag, title, body))
    }
}

/// Construct the GitLab provider using the project's resolved token source.
pub fn gitlab_provider(
    project: &crate::git::ProjectIdentity,
    allow_empty: bool,
) -> crate::error::Result<Option<Box<dyn GitProvider>>> {
    let (name, job_token) = match &project.token_source {
        crate::git::TokenSource::JobToken => ("CI_JOB_TOKEN", true),
        crate::git::TokenSource::Environment(name) => (name.as_str(), false),
    };
    std::env::var(name)
        .ok()
        .filter(|token| allow_empty || !token.is_empty())
        .map(|token| {
            crate::gitlab::GitLabProvider::new(
                &project.api_url,
                project.project_id.as_deref().unwrap_or(&project.path),
                token,
                job_token,
            )
            .map(|client| Box::new(client) as Box<dyn GitProvider>)
        })
        .transpose()
}

pub fn gitlab_token_name(project: &crate::git::ProjectIdentity) -> &str {
    match &project.token_source {
        crate::git::TokenSource::JobToken => "CI_JOB_TOKEN",
        crate::git::TokenSource::Environment(name) => name,
    }
}
