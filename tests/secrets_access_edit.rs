//! The new `ci` recipient `hoist secrets grant` adds to `access.toml` (spec §15.4): every rule
//! under its env, nothing else changed, and refused when a consumer, name or key is ambiguous.
use hoist::secrets::access::Access;
use hoist::secrets::access_edit::{NewCi, add_ci_recipient, default_name};

const NOW: i64 = 1_793_059_200; // 2026-10-27

fn key(c: char) -> String {
    format!("age1{}", c.to_string().repeat(58))
}

/// An access.toml with a human owner and rules for prod and preview, plus `extra` recipients.
fn access(extra: &str) -> String {
    format!(
        "version = 1\n\n# the owner\n[[recipient]]\nname = \"owner-2026\"\nkind = \"human\"\nage = \"{}\"\nexpires = \"2027-10-27\"\n{extra}\n[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\"]\n\n[[rule]]\npath = \"preview/**\"\nrecipients = [\"owner-2026\"]\n",
        key('q')
    )
}

fn ci(name: &str, repo: &str, env: &str, age: &str) -> String {
    format!(
        "\n[[recipient]]\nname = \"{name}\"\nkind = \"ci\"\nage = \"{age}\"\nrepo = \"{repo}\"\nenv = \"{env}\"\nprovider = \"github\"\nexpires = \"2027-10-27\"\n"
    )
}

fn new<'a>(name: &'a str, repo: &'a str, age: &'a str) -> NewCi<'a> {
    NewCi {
        name,
        repo,
        env: "prod",
        age,
        added: "2026-10-27",
        expires: "2027-10-27",
        rotating: None,
    }
}

#[test]
fn a_new_ci_key_joins_every_rule_under_its_env_and_nothing_else_changes() {
    assert_eq!(default_name("acme/My.Site", "prod"), "my-site-prod");
    let before = access("");
    let after = add_ci_recipient(&before, NOW, &new("site-prod", "acme/site", &key('p'))).unwrap();
    let parsed = Access::parse(&after, "access.toml", NOW).unwrap();
    let r = parsed
        .recipients
        .iter()
        .find(|r| r.name == "site-prod")
        .unwrap();
    assert_eq!(
        (r.repo.as_deref(), r.env.as_deref(), r.age.as_str()),
        (Some("acme/site"), Some("prod"), key('p').as_str())
    );
    for rule in &parsed.rules {
        let has = rule.recipients.iter().any(|n| n == "site-prod");
        assert_eq!(has, rule.path.starts_with("prod/"), "{}", rule.path);
    }
    // Comments and the order of what was there are kept.
    assert!(
        after.starts_with(&before[..before.find("[[rule]]").unwrap()]),
        "{after}"
    );
    assert!(after.contains("added = \"2026-10-27\""), "{after}");
}

#[test]
fn a_new_ci_key_is_refused_when_it_would_be_ambiguous_or_left_out() {
    let p = key('p');
    let taken = access(&ci("site-prod", "other/site", "prod", &key('z')));
    let granted = access(&ci("old-name", "acme/site", "prod", &key('z')));
    let shared = access(&ci("web-prod", "acme/web", "prod", &p));
    // Rules as one inline array at the top: valid, but not a form hoist edits.
    let plain = access("");
    let inline = format!(
        "version = 1\nrule = [{{ path = \"prod/**\", recipients = [\"owner-2026\"] }}]\n{}",
        &plain["version = 1\n".len()..plain.find("[[rule]]").unwrap()]
    );
    assert!(
        Access::parse(&inline, "access.toml", NOW).is_ok(),
        "{inline}"
    );
    let no_rule = access("").replace("prod/**", "staging/**");
    let upper = access(&ci("old-name", "Acme/Site", "prod", &key('z')));
    let cases = [
        (taken, new("site-prod", "acme/site", &p), "--name"),
        (
            granted.clone(),
            new("site-prod", "acme/site", &p),
            "rotate-key old-name",
        ),
        (shared, new("site-prod", "acme/site", &p), "web-prod"),
        (inline, new("site-prod", "acme/site", &p), "[[rule]]"),
        (no_rule, new("site-prod", "acme/site", &p), "no rule"),
        (
            granted.clone(),
            new("Site_Prod", "acme/site", &p),
            "[a-z0-9-]",
        ),
        (
            upper,
            new("site-prod", "acme/site", &p),
            "rotate-key old-name",
        ),
        (
            granted.clone(),
            NewCi {
                rotating: Some("other"),
                ..new("site-prod", "acme/site", &p)
            },
            "nothing to rotate",
        ),
    ];
    for (text, grant, want) in cases {
        let got = add_ci_recipient(&text, NOW, &grant);
        assert!(
            got.as_ref().is_err_and(|e| e.to_string().contains(want)),
            "{want}: {:?}",
            got.map_err(|e| e.to_string())
        );
    }
}

#[test]
fn a_rotation_adds_the_new_key_beside_the_one_it_replaces() {
    // The owner took the old key off `prod/db/**`: the new one stays off it too.
    let granted = access(&ci("site-prod", "acme/site", "prod", &key('z'))).replace(
        "[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\"]",
        "[[rule]]\npath = \"prod/db/**\"\nrecipients = [\"owner-2026\"]\n\n[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\", \"site-prod\"]",
    );
    let p = key('p');
    let rotation = NewCi {
        rotating: Some("site-prod"),
        ..new("site-prod-202610", "acme/site", &p)
    };
    let after = add_ci_recipient(&granted, NOW, &rotation).unwrap();
    let parsed = Access::parse(&after, "access.toml", NOW).unwrap();
    let prod = parsed.rules.iter().find(|r| r.path == "prod/**").unwrap();
    assert_eq!(
        prod.recipients,
        ["owner-2026", "site-prod", "site-prod-202610"]
    );
    let db = parsed
        .rules
        .iter()
        .find(|r| r.path == "prod/db/**")
        .unwrap();
    assert_eq!(db.recipients, ["owner-2026"]);
    // A rotation of a recipient this consumer does not have is refused.
    for other in ["nobody", "owner-2026"] {
        let wrong = NewCi {
            rotating: Some(other),
            ..new("site-prod-202610", "acme/site", &p)
        };
        let err = add_ci_recipient(&granted, NOW, &wrong)
            .map(|_| ())
            .unwrap_err();
        assert!(
            err.to_string().contains("nothing to rotate"),
            "{other}: {err}"
        );
    }
}
