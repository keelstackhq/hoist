//! Desired vs live (pure; no I/O) for GitHub (`diff_github`) and Cloudflare
//! (`diff_cloudflare`). Items use the same area/kind/key scheme as `hoist explain`, and
//! `source` is the desired element's source.

use crate::cloudflare::LiveCloudflare;
use crate::config::Managed;
use crate::desired::Desired;
use crate::github::{Fetch, LiveGithub, RepoSettings, Ruleset};
use crate::repo::Sourced;
use crate::report::{Action, HumanStep, Item};

struct Ctx {
    area: &'static str,
    managed: Managed,
    items: Vec<Item>,
}

impl Ctx {
    fn push(&mut self, kind: &str, key: &str, action: Action, reason: String, source: &str) {
        // Externally managed resources are never created or updated by hoist: that is drift.
        let action = match (self.managed, action) {
            (Managed::External, Action::Create | Action::Update) => Action::Drift,
            (_, a) => a,
        };
        self.items.push(Item {
            area: self.area.to_string(),
            kind: kind.to_string(),
            key: key.to_string(),
            action,
            reason,
            source: source.to_string(),
            verify: None,
        });
    }

    fn unknown(&mut self, kind: &str, key: &str, why: &str, source: &str) {
        self.push(kind, key, Action::Unknown, why.to_string(), source);
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn quoted(list: &[String]) -> String {
    if list.is_empty() {
        return "none".to_string();
    }
    list.iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn setting(
    cx: &mut Ctx,
    live: &Fetch<RepoSettings>,
    key: &str,
    want: &Sourced<String>,
    get: impl Fn(&RepoSettings) -> Option<String>,
) {
    match live.as_got() {
        Err(why) => cx.unknown("repo_setting", key, why, &want.source),
        Ok(s) => match get(s) {
            None => cx.unknown(
                "repo_setting",
                key,
                &format!("cannot read {key}: the token cannot see it (needs admin access)"),
                &want.source,
            ),
            Some(have) if have == want.value => {
                cx.push("repo_setting", key, Action::Ok, have, &want.source)
            }
            Some(have) => cx.push(
                "repo_setting",
                key,
                Action::Update,
                format!("live {have}, want {}", want.value),
                &want.source,
            ),
        },
    }
}

fn applies_to(rs: &Ruleset, branch: &str) -> bool {
    let explicit = format!("refs/heads/{branch}");
    rs.include
        .iter()
        .any(|i| i == "~DEFAULT_BRANCH" || *i == explicit)
}

/// The ruleset's (reason, source) when it is in sync; `hoist explain` prints the same.
/// Source: the first required check's source, or `default` when there is none.
pub fn ruleset_summary(d: &Desired) -> (String, String) {
    let want = &d.github.ruleset;
    match want.required_checks.first() {
        None => (
            "no required checks inferred; ruleset not managed".to_string(),
            "default".to_string(),
        ),
        Some(first) => (
            format!("required checks {}", quoted(&wanted_checks(d))),
            first.source.clone(),
        ),
    }
}

fn ruleset(cx: &mut Ctx, d: &Desired, l: &LiveGithub) {
    let want = &d.github.ruleset;
    let name = &want.name;
    let (in_sync, source) = ruleset_summary(d);
    if want.required_checks.is_empty() {
        cx.push("ruleset", name, Action::Ok, in_sync, &source);
        return;
    }
    let rulesets = match l.rulesets.as_got() {
        Ok(r) => r,
        Err(why) => return cx.unknown("ruleset", name, why, &source),
    };
    let Some(live) = rulesets.iter().find(|r| &r.name == name) else {
        let mut reason = format!("missing; would require {}", quoted(&wanted_checks(d)));
        if let Ok(true) = l.classic_protection.as_got().copied() {
            reason = format!(
                "missing; classic branch protection exists on `{}`",
                want.branch
            );
        }
        return cx.push("ruleset", name, Action::Create, reason, &source);
    };
    let mut problems = Vec::new();
    if let Some(mode) = live.enforcement.as_deref().filter(|m| *m != "active") {
        problems.push(format!("enforcement `{mode}`, want `active`"));
    }
    if !applies_to(live, &want.branch) {
        problems.push(format!("does not target default branch `{}`", want.branch));
    }
    let mut have = live.required_checks.clone();
    let mut need = wanted_checks(d);
    have.sort();
    need.sort();
    if have != need {
        problems.push(format!(
            "required checks: live {}, want {}",
            quoted(&have),
            quoted(&need)
        ));
    }
    if live.merge_queue != want.merge_queue.value {
        problems.push(format!(
            "merge queue: live {}, want {}",
            on_off(live.merge_queue),
            on_off(want.merge_queue.value)
        ));
    }
    if problems.is_empty() {
        cx.push("ruleset", name, Action::Ok, in_sync, &source);
    } else {
        cx.push(
            "ruleset",
            name,
            Action::Update,
            problems.join("; "),
            &source,
        );
    }
}

fn wanted_checks(d: &Desired) -> Vec<String> {
    d.github
        .ruleset
        .required_checks
        .iter()
        .map(|c| c.value.clone())
        .collect()
}

/// What hoist can do about a missing secret.
enum Producer {
    /// Hoist writes it.
    Yes,
    /// Hoist could, but the precondition cannot be checked (reason).
    Blind(String),
    /// A human sets it.
    No,
}

/// Secrets hoist writes itself: `CLOUDFLARE_ACCOUNT_ID`, and `CLOUDFLARE_API_TOKEN` when the
/// Cloudflare token list was read (hoist never mints blind). Both need a Cloudflare target,
/// an account id and `managed = "hoist"` for Cloudflare.
fn producer(d: &Desired, cf: Option<&LiveCloudflare>, name: &str) -> Producer {
    let c = &d.cloudflare;
    let ready = c.managed == Managed::Hoist
        && c.account_id.is_some()
        && !(c.pages.is_empty() && c.workers.is_empty());
    match name {
        "CLOUDFLARE_ACCOUNT_ID" if ready => Producer::Yes,
        "CLOUDFLARE_API_TOKEN" if ready => match cf.map(|l| l.tokens.as_got()) {
            Some(Ok(_)) => Producer::Yes,
            Some(Err(why)) => Producer::Blind(why.to_string()),
            None => Producer::Blind("Cloudflare was not read".to_string()),
        },
        _ => Producer::No,
    }
}

/// Presence check for a list of named things (environments, secrets).
fn presence(
    cx: &mut Ctx,
    kind: &str,
    want: &[Sourced<String>],
    live: &Fetch<Vec<String>>,
    missing: impl Fn(&str) -> String,
) {
    for w in want {
        match live.as_got() {
            Err(why) => cx.unknown(kind, &w.value, why, &w.source),
            Ok(have) if have.contains(&w.value) => {
                cx.push(kind, &w.value, Action::Ok, "present".to_string(), &w.source)
            }
            Ok(_) => cx.push(kind, &w.value, Action::Create, missing(&w.value), &w.source),
        }
    }
}

/// Diff the desired GitHub state against what was read. Returns the items plus the human
/// steps (missing GitHub Apps), sorted by (area, kind, key).
pub fn diff_github(
    d: &Desired,
    l: &LiveGithub,
    cf: Option<&LiveCloudflare>,
) -> (Vec<Item>, Vec<HumanStep>) {
    let g = &d.github;
    let mut cx = Ctx {
        area: "github",
        managed: g.managed,
        items: Vec::new(),
    };
    let mut steps = Vec::new();

    if let Some(v) = &g.settings.visibility {
        setting(&mut cx, &l.settings, "visibility", v, |s| {
            Some(s.visibility.clone())
        });
    }
    let flag = |b: bool| b.to_string();
    let want = Sourced {
        value: flag(g.settings.allow_auto_merge.value),
        source: g.settings.allow_auto_merge.source.clone(),
    };
    setting(&mut cx, &l.settings, "allow_auto_merge", &want, |s| {
        s.allow_auto_merge.map(flag)
    });
    let want = Sourced {
        value: flag(g.settings.delete_branch_on_merge.value),
        source: g.settings.delete_branch_on_merge.source.clone(),
    };
    setting(&mut cx, &l.settings, "delete_branch_on_merge", &want, |s| {
        s.delete_branch_on_merge.map(flag)
    });

    ruleset(&mut cx, d, l);

    let want = &g.actions_permissions;
    match l.actions_permissions.as_got() {
        Err(why) => cx.unknown("actions_permissions", "default", why, &want.source),
        Ok(have) if *have == want.value => cx.push(
            "actions_permissions",
            "default",
            Action::Ok,
            have.clone(),
            &want.source,
        ),
        Ok(have) => cx.push(
            "actions_permissions",
            "default",
            Action::Update,
            format!("live {have}, want {}", want.value),
            &want.source,
        ),
    }

    presence(
        &mut cx,
        "environment",
        &g.environments,
        &l.environments,
        |_| "missing; would create the environment".to_string(),
    );
    for w in &g.secrets {
        let name = &w.value;
        match l.secrets.as_got() {
            Err(why) => cx.unknown("secret", name, why, &w.source),
            Ok(have) if have.contains(name) => {
                cx.push("secret", name, Action::Ok, "present".to_string(), &w.source)
            }
            Ok(_) => {
                let hint = format!("missing; set with `hoist secrets set {name}` (M2)");
                let reason = match &l.org_secrets_note {
                    Some(note) => format!("{hint}; {note}"),
                    None => hint,
                };
                match producer(d, cf, name) {
                    Producer::Yes => cx.push("secret", name, Action::Create, reason, &w.source),
                    Producer::Blind(why) => cx.unknown(
                        "secret",
                        name,
                        &format!("cannot list Cloudflare tokens: {why}; hoist will not mint blind"),
                        &w.source,
                    ),
                    Producer::No => {
                        // Hoist cannot produce this value: a human sets it.
                        cx.push("secret", name, Action::Pending, reason, &w.source);
                        steps.push(HumanStep {
                            id: format!("secret:{name}"),
                            title: format!("set secret {name}"),
                            url: format!("hoist secrets set {name}"),
                            status: "pending".to_string(),
                        });
                    }
                }
            }
        }
    }

    for app in &g.apps {
        let slug = &app.value;
        match l.apps.as_got() {
            Err(why) => cx.unknown("app", slug, why, &app.source),
            Ok(have) if have.contains(slug) => cx.push(
                "app",
                slug,
                Action::Ok,
                "installed".to_string(),
                &app.source,
            ),
            Ok(_) => {
                cx.push(
                    "app",
                    slug,
                    Action::Pending,
                    format!("GitHub App `{slug}` is not installed on this repo"),
                    &app.source,
                );
                steps.push(HumanStep {
                    id: format!("app:{slug}"),
                    title: format!(
                        "Install the {slug} GitHub App on {}/{}",
                        d.repo.owner, d.repo.name
                    ),
                    url: format!("https://github.com/apps/{slug}"),
                    status: "pending".to_string(),
                });
            }
        }
    }

    cx.items
        .sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    (cx.items, steps)
}

/// Diff the desired Cloudflare state against what was read. Items are sorted by
/// (area, kind, key); no human steps arise in M1, the shape matches `diff_github`.
pub fn diff_cloudflare(d: &Desired, l: &LiveCloudflare) -> (Vec<Item>, Vec<HumanStep>) {
    let cf = &d.cloudflare;
    let mut cx = Ctx {
        area: "cloudflare",
        managed: cf.managed,
        items: Vec::new(),
    };
    for p in &cf.pages {
        let name = &p.project.value;
        let live = l.pages.iter().find(|(n, _)| n == name).map(|(_, f)| f);
        let live = match live.map(Fetch::as_got) {
            None => Err("not read"),
            Some(r) => r,
        };
        match live {
            Err(why) => {
                cx.unknown("pages_project", name, why, &p.project.source);
                if let Some(dom) = &p.domain {
                    cx.unknown("custom_domain", &dom.value, why, &dom.source);
                }
            }
            Ok(None) => {
                cx.push(
                    "pages_project",
                    name,
                    Action::Create,
                    "missing; would create the Pages project".to_string(),
                    &p.project.source,
                );
                if let Some(dom) = &p.domain {
                    cx.push(
                        "custom_domain",
                        &dom.value,
                        Action::Create,
                        format!("missing; Pages project `{name}` does not exist yet"),
                        &dom.source,
                    );
                }
            }
            Ok(Some(proj)) => {
                if proj.production_branch == d.default_branch {
                    cx.push(
                        "pages_project",
                        name,
                        Action::Ok,
                        format!("production branch `{}`", proj.production_branch),
                        &p.project.source,
                    );
                } else {
                    cx.push(
                        "pages_project",
                        name,
                        Action::Update,
                        format!(
                            "production branch: live `{}`, want `{}`",
                            proj.production_branch, d.default_branch
                        ),
                        &p.project.source,
                    );
                }
                if let Some(dom) = &p.domain {
                    if proj.domains.contains(&dom.value) {
                        cx.push(
                            "custom_domain",
                            &dom.value,
                            Action::Ok,
                            "attached".to_string(),
                            &dom.source,
                        );
                    } else {
                        cx.push(
                            "custom_domain",
                            &dom.value,
                            Action::Create,
                            format!("missing; would attach to Pages project `{name}`"),
                            &dom.source,
                        );
                    }
                }
            }
        }
    }
    for w in &cf.workers {
        let live = l
            .workers
            .iter()
            .find(|(n, _)| *n == w.value)
            .map(|(_, f)| f);
        match live.map(Fetch::as_got) {
            None => cx.unknown("workers_script", &w.value, "not read", &w.source),
            Some(Err(why)) => cx.unknown("workers_script", &w.value, why, &w.source),
            Some(Ok(true)) => cx.push(
                "workers_script",
                &w.value,
                Action::Ok,
                "deployed".to_string(),
                &w.source,
            ),
            Some(Ok(false)) => cx.push(
                "workers_script",
                &w.value,
                Action::Pending,
                "created by the first deploy".to_string(),
                &w.source,
            ),
        }
    }
    cx.items
        .sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    (cx.items, Vec::new())
}
