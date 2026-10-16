mod common;

use hoist::repo::git::{parse_github_remote, read_repo};

#[test]
fn parses_https_ssh_and_scp_forms() {
    for url in [
        "https://github.com/acme/site",
        "https://github.com/acme/site.git",
        "git@github.com:acme/site",
        "git@github.com:acme/site.git",
        "ssh://git@github.com/acme/site",
        "ssh://git@github.com/acme/site.git",
    ] {
        let repo = parse_github_remote(url).unwrap_or_else(|| panic!("{url} should parse"));
        assert_eq!(repo.to_string(), "acme/site", "{url}");
        assert_eq!(repo.owner, "acme");
        assert_eq!(repo.name, "site");
    }
}

#[test]
fn rejects_non_github() {
    for url in [
        "https://gitlab.com/a/b",
        "file:///x",
        "https://github.com/onlyowner",
        "https://github.com/a/b/c",
        "",
    ] {
        assert!(parse_github_remote(url).is_none(), "{url}");
    }
}

#[test]
fn no_origin_is_config_error() {
    let dir = common::git_repo_no_origin();
    let err = read_repo(dir.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains("set an `origin` remote pointing to github.com"),
        "{err}"
    );
    assert_eq!(err.exit_code(), 3);
}

#[test]
fn non_github_origin_is_config_error() {
    let dir = common::git_repo_no_origin();
    common::git(
        dir.path(),
        &["remote", "add", "origin", "https://gitlab.com/a/b"],
    );
    let err = read_repo(dir.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains("set an `origin` remote pointing to github.com"),
        "{err}"
    );
}

#[test]
fn default_branch_falls_back_to_main() {
    let dir = common::git_repo();
    let (repo, branch) = read_repo(dir.path()).unwrap();
    assert_eq!(repo.to_string(), "acme/x");
    assert_eq!(branch, "main");
}

#[test]
fn default_branch_comes_from_origin_head() {
    let dir = common::git_repo();
    common::git(
        dir.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let (_, branch) = read_repo(dir.path()).unwrap();
    assert_eq!(branch, "trunk");
}
