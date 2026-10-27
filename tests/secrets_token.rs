//! The token `hoist secrets exec` reads secrets repos with (spec §15.7.1 step 2):
//! `HOIST_SECRETS_TOKEN`, else an installation token minted with the secrets-read App.
#![cfg(unix)]
mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::FakeApi;
use hoist::http::{Http, Service};
use hoist::secrets::token::secrets_token;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const NOW: u64 = 1_793_059_200;
const REPOS: [&str; 4] = [
    "acme/secrets-site",
    "acme/secrets-shared",
    "Acme/secrets-site",
    "acme/SECRETS-site",
];
const INSTALL: &str = "GET /repos/acme/secrets-shared/installation";
const MINT: &str = "POST /app/installations/42/access_tokens";
const GRANT: &str = r#"{"token":"ghs_minted","permissions":{"contents":"read","metadata":"read"},
  "repository_selection":"selected","repositories":[{"name":"secrets-site"},{"name":"secrets-shared"}]}"#;

/// A throwaway 2048-bit key made at runtime (never committed): PEM, PKCS#8 or PKCS#1 (OpenSSL 3
/// needs `-traditional`; LibreSSL writes PKCS#1 without it), and the public key as PKCS#1 DER.
fn key(dir: &Path, pkcs8: bool) -> (String, Vec<u8>) {
    let (p8, p1) = (dir.join("k8.pem"), dir.join("k1.pem"));
    let (p8, p1) = (p8.to_str().unwrap(), p1.to_str().unwrap());
    let try_run = |args: &str, path: &str| {
        let args = args.split(' ').chain([path]);
        let out = Command::new("openssl").args(args).output().unwrap();
        out.status.success().then_some(out.stdout)
    };
    try_run(
        "genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out",
        p8,
    )
    .expect("genpkey");
    let traditional = try_run(&format!("rsa -in {p8} -traditional -out"), p1);
    traditional
        .or_else(|| try_run(&format!("rsa -in {p8} -out"), p1))
        .expect("rsa");
    let public = try_run("rsa -RSAPublicKey_out -outform DER -in", p8).unwrap();
    let pem = std::fs::read_to_string(if pkcs8 { p8 } else { p1 }).unwrap();
    assert!(pkcs8 || pem.contains("BEGIN RSA PRIVATE KEY"), "{pkcs8}");
    (pem, public)
}

fn mint(env: &BTreeMap<&str, String>, api: &FakeApi) -> Result<String, String> {
    let token = secrets_token(&|n| env.get(n).cloned(), &REPOS, NOW, &|t| {
        Http::with_base(Service::Github, t, &api.url()).max_sleep_ms(0)
    });
    let token = token.map_err(|e| e.to_string())?;
    // Show which token came back by sending it once.
    let probe = FakeApi::start(&[]);
    let _ = Http::with_base(Service::Github, token, &probe.url()).get_json("/probe");
    Ok(probe.requests()[0].auth.clone().unwrap_or_default())
}

fn app_env(pem: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("SECRETS_READ_APP_ID", "12345".into()),
        ("SECRETS_READ_APP_KEY", pem.into()),
    ])
}

#[test]
fn a_given_token_is_used_as_is() {
    let api = FakeApi::start(&[]);
    let env = BTreeMap::from([("HOIST_SECRETS_TOKEN", " ghs_given\n".to_string())]);
    assert_eq!(mint(&env, &api).unwrap(), "Bearer ghs_given");
    assert!(api.requests().is_empty());
}

#[test]
fn an_installation_token_is_minted_for_the_sources_only() {
    // A key pasted into a CI variable often has literal `\n` for its line breaks.
    for (pkcs8, pasted) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir().unwrap();
        let (pem, public) = key(dir.path(), pkcs8);
        let pem = pem.replace('\n', if pasted { "\\n" } else { "\n" });
        let api = FakeApi::start(&[(INSTALL, 200, r#"{"id":42}"#, &[]), (MINT, 201, GRANT, &[])]);
        assert_eq!(mint(&app_env(&pem), &api).unwrap(), "Bearer ghs_minted");
        let requests = api.requests();
        let auth = |i: usize| requests[i].auth.clone().unwrap_or_default();
        assert_eq!(auth(0), auth(1));
        let jwt = auth(0).strip_prefix("Bearer ").unwrap().to_string();
        let parts: Vec<&str> = jwt.split('.').collect();
        let part = |i: usize| -> Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[i]).unwrap()).unwrap()
        };
        assert_eq!(part(0), json!({ "alg": "RS256", "typ": "JWT" }));
        let claims = json!({ "iat": NOW - 60, "exp": NOW + 540, "iss": "12345" });
        assert_eq!(part(1), claims);
        let signed = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        let rs256 = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public);
        rs256.verify(signed.as_bytes(), &signature).expect("RS256");
        assert_eq!(requests[1].method, "POST");
        let body: Value = serde_json::from_str(requests[1].body.as_deref().unwrap()).unwrap();
        let want = json!({ "repositories": ["secrets-shared", "secrets-site"],
                           "permissions": { "contents": "read" } });
        assert_eq!(body, want);
    }
}

#[test]
fn errors_name_the_fix_and_never_quote_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let (pem, _) = key(dir.path(), false);
    let given = |t: &str| BTreeMap::from([("HOIST_SECRETS_TOKEN", t.to_string())]);
    let (unset, bad_key) = (
        "HOIST_SECRETS_TOKEN or SECRETS_READ_APP_ID",
        "SECRETS_READ_APP_KEY",
    );
    let broken = "-----BEGIN RSA PRIVATE KEY-----\nMARK\n-----END RSA PRIVATE KEY-----\n";
    let lookup = |status, body| vec![(INSTALL, status, body, &[][..])];
    let minted = |status, body| {
        vec![
            (INSTALL, 200, r#"{"id":42}"#, &[][..]),
            (MINT, status, body, &[]),
        ]
    };
    let cases = [
        (BTreeMap::new(), vec![], unset),
        (given(""), vec![], unset),
        (given("  "), vec![], unset),
        (app_env(broken), vec![], bad_key),
        (app_env("MARK"), vec![], bad_key),
        (app_env(&pem), lookup(404, "{}"), "installed"),
        (app_env(&pem), lookup(401, "{}"), "SECRETS_READ_APP_ID"),
        (
            app_env(&pem),
            lookup(403, r#"{"message":"Not accessible"}"#),
            "Not accessible",
        ),
        (app_env(&pem), minted(201, "{}"), "no token"),
        (app_env(&pem), minted(422, "{}"), "not installed on all"),
    ];
    // The JWT client follows no redirect off the API host either.
    let other = FakeApi::start(&[]);
    let to = format!("{}/repos/acme/secrets-shared/installation", other.url());
    let api = FakeApi::start(&[(INSTALL, 302, "{}", &[("Location", &to)])]);
    let err = mint(&app_env(&pem), &api).unwrap_err();
    assert!(
        err.contains("refused") && other.requests().is_empty(),
        "{err}"
    );
    for (env, routes, want) in cases {
        let err = mint(&env, &FakeApi::start(&routes)).unwrap_err();
        assert!(err.contains(want), "{want}: {err}");
        assert!(!err.contains("MARK") && !err.contains("BEGIN"), "{err}");
    }
    let api = FakeApi::start(&[]);
    let http = |t| Http::with_base(Service::Github, t, &api.url());
    let fails = |env: BTreeMap<&str, String>, repos: &[&str], want: &str| {
        let err = secrets_token(&|n| env.get(n).cloned(), repos, NOW, &http).err();
        assert!(
            err.is_some_and(|e| e.to_string().contains(want)),
            "{repos:?}"
        );
    };
    fails(app_env(&pem), &["acme/a", "other/b"], "one owner");
    for bad in [&["a"][..], &["acme/a/b"], &["/a"]] {
        fails(app_env(&pem), bad, "owner/name");
    }
    // Owners are case-insensitive on GitHub: one owner, so it goes on to the lookup.
    fails(app_env(&pem), &["Acme/a", "acme/b"], "installed");
    let mixed = secrets_token(&|n| given("x").get(n).cloned(), &["a/b", "c/d"], NOW, &http);
    assert!(mixed.is_ok(), "a given token may span owners");
}

/// A grant wider than asked (more permissions, all repos, other repos, or no permissions listed)
/// is refused, and the token it carried is revoked at once.
#[test]
fn a_wider_grant_is_refused_and_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let (pem, _) = key(dir.path(), false);
    let base: Value = serde_json::from_str(GRANT).unwrap();
    let wider = [
        ("/permissions", json!({ "contents": "write" })),
        (
            "/permissions",
            json!({ "contents": "read", "metadata": "write" }),
        ),
        ("/permissions", json!({ "metadata": "read" })),
        ("/permissions", Value::Null),
        ("/repository_selection", json!("all")),
        (
            "/repositories",
            json!([{ "name": "secrets-site" }, { "name": "other" }]),
        ),
        // A third entry without a name: the grant is not exactly the asked repos.
        (
            "/repositories",
            json!([{ "name": "secrets-shared" }, { "name": "secrets-site" }, { "id": 7 }]),
        ),
    ];
    let grants = wider.map(|(at, v)| {
        let mut grant = base.clone();
        (*grant.pointer_mut(at).unwrap(), grant["token"]) = (v, json!("ghs_x"));
        grant.to_string()
    });
    for grant in &grants {
        let routes = [
            (INSTALL, 200, r#"{"id":42}"#, &[][..]),
            (MINT, 201, grant.as_str(), &[]),
            ("DELETE /installation/token", 204, "", &[]),
        ];
        let api = FakeApi::start(&routes);
        let err = mint(&app_env(&pem), &api).unwrap_err();
        assert!(
            err.contains("refused") && !err.contains("ghs_"),
            "{grant}: {err}"
        );
        let revoke = api.requests().into_iter().find(|r| r.method == "DELETE");
        assert_eq!(
            revoke.and_then(|r| r.auth).as_deref(),
            Some("Bearer ghs_x"),
            "{grant}"
        );
    }
}
