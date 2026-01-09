use std::fs;
use std::io;
use std::path::Path;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct GithubSource {
    pub owner: String,
    pub repo: String,
    pub reference: Option<String>,
    pub reference_kind: ReferenceKind,
    pub archive_url: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ReferenceKind {
    Heads,
    Tags,
}

pub fn parse_github_source(input: &str) -> Result<GithubSource> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput("source cannot be empty".to_string()));
    }

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if is_github_url(trimmed) {
            parse_github_url(trimmed)
        } else {
            parse_direct_zip_url(trimmed)
        }
    } else {
        let mut parts = trimmed.splitn(2, '/');
        let owner = parts.next().unwrap_or("").trim();
        let repo = parts.next().unwrap_or("").trim();
        if owner.is_empty() || repo.is_empty() {
            return Err(Error::InvalidInput(
                "source must be in the form owner/repo".to_string(),
            ));
        }
        Ok(GithubSource {
            owner: owner.to_string(),
            repo: repo.trim_end_matches(".git").to_string(),
            reference: None,
            reference_kind: ReferenceKind::Heads,
            archive_url: None,
        })
    }
}

fn is_github_url(url: &str) -> bool {
    url.starts_with("https://github.com/") || url.starts_with("http://github.com/")
}

fn parse_github_url(url: &str) -> Result<GithubSource> {
    let without_query = url.split(['?', '#']).next().unwrap_or("");
    let without_scheme = without_query
        .strip_prefix("https://github.com/")
        .or_else(|| without_query.strip_prefix("http://github.com/"))
        .ok_or_else(|| Error::InvalidInput("only github.com URLs are supported".to_string()))?;

    let segments: Vec<&str> = without_scheme
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return Err(Error::InvalidInput(
            "github URL must include owner and repo".to_string(),
        ));
    }

    let owner = segments[0].to_string();
    let repo = segments[1].trim_end_matches(".git").to_string();
    let mut reference = None;
    let mut reference_kind = ReferenceKind::Heads;
    let mut archive_url = None;

    if segments.len() >= 4 && segments[2] == "tree" {
        reference = Some(segments[3].to_string());
    }

    if without_query.ends_with(".zip") && without_query.contains("/archive/") {
        archive_url = Some(without_query.to_string());
        if let Some(pos) = without_query.find("/archive/refs/tags/") {
            reference_kind = ReferenceKind::Tags;
            reference = Some(
                without_query[pos + "/archive/refs/tags/".len()..]
                    .trim_end_matches(".zip")
                    .to_string(),
            );
        } else if let Some(pos) = without_query.find("/archive/refs/heads/") {
            reference = Some(
                without_query[pos + "/archive/refs/heads/".len()..]
                    .trim_end_matches(".zip")
                    .to_string(),
            );
        }
    }

    Ok(GithubSource {
        owner,
        repo,
        reference,
        reference_kind,
        archive_url,
    })
}

fn parse_direct_zip_url(url: &str) -> Result<GithubSource> {
    let without_query = url.split(['?', '#']).next().unwrap_or("");
    if !without_query.ends_with(".zip") {
        return Err(Error::InvalidInput(
            "only github.com URLs or direct .zip URLs are supported".to_string(),
        ));
    }

    let without_scheme = without_query
        .strip_prefix("https://")
        .or_else(|| without_query.strip_prefix("http://"))
        .ok_or_else(|| Error::InvalidInput("invalid URL scheme".to_string()))?;

    let host = without_scheme
        .split('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("archive");
    let last_segment = without_scheme.rsplit('/').next().unwrap_or("archive.zip");
    let repo = last_segment.trim_end_matches(".zip");

    Ok(GithubSource {
        owner: host.to_string(),
        repo: repo.to_string(),
        reference: None,
        reference_kind: ReferenceKind::Heads,
        archive_url: Some(without_query.to_string()),
    })
}

pub fn download_archive(source: &GithubSource, dest: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("mews")
        .build()
        .map_err(|err| Error::InvalidInput(format!("failed to build http client: {}", err)))?;

    if let Some(url) = &source.archive_url {
        return download_url(&client, url, dest);
    }

    let mut refs = Vec::new();
    if let Some(reference) = &source.reference {
        refs.push((reference.clone(), source.reference_kind));
        refs.push((reference.clone(), ReferenceKind::Tags));
        refs.push((reference.clone(), ReferenceKind::Heads));
    } else {
        refs.push(("main".to_string(), ReferenceKind::Heads));
        refs.push(("master".to_string(), ReferenceKind::Heads));
    }

    let mut last_error = None;

    for (reference, kind) in refs {
        let url = match kind {
            ReferenceKind::Heads => format!(
                "https://github.com/{}/{}/archive/refs/heads/{}.zip",
                source.owner, source.repo, reference
            ),
            ReferenceKind::Tags => format!(
                "https://github.com/{}/{}/archive/refs/tags/{}.zip",
                source.owner, source.repo, reference
            ),
        };

        match download_url(&client, &url, dest) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if let Error::InvalidInput(message) = &err {
                    if message.contains("status 404") && source.reference.is_none() {
                        continue;
                    }
                }
                last_error = Some(err.to_string());
            }
        }
    }

    Err(Error::InvalidInput(format!(
        "failed to download GitHub archive{}",
        last_error
            .as_ref()
            .map(|err| format!(": {}", err))
            .unwrap_or_default()
    )))
}

fn download_url(client: &reqwest::blocking::Client, url: &str, dest: &Path) -> Result<()> {
    let response = client
        .get(url)
        .send()
        .map_err(|err| Error::InvalidInput(format!("download failed for {}: {}", url, err)))?;

    if !response.status().is_success() {
        return Err(Error::InvalidInput(format!(
            "download failed with status {}",
            response.status()
        )));
    }

    let mut file = fs::File::create(dest).map_err(|err| Error::io(dest, err))?;
    let mut stream = io::BufReader::new(response);
    io::copy(&mut stream, &mut file).map_err(|err| Error::io(dest, err))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_github_source;

    #[test]
    fn parse_owner_repo() {
        let parsed = parse_github_source("owner/repo").unwrap();
        assert_eq!(parsed.owner, "owner");
        assert_eq!(parsed.repo, "repo");
    }

    #[test]
    fn parse_github_url() {
        let parsed = parse_github_source("https://github.com/owner/repo").unwrap();
        assert_eq!(parsed.owner, "owner");
        assert_eq!(parsed.repo, "repo");
    }
}
