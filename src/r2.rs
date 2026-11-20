//! The R2 bucket for backups (`[cloudflare.r2]`, spec §6.2): config, live read, diff, the typed
//! writes and the bucket-scoped token. Delivery of the token's credentials is in
//! `ops/apply/r2_sink.rs`. No value in an item, reason or error is a token or a secret.

use crate::auth::Secret;
use crate::change::token_prefix;
use crate::cloudflare::{CfToken, LiveCloudflare};
use crate::cloudflare_write::{MintScope, mint_scoped, permission_group_ids_named};
use crate::config::Managed;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::github::Fetch;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use crate::report::{Action, Item};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The lifecycle rule hoist owns; every other rule of the bucket is left as read.
pub const RULE_ID: &str = "hoist: retention";
/// Where the items come from.
pub const SOURCE: &str = "hoist.toml:cloudflare.r2";
/// The two permission groups of the bucket-scoped token, resolved by name in the preflight.
pub const PERMISSION_GROUPS: [&str; 2] = [
    "Workers R2 Storage Bucket Item Read",
    "Workers R2 Storage Bucket Item Write",
];
/// Multipart uploads left open this long are aborted (seconds).
const ABORT_MULTIPART_SECS: i64 = 86_400;

/// `[cloudflare.r2]` as written. `bucket` and `retention_days` are required; that is checked
/// in `desired_from_cfg` so the message names the key.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct R2Cfg {
    pub bucket: Option<String>,
    pub retention_days: Option<i64>,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredR2 {
    pub bucket: String,
    pub retention_days: u32,
    pub prefix: String,
}

impl DesiredR2 {
    /// The rule hoist writes.
    pub fn rule(&self) -> Value {
        json!({
            "id": RULE_ID,
            "enabled": true,
            "conditions": { "prefix": self.prefix },
            "deleteObjectsTransition": {
                "condition": { "type": "Age", "maxAge": i64::from(self.retention_days) * 86_400 },
            },
            "abortMultipartUploadsTransition": {
                "condition": { "type": "Age", "maxAge": ABORT_MULTIPART_SECS },
            },
        })
    }

    /// `hoist:<owner>/<repo>:r2:<bucket>`.
    pub fn token_name(&self, d: &Desired) -> String {
        format!("{}r2:{}", token_prefix(d), self.bucket)
    }
}

/// R2 bucket names: 3 to 63 of `[a-z0-9-]`, no leading or trailing `-`.
fn valid_bucket(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Validate `[cloudflare.r2]`; `file` names hoist.toml in errors. `None` when unset.
pub fn desired_from_cfg(file: &str, cfg: &Option<R2Cfg>) -> Result<Option<DesiredR2>, HoistError> {
    let Some(cfg) = cfg else { return Ok(None) };
    let bad = |message: String| HoistError::Config {
        file: file.to_string(),
        message,
    };
    let bucket = cfg.bucket.clone().ok_or_else(|| {
        bad("`[cloudflare.r2] bucket` is required: the bucket name, e.g. `bucket = \"app-backups\"`".into())
    })?;
    if !valid_bucket(&bucket) {
        return Err(bad(format!(
            "`[cloudflare.r2] bucket = \"{bucket}\"` is not an R2 bucket name: use 3 to 63 \
             characters of a-z, 0-9 and `-`, not starting or ending with `-`"
        )));
    }
    let days = cfg.retention_days.ok_or_else(|| {
        bad("`[cloudflare.r2] retention_days` is required: days to keep objects, 1 to 3650".into())
    })?;
    let retention_days = u32::try_from(days)
        .ok()
        .filter(|d| (1..=3650).contains(d))
        .ok_or_else(|| {
            bad(format!(
                "`[cloudflare.r2] retention_days = {days}` is out of range: use 1 to 3650"
            ))
        })?;
    Ok(Some(DesiredR2 {
        bucket,
        retention_days,
        prefix: cfg.prefix.clone().unwrap_or_default(),
    }))
}

/// What `explain` lists for the section.
pub fn explain_items(d: &Desired) -> Vec<Item> {
    let Some(r2) = &d.cloudflare.r2 else {
        return Vec::new();
    };
    let item = |kind: &str, key: &str, reason: String| Item {
        area: "cloudflare".into(),
        kind: kind.into(),
        key: key.into(),
        action: Action::Ok,
        reason,
        source: SOURCE.into(),
        verify: None,
    };
    vec![
        item("r2_bucket", &r2.bucket, r2.bucket.clone()),
        item(
            "r2_lifecycle",
            &r2.bucket,
            format!(
                "objects under `{}` expire after {} days",
                r2.prefix, r2.retention_days
            ),
        ),
        item(
            "r2_token",
            &r2.token_name(d),
            "bucket-scoped read and write".into(),
        ),
    ]
}

/// Live state of the bucket and its lifecycle rules (the token list is `LiveCloudflare.tokens`).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveR2 {
    pub bucket: Fetch<bool>,
    /// Every lifecycle rule as read; empty when the bucket is missing.
    pub rules: Fetch<Vec<Value>>,
}

fn why(what: &str, hint: &str) -> String {
    format!("cannot read {what}: {hint}")
}

fn bucket_path(account: &str, bucket: &str) -> String {
    format!("/accounts/{}/r2/buckets/{}", seg(account), seg(bucket))
}

fn read_rules(http: &Http, account: &str, bucket: &str) -> Result<Fetch<Vec<Value>>, HttpError> {
    let what = format!("R2 lifecycle of `{bucket}`");
    let path = format!("{}/lifecycle", bucket_path(account, bucket));
    Ok(match http.get_json(&path)? {
        Fetched::Ok(v) => Fetch::Got(
            v.pointer("/result/rules")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        ),
        Fetched::NotFound => Fetch::Got(Vec::new()),
        Fetched::Forbidden(hint) => Fetch::Unknown(why(&what, &hint)),
    })
}

/// Read the bucket and, when it exists, its lifecycle rules. GET only.
pub fn read(http: &Http, account: &str, r2: &DesiredR2) -> Result<LiveR2, HttpError> {
    let what = format!("R2 bucket `{}`", r2.bucket);
    let bucket = match http.get_json(&bucket_path(account, &r2.bucket))? {
        Fetched::Ok(_) => Fetch::Got(true),
        Fetched::NotFound => Fetch::Got(false),
        Fetched::Forbidden(hint) => Fetch::Unknown(why(&what, &hint)),
    };
    let rules = match &bucket {
        Fetch::Got(true) => read_rules(http, account, &r2.bucket)?,
        Fetch::Got(false) => Fetch::Got(Vec::new()),
        Fetch::Unknown(w) => Fetch::Unknown(w.clone()),
    };
    Ok(LiveR2 { bucket, rules })
}

fn max_age(rule: &Value, transition: &str) -> Option<i64> {
    rule.pointer(&format!("/{transition}/condition/maxAge"))
        .and_then(Value::as_i64)
}

/// How the live rule differs from the wanted one, field by field; empty when equal
/// (`enabled`, the prefix and both `maxAge`s).
fn rule_differences(live: &Value, want: &DesiredR2) -> Vec<String> {
    let mut out = Vec::new();
    if live.get("enabled").and_then(Value::as_bool) != Some(true) {
        out.push("enabled: live off, want on".to_string());
    }
    let prefix = live
        .pointer("/conditions/prefix")
        .and_then(Value::as_str)
        .unwrap_or("");
    if prefix != want.prefix {
        out.push(format!("prefix: live `{prefix}`, want `{}`", want.prefix));
    }
    let days = |secs: Option<i64>| match secs {
        None => "none".to_string(),
        Some(s) => format!("{s}s"),
    };
    let want_delete = i64::from(want.retention_days) * 86_400;
    let have_delete = max_age(live, "deleteObjectsTransition");
    if have_delete != Some(want_delete) {
        out.push(format!(
            "retention_days: live delete after {}, want {want_delete}s ({} days)",
            days(have_delete),
            want.retention_days
        ));
    }
    let have_abort = max_age(live, "abortMultipartUploadsTransition");
    if have_abort != Some(ABORT_MULTIPART_SECS) {
        out.push(format!(
            "abort multipart: live {}, want {ABORT_MULTIPART_SECS}s",
            days(have_abort)
        ));
    }
    out
}

/// The `r2_bucket`, `r2_lifecycle` and `r2_token` items. Externally managed Cloudflare
/// resources are never created or updated: `drift`.
pub fn diff(d: &Desired, live: &LiveCloudflare) -> Vec<Item> {
    let Some(r2) = &d.cloudflare.r2 else {
        return Vec::new();
    };
    let managed = d.cloudflare.managed;
    let mut items = Vec::new();
    let mut push = |kind: &str, key: &str, action: Action, reason: String| {
        let action = match (managed, action) {
            (Managed::External, Action::Create | Action::Update) => Action::Drift,
            (_, a) => a,
        };
        items.push(Item {
            area: "cloudflare".into(),
            kind: kind.into(),
            key: key.into(),
            action,
            reason,
            source: SOURCE.into(),
            verify: None,
        });
    };
    let bucket = &r2.bucket;
    match live.r2.as_ref() {
        None => {
            push("r2_bucket", bucket, Action::Unknown, "not read".into());
            push("r2_lifecycle", bucket, Action::Unknown, "not read".into());
        }
        Some(l) => {
            match l.bucket.as_got() {
                Err(w) => push("r2_bucket", bucket, Action::Unknown, w.to_string()),
                Ok(true) => push("r2_bucket", bucket, Action::Ok, "exists".into()),
                Ok(false) => push(
                    "r2_bucket",
                    bucket,
                    Action::Create,
                    "missing; would create the bucket".into(),
                ),
            }
            let missing = matches!(l.bucket.as_got(), Ok(false));
            match l.rules.as_got() {
                _ if missing => push(
                    "r2_lifecycle",
                    bucket,
                    Action::Create,
                    format!("bucket missing; would add rule `{RULE_ID}`"),
                ),
                Err(w) => push("r2_lifecycle", bucket, Action::Unknown, w.to_string()),
                Ok(rules) => {
                    let found = rules
                        .iter()
                        .find(|r| r.get("id").and_then(Value::as_str) == Some(RULE_ID));
                    match found {
                        None => push(
                            "r2_lifecycle",
                            bucket,
                            Action::Create,
                            format!("no rule `{RULE_ID}`; would add it"),
                        ),
                        Some(rule) => match rule_differences(rule, r2).as_slice() {
                            [] => push(
                                "r2_lifecycle",
                                bucket,
                                Action::Ok,
                                format!(
                                    "objects under `{}` expire after {} days",
                                    r2.prefix, r2.retention_days
                                ),
                            ),
                            diffs => push(
                                "r2_lifecycle",
                                bucket,
                                Action::Update,
                                format!("rule `{RULE_ID}` differs, {}", diffs.join("; ")),
                            ),
                        },
                    }
                }
            }
        }
    }
    let name = r2.token_name(d);
    match live.tokens.as_got() {
        Err(w) => push("r2_token", &name, Action::Unknown, w.to_string()),
        Ok(tokens) if tokens.iter().any(|t: &CfToken| t.name == name) => push(
            "r2_token",
            &name,
            Action::Ok,
            "exists; the secret is shown only at creation".into(),
        ),
        Ok(_) => push(
            "r2_token",
            &name,
            Action::Create,
            "missing; would mint a token for this bucket only and deliver its credentials \
             (--secrets-to or --emit-secrets)"
                .into(),
        ),
    }
    items
}

/// One R2 write.
#[derive(Debug, Clone, PartialEq)]
pub enum R2Change {
    Bucket {
        bucket: String,
    },
    /// Replace or append the hoist rule, keeping the other rules as read.
    Lifecycle {
        bucket: String,
        rule: Value,
    },
    /// Mint the bucket-scoped token and deliver its credentials.
    Token {
        name: String,
        bucket: String,
    },
}

impl R2Change {
    /// (area, kind, key) in the scheme of the matching report item.
    pub fn key(&self) -> (&'static str, &'static str, String) {
        match self {
            R2Change::Bucket { bucket } => ("cloudflare", "r2_bucket", bucket.clone()),
            R2Change::Lifecycle { bucket, .. } => ("cloudflare", "r2_lifecycle", bucket.clone()),
            R2Change::Token { name, .. } => ("cloudflare", "r2_token", name.clone()),
        }
    }
}

/// The writes for the `create` and `update` items of `diff`, in dependency order.
pub fn plan(d: &Desired, items: &[Item]) -> Vec<R2Change> {
    let Some(r2) = &d.cloudflare.r2 else {
        return Vec::new();
    };
    let todo = |kind: &str, key: &str| {
        items.iter().any(|i| {
            i.kind == kind && i.key == key && matches!(i.action, Action::Create | Action::Update)
        })
    };
    let mut out = Vec::new();
    if todo("r2_bucket", &r2.bucket) {
        out.push(R2Change::Bucket {
            bucket: r2.bucket.clone(),
        });
    }
    if todo("r2_lifecycle", &r2.bucket) {
        out.push(R2Change::Lifecycle {
            bucket: r2.bucket.clone(),
            rule: r2.rule(),
        });
    }
    let name = r2.token_name(d);
    if todo("r2_token", &name) {
        out.push(R2Change::Token {
            name,
            bucket: r2.bucket.clone(),
        });
    }
    out
}

fn account(d: &Desired) -> Result<&str, HoistError> {
    d.cloudflare
        .account_id
        .as_ref()
        .map(|a| a.value.as_str())
        .ok_or_else(|| {
            HoistError::Usage(
                "no Cloudflare account id; set `[cloudflare] account_id` in hoist.toml or export \
                 CLOUDFLARE_ACCOUNT_ID"
                    .into(),
            )
        })
}

/// The ids of the two R2 permission groups (a GET: apply resolves them before any write).
pub fn permission_groups(cf: &Http) -> Result<Vec<String>, HoistError> {
    permission_group_ids_named(cf, &PERMISSION_GROUPS)
}

/// `POST /accounts/{a}/r2/buckets`.
pub fn create_bucket(cf: &Http, d: &Desired, bucket: &str) -> Result<(), HoistError> {
    let path = format!("/accounts/{}/r2/buckets", seg(account(d)?));
    cf.send_json(Method::Post, &path, Some(&json!({ "name": bucket })))?;
    Ok(())
}

/// Read the rules again, replace (or append) the hoist rule and `PUT` the whole list.
pub fn write_lifecycle(
    cf: &Http,
    d: &Desired,
    bucket: &str,
    rule: &Value,
) -> Result<(), HoistError> {
    let account = account(d)?;
    let live = match read_rules(cf, account, bucket)? {
        Fetch::Got(rules) => rules,
        Fetch::Unknown(w) => return Err(HoistError::Network(w)),
    };
    let is_hoist = |r: &Value| r.get("id").and_then(Value::as_str) == Some(RULE_ID);
    let mut rules = Vec::with_capacity(live.len() + 1);
    let mut placed = false;
    for r in live {
        if !is_hoist(&r) {
            rules.push(r);
        } else if !placed {
            rules.push(rule.clone());
            placed = true;
        }
    }
    if !placed {
        rules.push(rule.clone());
    }
    let path = format!("{}/lifecycle", bucket_path(account, bucket));
    cf.send_json(Method::Put, &path, Some(&json!({ "rules": rules })))?;
    Ok(())
}

/// Mint the user API token that reads and writes `bucket` only. Returns its id and value.
pub fn mint_token(
    cf: &Http,
    d: &Desired,
    name: &str,
    bucket: &str,
    groups: &[String],
) -> Result<(String, Secret), HoistError> {
    let account = account(d)?;
    let scope = MintScope {
        resource: format!("com.cloudflare.edge.r2.bucket.{account}_default_{bucket}"),
        // The credentials cannot be read back or rotated by hoist: a silent expiry would
        // break backups.
        expires: false,
        next: "the credentials were not delivered; delete it, then apply again",
    };
    mint_scoped(cf, name, groups, &scope)
}

/// The four credential lines: access key id (the token id), secret access key (lower-case hex
/// SHA-256 of the token value), bucket and endpoint.
pub fn credentials(
    d: &Desired,
    bucket: &str,
    token_id: &str,
    token_value: &Secret,
) -> Result<Vec<(&'static str, Secret)>, HoistError> {
    let digest = Sha256::digest(token_value.expose().as_bytes());
    let secret = digest.iter().fold(String::with_capacity(64), |mut s, b| {
        s.push_str(&format!("{b:02x}"));
        s
    });
    Ok(vec![
        ("R2_ACCESS_KEY_ID", Secret::from(token_id)),
        ("R2_SECRET_ACCESS_KEY", Secret::from(secret)),
        ("R2_BUCKET", Secret::from(bucket)),
        (
            "R2_ENDPOINT",
            Secret::from(format!("https://{}.r2.cloudflarestorage.com", account(d)?)),
        ),
    ])
}
