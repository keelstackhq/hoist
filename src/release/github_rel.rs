//! Tags and GitHub Releases for `release publish` (spec §9.3): a lightweight tag on the release
//! commit, then a draft release that gets every asset and `SHA256SUMS` before it is published
//! with `PATCH {"draft": false}` (which works with immutable releases).

use crate::error::HoistError;
use crate::http::{Fetched, Http, Method, seg};
use crate::repo::git::GithubRepo;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The checksum asset every release with binaries gets.
pub const SUMS: &str = "SHA256SUMS";

/// Whether a step found its work already done or wrote it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    Already,
    Wrote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagState {
    Absent,
    /// The sha the tag ref points at.
    At(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRelease {
    pub id: u64,
    pub draft: bool,
    pub assets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseState {
    /// Published with every expected asset.
    Complete,
    /// Published, without the listed assets.
    PublishedMissing(LiveRelease, Vec<String>),
    /// A draft for the tag, without the listed assets.
    Draft(LiveRelease, Vec<String>),
    Absent,
}

fn repo_path(repo: &GithubRepo) -> String {
    format!("/repos/{}/{}", seg(&repo.owner), seg(&repo.name))
}

/// A tag in a URL path: each `/`-separated part encoded, the slashes kept (`@scope/name@1.0.0`).
fn tag_path(tag: &str) -> String {
    tag.split('/').map(seg).collect::<Vec<_>>().join("/")
}

fn unreadable(what: &str, repo: &GithubRepo, message: &str) -> HoistError {
    HoistError::Network(format!(
        "GitHub: cannot read {what} of {repo}: {message}; the token needs Contents: read and write"
    ))
}

fn invalid(what: &str) -> HoistError {
    HoistError::Network(format!("GitHub returned an unexpected body for {what}"))
}

/// Lowercase hex of a byte slice.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read(path: &Path) -> Result<Vec<u8>, HoistError> {
    std::fs::read(path)
        .map_err(|e| HoistError::Usage(format!("cannot read asset {}: {e}", path.display())))
}

/// The asset names a release must carry: each file, plus `SHA256SUMS` when there are files.
pub fn expected_assets(assets: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = assets.iter().map(|a| file_name(a)).collect();
    if !names.is_empty() {
        names.push(SUMS.to_string());
    }
    names
}

/// `SHA256SUMS`: one `<hex>  <file>` line per asset, in the given order.
pub fn sha256sums(assets: &[PathBuf]) -> Result<String, HoistError> {
    let mut out = String::new();
    for a in assets {
        let digest = Sha256::digest(read(a)?);
        out.push_str(&format!("{}  {}\n", hex(&digest), file_name(a)));
    }
    Ok(out)
}

pub fn tag_state(gh: &Http, repo: &GithubRepo, tag: &str) -> Result<TagState, HoistError> {
    let path = format!("{}/git/ref/tags/{}", repo_path(repo), tag_path(tag));
    match gh.get_json(&path)? {
        // An array lists refs that only start with the tag: the tag itself is absent.
        Fetched::Ok(Value::Array(_)) | Fetched::NotFound => Ok(TagState::Absent),
        Fetched::Ok(v) => v["object"]["sha"]
            .as_str()
            .map(|s| TagState::At(s.to_string()))
            .ok_or_else(|| invalid(&format!("tag `{tag}`"))),
        Fetched::Forbidden(m) => Err(unreadable(&format!("tag `{tag}`"), repo, &m)),
    }
}

/// Tag `sha` as `tag` unless that tag already points at it. A tag at another commit fails.
pub fn ensure_tag(gh: &Http, repo: &GithubRepo, tag: &str, sha: &str) -> Result<Done, HoistError> {
    match tag_state(gh, repo, tag)? {
        TagState::At(at) if at == sha => Ok(Done::Already),
        TagState::At(at) => Err(HoistError::Usage(format!(
            "tag exists at another commit: `{tag}` points at {at}, not {sha}; a released \
             version is never re-tagged, so bump the version in a new release PR"
        ))),
        TagState::Absent => {
            let body = json!({"ref": format!("refs/tags/{tag}"), "sha": sha});
            gh.send_json(
                Method::Post,
                &format!("{}/git/refs", repo_path(repo)),
                Some(&body),
            )?;
            Ok(Done::Wrote)
        }
    }
}

fn parse_release(v: &Value) -> Option<LiveRelease> {
    Some(LiveRelease {
        id: v.get("id")?.as_u64()?,
        draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
        assets: v
            .get("assets")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("name")?.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The published release of `tag` (`GET /releases/tags/<tag>` does not return drafts).
pub fn published_release(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
) -> Result<Option<LiveRelease>, HoistError> {
    let what = format!("the release `{tag}`");
    let path = format!("{}/releases/tags/{}", repo_path(repo), tag_path(tag));
    match gh.get_json(&path)? {
        Fetched::Ok(v) => parse_release(&v).map(Some).ok_or_else(|| invalid(&what)),
        Fetched::NotFound => Ok(None),
        Fetched::Forbidden(m) => Err(unreadable(&what, repo, &m)),
    }
}

/// A draft release for `tag`, from the release list.
fn draft_release(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
) -> Result<Option<LiveRelease>, HoistError> {
    let path = format!("{}/releases", repo_path(repo));
    match gh.get_all(&path, None)? {
        Fetched::Ok(Value::Array(list)) => Ok(list
            .iter()
            .filter(|r| r.get("tag_name").and_then(Value::as_str) == Some(tag))
            .filter_map(parse_release)
            .find(|r| r.draft)),
        Fetched::Ok(_) => Err(invalid("the release list")),
        Fetched::NotFound => Err(unreadable("the releases", repo, "not found (404)")),
        Fetched::Forbidden(m) => Err(unreadable("the releases", repo, &m)),
    }
}

fn missing(expected: &[String], have: &[String]) -> Vec<String> {
    expected
        .iter()
        .filter(|e| !have.contains(e))
        .cloned()
        .collect()
}

pub fn release_state(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
    expected: &[String],
) -> Result<ReleaseState, HoistError> {
    if let Some(r) = published_release(gh, repo, tag)?.filter(|r| !r.draft) {
        let lacking = missing(expected, &r.assets);
        return Ok(if lacking.is_empty() {
            ReleaseState::Complete
        } else {
            ReleaseState::PublishedMissing(r, lacking)
        });
    }
    Ok(match draft_release(gh, repo, tag)? {
        Some(r) => {
            let lacking = missing(expected, &r.assets);
            ReleaseState::Draft(r, lacking)
        }
        None => ReleaseState::Absent,
    })
}

fn upload(
    up: &Http,
    repo: &GithubRepo,
    id: u64,
    name: &str,
    bytes: &[u8],
) -> Result<(), HoistError> {
    let path = format!(
        "{}/releases/{id}/assets?name={}",
        repo_path(repo),
        seg(name)
    );
    up.send_bytes(Method::Post, &path, "application/octet-stream", bytes)?;
    Ok(())
}

/// Make the release of `tag` exist, published, with every asset and `SHA256SUMS`: reuse a
/// published release (uploading what it lacks) or a draft, else create a draft; upload the
/// missing assets; then publish the draft. `up` is the uploads client.
pub fn ensure_release(
    gh: &Http,
    up: &Http,
    repo: &GithubRepo,
    tag: &str,
    body: &str,
    assets: &[PathBuf],
) -> Result<Done, HoistError> {
    let expected = expected_assets(assets);
    let (id, lacking, publish) = match release_state(gh, repo, tag, &expected)? {
        ReleaseState::Complete => return Ok(Done::Already),
        ReleaseState::PublishedMissing(r, lacking) => (r.id, lacking, false),
        ReleaseState::Draft(r, lacking) => (r.id, lacking, true),
        ReleaseState::Absent => {
            let create = json!({"tag_name": tag, "name": tag, "body": body, "draft": true});
            let written = gh.send_json(
                Method::Post,
                &format!("{}/releases", repo_path(repo)),
                Some(&create),
            )?;
            let id = written
                .body
                .as_ref()
                .and_then(|b| b.get("id"))
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("the created release"))?;
            (id, expected.clone(), true)
        }
    };
    for a in assets {
        let name = file_name(a);
        if lacking.contains(&name) {
            upload(up, repo, id, &name, &read(a)?)?;
        }
    }
    if lacking.iter().any(|n| n == SUMS) {
        upload(up, repo, id, SUMS, sha256sums(assets)?.as_bytes())?;
    }
    if publish {
        gh.send_json(
            Method::Patch,
            &format!("{}/releases/{id}", repo_path(repo)),
            Some(&json!({"draft": false})),
        )?;
    }
    Ok(Done::Wrote)
}

/// Verification (§9.5): `Err` says what is wrong.
pub fn verify_tag(gh: &Http, repo: &GithubRepo, tag: &str, sha: &str) -> Result<String, String> {
    match tag_state(gh, repo, tag) {
        Ok(TagState::At(at)) if at == sha => Ok(format!("`{tag}` points at {sha}")),
        Ok(TagState::At(at)) => Err(format!("`{tag}` points at {at}, not GITHUB_SHA {sha}")),
        Ok(TagState::Absent) => Err(format!("tag `{tag}` is missing")),
        Err(e) => Err(format!("cannot read tag `{tag}`: {e}")),
    }
}

/// Verification (§9.5): the release is published and carries every expected asset.
pub fn verify_release(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
    expected: &[String],
) -> Result<String, String> {
    match published_release(gh, repo, tag) {
        Ok(Some(r)) if !r.draft => {
            let lacking = missing(expected, &r.assets);
            if lacking.is_empty() {
                Ok(if expected.is_empty() {
                    format!("release `{tag}` is published")
                } else {
                    format!("release `{tag}` is published with {}", expected.join(", "))
                })
            } else {
                Err(format!(
                    "release `{tag}` is missing assets: {}",
                    lacking.join(", ")
                ))
            }
        }
        Ok(_) => Err(format!("release `{tag}` is not published")),
        Err(e) => Err(format!("cannot read release `{tag}`: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_tag_paths() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(tag_path("@acme/lib@1.0.0"), "%40acme/lib%401.0.0");
        assert_eq!(tag_path("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn expected_assets_add_sums_only_with_files() {
        assert!(expected_assets(&[]).is_empty());
        assert_eq!(
            expected_assets(&[PathBuf::from("/a/tool-x")]),
            vec!["tool-x".to_string(), SUMS.to_string()]
        );
    }
}
