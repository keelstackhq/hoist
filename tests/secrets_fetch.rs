//! One file of a secrets repo (spec §15.7.1 step 3): `fetch` reads through a `Reader`, which never
//! follows a redirect off the API host, whatever client it was built from.
mod common;

use common::FakeApi;
use hoist::http::{Http, Service};
use hoist::secrets::fetch::{Reader, fetch};
use hoist::secrets::manifest::Source;

#[test]
fn a_reader_never_follows_a_redirect_off_the_api_host() {
    let other = FakeApi::start(&[]);
    let to = format!("{}/repos/acme/s/contents/a.yaml", other.url());
    let route = (
        "GET /repos/acme/s/contents/a.yaml",
        302,
        "{}",
        &[("Location", to.as_str())][..],
    );
    let api = FakeApi::start(&[route]);
    // A client that follows redirects anywhere: the reader still refuses this one.
    let http = Http::with_base(Service::Github, "t".into(), &api.url()).max_sleep_ms(0);
    let source = Source {
        repo: "acme/s".into(),
        git_ref: "main".into(),
    };
    let err = fetch(&Reader::new(http), &source, "a.yaml").unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
    assert!(other.requests().is_empty(), "the other host was contacted");
}
