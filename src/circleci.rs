//! CircleCI contexts for `hoist secrets grant|revoke|rotate-key --provider circleci` (spec
//! §15.8.3), through CircleCI's v3 API: a context per consumer environment, its
//! `SOPS_AGE_KEY`, and the restrictions that decide which pipelines may use it. Values are only
//! written, never read back (the API returns names only). The token is `CIRCLECI_TOKEN`.

use crate::auth::Secret;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Method, seg};
use serde_json::{Value, json};

/// A CircleCI project: its id (the target of a project restriction) and its organization's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub org_id: String,
}

/// One context restriction: `kind` is `project`, `expression` or `group`; `pattern` is the
/// project or group id, or the pipeline expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    pub id: String,
    pub kind: String,
    pub pattern: String,
}

/// The CircleCI token, from `CIRCLECI_TOKEN` only.
pub fn token() -> Result<Secret, HoistError> {
    std::env::var("CIRCLECI_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
        .map(Secret::from)
        .ok_or_else(|| {
            HoistError::Usage(
                "no CircleCI token found. Set CIRCLECI_TOKEN to a personal API token of an \
                 organization admin (it manages contexts and their restrictions)"
                    .into(),
            )
        })
}

/// `filter[key]=value`, encoded.
fn filter(key: &str, value: &str) -> String {
    format!("filter%5B{key}%5D={}", seg(value))
}

/// Every item of a v3 list, or why not.
fn list(http: &Http, path: &str, what: &str) -> Result<Vec<Value>, HoistError> {
    match http.get_all(path, None)? {
        Fetched::Ok(Value::Array(items)) => Ok(items),
        Fetched::Ok(_) => Err(fail(what, "not a list".into())),
        Fetched::NotFound => Err(fail(what, "404".into())),
        Fetched::Forbidden(why) => Err(fail(what, why)),
    }
}

fn fail(what: &str, why: String) -> HoistError {
    HoistError::Usage(format!(
        "CircleCI: cannot read {what} ({why}); the token needs to be an organization admin's"
    ))
}

/// A string field of a v3 item, or an error naming what was malformed.
fn field<'a>(item: &'a Value, pointer: &str, what: &str) -> Result<&'a str, HoistError> {
    item.pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            HoistError::Usage(format!(
                "CircleCI: {what} without `{pointer}` in the answer"
            ))
        })
}

/// The project with this slug (`gh/<owner>/<name>`, or `circleci/<org>/<project>` in an
/// organization connected through the GitHub App), or `None`.
pub fn project(http: &Http, slug: &str) -> Result<Option<Project>, HoistError> {
    let what = format!("project {slug}");
    let path = format!("/api/v3/projects?{}", filter("slug", slug));
    let Some(item) = list(http, &path, &what)?.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(Project {
        id: field(&item, "/id", &what)?.into(),
        org_id: field(&item, "/references/org/id", &what)?.into(),
    }))
}

/// The id of the context named exactly `name` in organization `org_id`, or `None`. (The API's
/// name filter matches any case-insensitive substring.)
pub fn find_context(http: &Http, org_id: &str, name: &str) -> Result<Option<String>, HoistError> {
    let what = format!("the contexts of organization {org_id}");
    let path = format!(
        "/api/v3/contexts?{}&{}",
        filter("org_id", org_id),
        filter("name", name)
    );
    for item in list(http, &path, &what)? {
        if field(&item, "/attributes/name", "a context")? == name {
            return Ok(Some(field(&item, "/id", "a context")?.into()));
        }
    }
    Ok(None)
}

/// A new context `name` in organization `org_id`; its id.
pub fn create_context(http: &Http, org_id: &str, name: &str) -> Result<String, HoistError> {
    let body = json!({"data": {"attributes": {"name": name},
        "references": {"org": {"id": org_id}}}});
    let made = http.send_json(Method::Post, "/api/v3/contexts", Some(&body))?;
    let made = made.body.unwrap_or_default();
    Ok(field(&made, "/data/id", "the new context")?.into())
}

/// The names of the context's environment variables (the API never returns values).
pub fn env_var_names(http: &Http, context: &str) -> Result<Vec<String>, HoistError> {
    let what = format!("the variables of context {context}");
    let path = format!("/api/v3/contexts/{}/env-vars", seg(context));
    list(http, &path, &what)?
        .iter()
        .map(|v| field(v, "/attributes/name", "a variable").map(str::to_string))
        .collect()
}

/// Sets (adds or replaces) variable `name` of the context to `value`.
pub fn set_env_var(
    http: &Http,
    context: &str,
    name: &str,
    value: &Secret,
) -> Result<(), HoistError> {
    let body = json!({"name": name, "value": value.expose()});
    let path = format!("/api/v3/contexts/{}/env-vars/set", seg(context));
    http.send_json(Method::Post, &path, Some(&body))?;
    Ok(())
}

/// Deletes variable `name` of the context.
pub fn delete_env_var(http: &Http, context: &str, name: &str) -> Result<(), HoistError> {
    let path = format!(
        "/api/v3/contexts/{}/env-vars?{}",
        seg(context),
        filter("name", name)
    );
    http.send_json(Method::Delete, &path, None)?;
    Ok(())
}

fn restriction(item: &Value, at: &str) -> Result<Restriction, HoistError> {
    let get = |p: &str| field(item, &format!("{at}{p}"), "a restriction").map(str::to_string);
    Ok(Restriction {
        id: get("/id")?,
        kind: get("/attributes/restriction_type")?,
        pattern: get("/attributes/match_pattern")?,
    })
}

/// Every restriction of the context.
pub fn restrictions(http: &Http, context: &str) -> Result<Vec<Restriction>, HoistError> {
    let what = format!("the restrictions of context {context}");
    let path = format!(
        "/api/v3/context-restrictions?{}",
        filter("context_id", context)
    );
    list(http, &path, &what)?
        .iter()
        .map(|r| restriction(r, ""))
        .collect()
}

/// Adds a restriction of `kind` matching `pattern` to the context.
pub fn add_restriction(
    http: &Http,
    context: &str,
    kind: &str,
    pattern: &str,
) -> Result<Restriction, HoistError> {
    let body = json!({"data": {
        "attributes": {"restriction_type": kind, "match_pattern": pattern},
        "references": {"context": {"id": context}}}});
    let made = http.send_json(Method::Post, "/api/v3/context-restrictions", Some(&body))?;
    restriction(&made.body.unwrap_or_default(), "/data")
}
