//! The consumer's GitHub environment, read before `hoist secrets grant` stores a key in it (spec
//! §15.8.2). GET only.

use crate::error::HoistError;
use crate::http::{Fetched, Http, seg};
use serde_json::Value;

/// Environment `env` of repository `owner/name`.
pub struct Consumer<'a> {
    pub owner: &'a str,
    pub name: &'a str,
    pub env: &'a str,
}

impl Consumer<'_> {
    fn base(&self) -> String {
        let (o, n, e) = (seg(self.owner), seg(self.name), seg(self.env));
        format!("/repos/{o}/{n}/environments/{e}")
    }

    fn what(&self) -> String {
        format!("environment {} of {}/{}", self.env, self.owner, self.name)
    }

    fn get(&self, http: &Http, path: &str) -> Result<Option<Value>, HoistError> {
        match http.get_json(path)? {
            Fetched::Ok(v) => Ok(Some(v)),
            Fetched::NotFound => Ok(None),
            Fetched::Forbidden(msg) => Err(self.forbidden(path, &msg)),
        }
    }

    fn forbidden(&self, path: &str, msg: &str) -> HoistError {
        HoistError::Network(format!(
            "GitHub refused GET {path} (403) for {}: {msg}. The token needs Environments and \
             Secrets read access",
            self.what()
        ))
    }
}

/// Refuses an environment that anything but its own ref can deploy to: a workflow pushed on
/// another branch or tag could name it and print the key (spec §15.8.2). Every custom rule must
/// be branch `main`; for `release`, tag `*-v*` plus a required reviewer. Protected branches do
/// not pass: without a classic protection rule, GitHub lets every branch deploy.
pub fn require_branch_rules(http: &Http, c: &Consumer) -> Result<(), HoistError> {
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets grant: {msg}")));
    let Some(env) = c.get(http, &c.base())? else {
        return usage(format!(
            "no environment {} in {}/{}; create it with deployment branch rules (`main` only, \
             spec §15.8.2), then grant",
            c.env, c.owner, c.name
        ));
    };
    let (kind, name) = match c.env {
        "release" => ("tag", "*-v*"),
        _ => ("branch", "main"),
    };
    let policy = &env["deployment_branch_policy"];
    let custom = policy["custom_branch_policies"] == Value::Bool(true)
        && policy["protected_branches"] != Value::Bool(true);
    let mut rules = Vec::new();
    if custom {
        let path = format!("{}/deployment-branch-policies", c.base());
        match http.get_all(&path, Some("branch_policies"))? {
            Fetched::Ok(Value::Array(all)) => rules = all,
            Fetched::Ok(_) | Fetched::NotFound => {}
            Fetched::Forbidden(msg) => return Err(c.forbidden(&path, &msg)),
        }
    }
    // A rule without a type is a branch rule (GitHub's default).
    let only = |r: &Value| r["type"].as_str().unwrap_or("branch") == kind && r["name"] == name;
    if rules.is_empty() || !rules.iter().all(only) {
        return usage(format!(
            "{} must let only {kind} `{name}` deploy (selected branches and tags, spec §15.8.2), \
             or a workflow pushed elsewhere could read SOPS_AGE_KEY; set its deployment \
             branches, then grant",
            c.what()
        ));
    }
    // Anyone who can push a release tag chooses the workflow it runs: an owner approves.
    let reviewed = env["protection_rules"].as_array().is_some_and(|all| {
        all.iter().any(|r| {
            r["type"] == "required_reviewers"
                && r["reviewers"].as_array().is_some_and(|a| !a.is_empty())
        })
    });
    if kind == "tag" && !reviewed {
        return usage(format!(
            "{} needs a required reviewer (the owner, spec §15.8.2): any release tag pushed \
             runs the workflow it points at; add one, then grant",
            c.what()
        ));
    }
    Ok(())
}

/// Whether the environment already has a `SOPS_AGE_KEY` secret (its value is never readable).
pub fn holds_age_key(http: &Http, c: &Consumer) -> Result<bool, HoistError> {
    let path = format!("{}/secrets/SOPS_AGE_KEY", c.base());
    Ok(c.get(http, &path)?.is_some())
}
