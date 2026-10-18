mod common;

use common::FakeApi;
use hoist::auth::Secret;
use hoist::config::PublishCfg;
use hoist::github::Fetch;
use hoist::http::{Http, Service};
use hoist::release::registry::{package_exists, version_exists};
use hoist::release::{Package, PkgKind};
use hoist::repo::Sourced;
use std::path::PathBuf;

const CRATES_UA: &str = concat!(
    "hoist/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/keelstackhq/hoist)"
);

fn pkg(name: &str, kind: PkgKind) -> Package {
    Package {
        name: name.to_string(),
        kind,
        dir: PathBuf::from("packages/x"),
        version: Sourced {
            value: "1.0.0".to_string(),
            source: "test".to_string(),
        },
        deps: Vec::new(),
        publish: PublishCfg::default(),
        launcher: None,
    }
}

fn client(service: Service, api: &FakeApi) -> Http {
    Http::with_base(service, Secret::from(""), &api.url())
}

#[test]
fn registry_404_means_absent() {
    let api = FakeApi::start(&[]);
    let npm = client(Service::Npm, &api);
    let crates = client(Service::Crates, &api);
    let lib = pkg("@acme/lib", PkgKind::Npm);
    let tool = pkg("tool", PkgKind::Crate);
    assert_eq!(package_exists(&npm, &lib).unwrap(), Fetch::Got(false));
    assert_eq!(
        version_exists(&npm, &lib, "1.0.0").unwrap(),
        Fetch::Got(false)
    );
    assert_eq!(package_exists(&crates, &tool).unwrap(), Fetch::Got(false));
    assert_eq!(
        version_exists(&crates, &tool, "0.3.0").unwrap(),
        Fetch::Got(false)
    );
    let paths: Vec<String> = api.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        paths,
        [
            "/@acme%2Flib",
            "/@acme%2Flib/1.0.0",
            "/api/v1/crates/tool",
            "/api/v1/crates/tool/0.3.0"
        ]
    );
    api.assert_only_get();
}

#[test]
fn registry_200_means_present() {
    let api = FakeApi::start(&[
        ("GET /@acme%2Flib", 200, r#"{"name":"@acme/lib"}"#, &[]),
        ("GET /@acme%2Flib/1.0.0", 200, r#"{"version":"1.0.0"}"#, &[]),
        ("GET /api/v1/crates/tool", 200, r#"{"crate":{}}"#, &[]),
        (
            "GET /api/v1/crates/tool/0.3.0",
            200,
            r#"{"version":{}}"#,
            &[],
        ),
    ]);
    let npm = client(Service::Npm, &api);
    let crates = client(Service::Crates, &api);
    let lib = pkg("@acme/lib", PkgKind::Npm);
    let tool = pkg("tool", PkgKind::Crate);
    assert_eq!(package_exists(&npm, &lib).unwrap(), Fetch::Got(true));
    assert_eq!(
        version_exists(&npm, &lib, "1.0.0").unwrap(),
        Fetch::Got(true)
    );
    assert_eq!(package_exists(&crates, &tool).unwrap(), Fetch::Got(true));
    assert_eq!(
        version_exists(&crates, &tool, "0.3.0").unwrap(),
        Fetch::Got(true)
    );
    for r in api.requests() {
        // Registry reads are anonymous.
        assert_eq!(r.auth, None, "{}", r.path);
        if r.path.starts_with("/api/v1/crates/") {
            assert_eq!(r.user_agent.as_deref(), Some(CRATES_UA));
        }
    }
}

#[test]
fn registry_5xx_is_unknown() {
    let api = FakeApi::start(&[("GET /api/v1/crates/tool", 503, "{}", &[])]);
    let crates = client(Service::Crates, &api);
    let tool = pkg("tool", PkgKind::Crate);
    match package_exists(&crates, &tool).unwrap() {
        Fetch::Unknown(why) => assert!(why.contains("503"), "{why}"),
        other => panic!("expected unknown, got {other:?}"),
    }
}
