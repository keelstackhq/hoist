//! Tags and GitHub Releases for `release publish` (spec §9.3): a lightweight tag on the release
//! commit, then a draft release that gets every asset and `SHA256SUMS` before it is published
//! with `PATCH {"draft": false}` (which works with immutable releases).

use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use crate::repo::git::GithubRepo;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The checksum asset every release with binaries gets.
pub const SUMS: &str = "SHA256SUMS";
/// Pages of 100 releases read while looking for a draft before giving up.
const MAX_RELEASE_PAGES: u32 = 100;
/// Nested annotated tags followed before giving up.
const MAX_PEEL: usize = 5;

/// Whether a step found its work already done or wrote it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    Already,
    Wrote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagState {
    Absent,
    /// The commit the tag points at (annotated tags peeled).
    At(String),
}

/// An asset to attach: a built file or the generated `SHA256SUMS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAsset {
    pub name: String,
    /// Lowercase hex sha256 of the content.
    pub sha256: String,
    pub size: u64,
    source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl LocalAsset {
    fn bytes(&self) -> Result<Vec<u8>, HoistError> {
        match &self.source {
            Source::File(path) => read(path),
            Source::Bytes(b) => Ok(b.clone()),
        }
    }
}

/// An asset as GitHub lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAsset {
    pub id: u64,
    pub name: String,
    pub state: Option<String>,
    /// `sha256:<hex>` when GitHub computed it.
    pub digest: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRelease {
    pub id: u64,
    pub draft: bool,
    pub assets: Vec<LiveAsset>,
}

/// An asset a draft still needs: upload `name`, deleting the stale asset `delete` first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub name: String,
    pub delete: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseState {
    /// Published with every expected asset, each matching the local file.
    Complete,
    /// Published, lacking or mismatching assets ("lacks …", "mismatches …"); immutable.
    Published(String),
    /// A draft (its id) and the assets it still needs.
    Draft(u64, Vec<Fix>),
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

fn local(name: String, bytes: &[u8], source: Source) -> LocalAsset {
    LocalAsset {
        name,
        sha256: hex(&Sha256::digest(bytes)),
        size: bytes.len() as u64,
        source,
    }
}

/// The assets of a release: each file in order, then `SHA256SUMS` (one `<hex>  <file>` line
/// per file) when there are files. Reads every file once (exit 3 when one is unreadable).
pub fn local_assets(files: &[PathBuf]) -> Result<Vec<LocalAsset>, HoistError> {
    let mut out = Vec::new();
    let mut sums = String::new();
    for f in files {
        let a = local(file_name(f), &read(f)?, Source::File(f.clone()));
        sums.push_str(&format!("{}  {}\n", a.sha256, a.name));
        out.push(a);
    }
    if !out.is_empty() {
        let bytes = sums.into_bytes();
        out.push(local(
            SUMS.to_string(),
            &bytes,
            Source::Bytes(bytes.clone()),
        ));
    }
    Ok(out)
}

/// R20: a live asset counts only when uploaded and its digest (else its size) matches.
fn matches(a: &LiveAsset, l: &LocalAsset) -> bool {
    a.state.as_deref() == Some("uploaded")
        && match &a.digest {
            Some(d) => d.eq_ignore_ascii_case(&format!("sha256:{}", l.sha256)),
            None => a.size == Some(l.size),
        }
}

/// The local assets `live` lacks (`None`) or holds in a stale form (`Some(id)`).
fn gaps<'a>(live: &[LiveAsset], local: &'a [LocalAsset]) -> Vec<(&'a LocalAsset, Option<u64>)> {
    local
        .iter()
        .filter_map(|l| match live.iter().find(|a| a.name == l.name) {
            None => Some((l, None)),
            Some(a) if matches(a, l) => None,
            Some(a) => Some((l, Some(a.id))),
        })
        .collect()
}

/// "lacks a, b and mismatches c".
fn describe(gaps: &[(&LocalAsset, Option<u64>)]) -> String {
    let names = |stale: bool| {
        gaps.iter()
            .filter(|(_, id)| id.is_some() == stale)
            .map(|(l, _)| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    for (stale, verb) in [(false, "lacks"), (true, "mismatches")] {
        let n = names(stale);
        if !n.is_empty() {
            parts.push(format!("{verb} {n}"));
        }
    }
    parts.join(" and ")
}

/// The commit an annotated tag object points at (`GET /git/tags/<sha>`, nested tags followed).
fn peel(gh: &Http, repo: &GithubRepo, tag: &str, mut sha: String) -> Result<String, HoistError> {
    let what = format!("tag `{tag}`");
    for _ in 0..MAX_PEEL {
        let path = format!("{}/git/tags/{}", repo_path(repo), seg(&sha));
        let v = match gh.get_json(&path)? {
            Fetched::Ok(v) => v,
            Fetched::NotFound => return Err(unreadable(&what, repo, "tag object not found (404)")),
            Fetched::Forbidden(m) => return Err(unreadable(&what, repo, &m)),
        };
        let object = &v["object"];
        sha = object["sha"]
            .as_str()
            .ok_or_else(|| invalid(&what))?
            .to_string();
        if object["type"] != "tag" {
            return Ok(sha);
        }
    }
    Err(invalid(&what))
}

pub fn tag_state(gh: &Http, repo: &GithubRepo, tag: &str) -> Result<TagState, HoistError> {
    let what = format!("tag `{tag}`");
    let path = format!("{}/git/ref/tags/{}", repo_path(repo), tag_path(tag));
    match gh.get_json(&path)? {
        // An array lists refs that only start with the tag: the tag itself is absent.
        Fetched::Ok(Value::Array(_)) | Fetched::NotFound => Ok(TagState::Absent),
        Fetched::Ok(v) => {
            let object = &v["object"];
            let sha = object["sha"]
                .as_str()
                .ok_or_else(|| invalid(&what))?
                .to_string();
            if object["type"] == "tag" {
                Ok(TagState::At(peel(gh, repo, tag, sha)?))
            } else {
                Ok(TagState::At(sha))
            }
        }
        Fetched::Forbidden(m) => Err(unreadable(&what, repo, &m)),
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

fn parse_asset(v: &Value) -> Option<LiveAsset> {
    Some(LiveAsset {
        id: v.get("id")?.as_u64()?,
        name: v.get("name")?.as_str()?.to_string(),
        state: v.get("state").and_then(Value::as_str).map(str::to_string),
        digest: v.get("digest").and_then(Value::as_str).map(str::to_string),
        size: v.get("size").and_then(Value::as_u64),
    })
}

fn parse_assets(v: Option<&Value>) -> Vec<LiveAsset> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_asset).collect())
        .unwrap_or_default()
}

fn parse_release(v: &Value) -> Option<LiveRelease> {
    Some(LiveRelease {
        id: v.get("id")?.as_u64()?,
        draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
        assets: parse_assets(v.get("assets")),
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

/// The first draft release for `tag`, reading the release list page by page and stopping
/// at the first match or the first short page.
fn draft_release(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
) -> Result<Option<LiveRelease>, HoistError> {
    for page in 1..=MAX_RELEASE_PAGES {
        let path = format!("{}/releases?per_page=100&page={page}", repo_path(repo));
        let list = match gh.get_json(&path)? {
            Fetched::Ok(Value::Array(list)) => list,
            Fetched::Ok(_) => return Err(invalid("the release list")),
            Fetched::NotFound => return Err(unreadable("the releases", repo, "not found (404)")),
            Fetched::Forbidden(m) => return Err(unreadable("the releases", repo, &m)),
        };
        let found = list
            .iter()
            .filter(|r| r.get("tag_name").and_then(Value::as_str) == Some(tag))
            .filter_map(parse_release)
            .find(|r| r.draft);
        if found.is_some() || list.len() < 100 {
            return Ok(found);
        }
    }
    Err(HoistError::Network(format!(
        "GitHub: {repo} has more than {} releases; no draft for `{tag}` found in them",
        MAX_RELEASE_PAGES * 100
    )))
}

pub fn release_state(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
    local: &[LocalAsset],
) -> Result<ReleaseState, HoistError> {
    if let Some(r) = published_release(gh, repo, tag)?.filter(|r| !r.draft) {
        let g = gaps(&r.assets, local);
        return Ok(if g.is_empty() {
            ReleaseState::Complete
        } else {
            ReleaseState::Published(describe(&g))
        });
    }
    Ok(match draft_release(gh, repo, tag)? {
        Some(r) => {
            let fixes = gaps(&r.assets, local)
                .into_iter()
                .map(|(l, delete)| Fix {
                    name: l.name.clone(),
                    delete,
                })
                .collect();
            ReleaseState::Draft(r.id, fixes)
        }
        None => ReleaseState::Absent,
    })
}

fn delete_asset(gh: &Http, repo: &GithubRepo, asset: u64) -> Result<(), HoistError> {
    let path = format!("{}/releases/assets/{asset}", repo_path(repo));
    gh.send_json(Method::Delete, &path, None)?;
    Ok(())
}

fn list_assets(gh: &Http, repo: &GithubRepo, id: u64) -> Result<Vec<LiveAsset>, HoistError> {
    let what = "the release assets";
    match gh.get_all(&format!("{}/releases/{id}/assets", repo_path(repo)), None)? {
        Fetched::Ok(v) => Ok(parse_assets(Some(&v))),
        Fetched::NotFound => Err(unreadable(what, repo, "not found (404)")),
        Fetched::Forbidden(m) => Err(unreadable(what, repo, &m)),
    }
}

fn post_asset(
    up: &Http,
    repo: &GithubRepo,
    id: u64,
    name: &str,
    bytes: &[u8],
) -> Result<(), HttpError> {
    let path = format!(
        "{}/releases/{id}/assets?name={}",
        repo_path(repo),
        seg(name)
    );
    up.send_bytes(Method::Post, &path, "application/octet-stream", bytes)
        .map(|_| ())
}

/// Upload `l`. A 422 (`already_exists`) re-lists the assets: a matching one counts, a stale
/// one is deleted and uploaded once more (R20).
fn upload(
    gh: &Http,
    up: &Http,
    repo: &GithubRepo,
    id: u64,
    l: &LocalAsset,
) -> Result<(), HoistError> {
    let bytes = l.bytes()?;
    match post_asset(up, repo, id, &l.name, &bytes) {
        Ok(()) => Ok(()),
        Err(e @ HttpError::Rejected { status: 422, .. }) => {
            let live = list_assets(gh, repo, id)?;
            match live.iter().find(|a| a.name == l.name) {
                Some(a) if matches(a, l) => Ok(()),
                Some(a) => {
                    delete_asset(gh, repo, a.id)?;
                    Ok(post_asset(up, repo, id, &l.name, &bytes)?)
                }
                None => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Make the release of `tag` exist, published, with every asset: a complete published release
/// is done, an incomplete one fails (releases are immutable); otherwise reuse the draft or
/// create one, replace stale and upload missing assets, then publish it. `up` is the uploads
/// client.
pub fn ensure_release(
    gh: &Http,
    up: &Http,
    repo: &GithubRepo,
    tag: &str,
    body: &str,
    local: &[LocalAsset],
) -> Result<Done, HoistError> {
    let (id, fixes) = match release_state(gh, repo, tag, local)? {
        ReleaseState::Complete => return Ok(Done::Already),
        ReleaseState::Published(problem) => {
            return Err(HoistError::Usage(format!(
                "published release {tag} {problem}; releases are immutable — release a new version"
            )));
        }
        ReleaseState::Draft(id, fixes) => (id, fixes),
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
            let all = local
                .iter()
                .map(|l| Fix {
                    name: l.name.clone(),
                    delete: None,
                })
                .collect();
            (id, all)
        }
    };
    for l in local {
        let Some(fix) = fixes.iter().find(|f| f.name == l.name) else {
            continue;
        };
        if let Some(stale) = fix.delete {
            delete_asset(gh, repo, stale)?;
        }
        upload(gh, up, repo, id, l)?;
    }
    gh.send_json(
        Method::Patch,
        &format!("{}/releases/{id}", repo_path(repo)),
        Some(&json!({"draft": false})),
    )?;
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

/// Verification (§9.5): the release is published and every asset matches the local file.
pub fn verify_release(
    gh: &Http,
    repo: &GithubRepo,
    tag: &str,
    local: &[LocalAsset],
) -> Result<String, String> {
    match published_release(gh, repo, tag) {
        Ok(Some(r)) if !r.draft => {
            let g = gaps(&r.assets, local);
            if !g.is_empty() {
                return Err(format!("release `{tag}` {}", describe(&g)));
            }
            let names: Vec<&str> = local.iter().map(|l| l.name.as_str()).collect();
            Ok(if names.is_empty() {
                format!("release `{tag}` is published")
            } else {
                format!("release `{tag}` is published with {}", names.join(", "))
            })
        }
        Ok(_) => Err(format!("release `{tag}` is not published")),
        Err(e) => Err(format!("cannot read release `{tag}`: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(name: &str, state: &str, digest: Option<&str>, size: Option<u64>) -> LiveAsset {
        LiveAsset {
            id: 1,
            name: name.to_string(),
            state: Some(state.to_string()),
            digest: digest.map(str::to_string),
            size,
        }
    }

    #[test]
    fn hex_and_tag_paths() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(tag_path("@acme/lib@1.0.0"), "%40acme/lib%401.0.0");
        assert_eq!(tag_path("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn local_assets_add_sums_only_with_files() {
        assert!(local_assets(&[]).unwrap().is_empty());
        let dir = std::env::temp_dir().join(format!("hoist-gh-rel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("tool-x");
        std::fs::write(&f, b"abc").unwrap();
        let a = local_assets(std::slice::from_ref(&f)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(a[0].sha256, abc);
        assert_eq!(a[1].name, SUMS);
        assert_eq!(
            a[1].bytes().unwrap(),
            format!("{abc}  tool-x\n").into_bytes()
        );
    }

    #[test]
    fn asset_match_needs_uploaded_and_digest_or_size() {
        let l = local("x".into(), b"abc", Source::Bytes(b"abc".to_vec()));
        let good = format!("sha256:{}", l.sha256);
        assert!(matches(&live("x", "uploaded", Some(&good), None), &l));
        assert!(!matches(&live("x", "starter", Some(&good), Some(3)), &l));
        assert!(!matches(
            &live("x", "uploaded", Some("sha256:00"), Some(3)),
            &l
        ));
        assert!(matches(&live("x", "uploaded", None, Some(3)), &l));
        assert!(!matches(&live("x", "uploaded", None, Some(4)), &l));
        assert!(!matches(&live("x", "uploaded", None, None), &l));
    }
}
