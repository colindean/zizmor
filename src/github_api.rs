//! A very minimal GitHub API client.
//!
//! Build on synchronous reqwest to avoid octocrab's need to taint
//! the whole codebase with async.

use anyhow::{anyhow, Result};
use reqwest::{
    blocking,
    header::{HeaderMap, ACCEPT, AUTHORIZATION, USER_AGENT},
    StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize};

use crate::state::Caches;

pub(crate) struct Client {
    api_base: &'static str,
    http: blocking::Client,
    caches: Caches,
}

impl Client {
    pub(crate) fn new(token: &str, caches: Caches) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, "zizmor".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .expect("couldn't build authorization header for GitHub client?"),
        );
        headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().unwrap());
        headers.insert(ACCEPT, "application/vnd.github+json".parse().unwrap());

        Self {
            api_base: "https://api.github.com",
            http: blocking::Client::builder()
                .default_headers(headers)
                .build()
                .expect("couldn't build GitHub client?"),
            caches,
        }
    }

    fn paginate<T: DeserializeOwned>(&self, endpoint: &str) -> reqwest::Result<Vec<T>> {
        let mut dest = vec![];
        let url = format!("{api_base}/{endpoint}", api_base = self.api_base);

        // If we were nice, we would parse GitHub's `links` header and extract
        // the remaining number of pages. But this is annoying, and we are
        // not nice, so we simply request pages until GitHub bails on us
        // and returns empty results.
        let mut pageno = 0;
        loop {
            let resp = self
                .http
                .get(&url)
                .query(&[("page", pageno), ("per_page", 100)])
                .send()?
                .error_for_status()?;

            let page = resp.json::<Vec<T>>()?;
            if page.is_empty() {
                break;
            }

            dest.extend(page);
            pageno += 1;
        }

        Ok(dest)
    }

    pub(crate) fn list_branches(&self, owner: &str, repo: &str) -> Result<Vec<Branch>> {
        self.caches
            .branch_cache
            .try_get_with((owner.into(), repo.into()), || {
                self.paginate(&format!("repos/{owner}/{repo}/branches"))
            })
            .map_err(Into::into)
    }

    pub(crate) fn list_tags(&self, owner: &str, repo: &str) -> Result<Vec<Tag>> {
        self.caches
            .tag_cache
            .try_get_with((owner.into(), repo.into()), || {
                self.paginate(&format!("repos/{owner}/{repo}/tags"))
            })
            .map_err(Into::into)
    }

    pub(crate) fn commit_for_ref(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
    ) -> Result<Option<String>> {
        // GitHub Actions generally resolves branches before tags, so try
        // the repo's branches first.
        let url = format!(
            "{api_base}/repos/{owner}/{repo}/git/ref/heads/{git_ref}",
            api_base = self.api_base
        );

        let resp = self.http.get(url).send()?;
        match resp.status() {
            StatusCode::OK => Ok(Some(resp.json::<GitRef>()?.object.sha)),
            StatusCode::NOT_FOUND => {
                let url = format!(
                    "{api_base}/repos/{owner}/{repo}/git/ref/tags/{git_ref}",
                    api_base = self.api_base
                );

                let resp = self.http.get(url).send()?;
                match resp.status() {
                    StatusCode::OK => Ok(Some(resp.json::<GitRef>()?.object.sha)),
                    StatusCode::NOT_FOUND => Ok(None),
                    s => Err(anyhow!(
                        "{owner}/{repo}: error from GitHub API while accessing ref {git_ref}: {s}"
                    )),
                }
            }
            s => Err(anyhow!(
                "{owner}/{repo}: error from GitHub API while accessing ref {git_ref}: {s}"
            )),
        }
    }

    pub(crate) fn longest_tag_for_commit(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
    ) -> Result<Option<Tag>> {
        // Annoying: GitHub doesn't provide a rev-parse or similar API to
        // perform the commit -> tag lookup, so we download every tag and
        // do it for them.
        // This could be optimized in various ways, not least of which
        // is not pulling every tag eagerly before scanning them.
        let tags = self.list_tags(owner, repo)?;

        // Heuristic: there can be multiple tags for a commit, so we pick
        // the longest one. This isn't super sound, but it gets us from
        // `sha -> v1.2.3` instead of `sha -> v1`.
        Ok(tags
            .into_iter()
            .filter(|t| t.commit.sha == commit)
            .max_by_key(|t| t.name.len()))
    }

    pub(crate) fn compare_commits(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<ComparisonStatus>> {
        self.caches
            .ref_comparison_cache
            .try_get_with((base.into(), head.into()), || {
                let url = format!(
                    "{api_base}/repos/{owner}/{repo}/compare/{base}...{head}",
                    api_base = self.api_base
                );

                let resp = self.http.get(url).send()?;

                match resp.status() {
                    StatusCode::OK => {
                        Ok::<_, reqwest::Error>(Some(resp.json::<Comparison>()?.status))
                    }
                    StatusCode::NOT_FOUND => Ok(None),
                    _ => Err(resp.error_for_status().unwrap_err()),
                }
            })
            .map_err(Into::into)
    }

    pub(crate) fn gha_advisories(
        &self,
        owner: &str,
        repo: &str,
        version: &str,
    ) -> Result<Vec<Advisory>> {
        // TODO: Paginate this as well.
        let url = format!("{api_base}/advisories", api_base = self.api_base);

        self.http
            .get(url)
            .query(&[
                ("ecosystem", "actions"),
                ("affects", &format!("{owner}/{repo}@{version}")),
            ])
            .send()?
            .error_for_status()?
            .json()
            .map_err(Into::into)
    }
}

/// A single branch, as returned by GitHub's branches endpoints.
///
/// This model is intentionally incomplete.
///
/// See <https://docs.github.com/en/rest/branches/branches?apiVersion=2022-11-28>.
#[derive(Deserialize, Clone)]
pub(crate) struct Branch {
    pub(crate) name: String,
}

/// A single tag, as returned by GitHub's tags endpoints.
///
/// This model is intentionally incomplete.
#[derive(Deserialize, Clone)]
pub(crate) struct Tag {
    pub(crate) name: String,
    pub(crate) commit: TagCommit,
}

/// Represents the SHA ref bound to a tag.
#[derive(Deserialize, Clone)]
pub(crate) struct TagCommit {
    pub(crate) sha: String,
}

#[derive(Deserialize)]
pub(crate) struct GitRef {
    pub(crate) object: GitObj,
}

#[derive(Deserialize)]
pub(crate) struct GitObj {
    pub(crate) sha: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ComparisonStatus {
    Ahead,
    Behind,
    Diverged,
    Identical,
}

/// The result of comparing two commits via GitHub's API.
///
/// See <https://docs.github.com/en/rest/commits/commits?apiVersion=2022-11-28>
#[derive(Deserialize)]
pub(crate) struct Comparison {
    pub(crate) status: ComparisonStatus,
}

/// Represents a GHSA advisory.
#[derive(Deserialize)]
pub(crate) struct Advisory {
    pub(crate) ghsa_id: String,
    pub(crate) severity: String,
}
