//! The access.toml model (hoist spec §15.2).

use hoist::secrets::access::{Access, Kind};

const HUMAN: &str = "age1pa8pm3arjta5297ffjhgl3j7u5rm5eyf528sp67v78ykj70q8yqsttyjlw";
const CI: &str = "age1wrr8jch3fe87hgx6uxrhpk847ep0hpkchj8gpqwtxtmylrhxn33shujsln";
const SECOND: &str = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";

fn access_text() -> String {
    format!(
        r#"version = 1

[[recipient]]
name = "owner-2026"
kind = "human"
age = "{HUMAN}"
expires = "2027-10-27"

[[recipient]]
name = "keelstack-prod"
kind = "ci"
repo = "keelstackhq/keelstack"
env = "prod"
provider = "github"
age = "{CI}"
added = "2026-10-27"
expires = "2027-10-27"

[[recipient]]
name = "owner-safe"
kind = "human"
age = "{SECOND}"
expires = "2027-06-01"

[[rule]]
path = "prod/**"
recipients = ["owner-2026", "keelstack-prod"]

[[rule]]
path = "preview/*.sops.yaml"
recipients = ["owner-2026", "owner-safe"]
"#
    )
}

/// 2026-10-27 as Unix seconds (the spec's date).
const NOW: i64 = 1_793_059_200;

fn access_err(text: &str) -> String {
    let err = Access::parse(text, "access.toml", NOW).expect_err("must fail");
    assert_eq!(err.exit_code(), 3);
    err.to_string()
}

#[test]
fn access_parses_and_matches_first_rule() {
    let a = Access::parse(&access_text(), "access.toml", NOW).expect("valid");
    assert_eq!(a.recipients[1].kind, Kind::Ci);
    let rule = a.rule_for("prod/cloudflare.sops.yaml").expect("prod rule");
    assert_eq!(rule.recipients, ["owner-2026", "keelstack-prod"]);
    assert_eq!(a.rule_for("prod/deep/x.sops.yaml").unwrap().path, "prod/**");
    assert!(a.rule_for("preview/deep/x.sops.yaml").is_none());
    assert!(a.rule_for("staging/x.sops.yaml").is_none());
    for p in ["/prod/a", "prod//a", "prod/./a", "prod/../preview/a"] {
        assert!(a.rule_for(p).is_none(), "{p}");
    }
    assert_eq!(a.public_keys(rule), [HUMAN, CI]);
    assert_eq!([0, 2].map(|i| a.recipients[i].days_left(NOW)), [365, 217]);
    assert_eq!(a.recipients[2].days_left(NOW + 187 * 86_400), 30); // `check` warns at <= 30
}

/// Each case is `from | to | want`: replace `from` with `to` in the valid text, expect `want`.
#[test]
fn access_errors_name_the_problem() {
    let cases = [
        r#"["owner-2026", "keelstack | ["keelstack | line 26: rule `prod/**`: no human"#,
        r#""owner-safe"] | "keelstack-prod"] | line 30: rule `preview/*.sops.yaml`: ci recipient `keelstack-prod` (env prod)"#,
        r#""owner-safe"] | "AGE-SECRET-KEY-1MARK"] | line 30: rule `preview/*.sops.yaml`: a `recipients` entry is not"#,
        r#""owner-safe"] | "owner-2026"] | line 30: rule `preview/*.sops.yaml`: `owner-2026` is listed twice"#,
        r#"path = "preview/*.sops.yaml | path = "prod/** | line 30: rule `prod/**` is defined twice"#,
        r#"name = "owner-safe | name = "owner-2026 | line 20: duplicate recipient `owner-2026`"#,
        r#"name = "owner-safe | name = "AGE-SECRET-KEY-1MARK | line 20: recipient name must match"#,
        r#"provider = "github"\n |  | line 10: `keelstack-prod`: kind ci needs repo, env and provider"#,
        r#"kind = "human"\nage = "age1ql3 | kind = "human"\nenv = "prod"\nage = "age1ql3 | line 20: `owner-safe`: kind human takes no"#,
        r#"env = "prod | env = "* | line 13: `keelstack-prod`: env must match [a-z0-9][a-z0-9-]*"#,
        r#"env = "prod | env = "AGE-SECRET-KEY-1MARK | line 13: `keelstack-prod`: env must match"#,
        r#"repo = "keelstackhq/keelstack | repo = "-x/MARK | line 12: `keelstack-prod`: repo must be `owner/name`"#,
        r#"age1wrr8jch3fe87 | AGE-SECRET-KEY-1MARK | line 15: `keelstack-prod`: `age` is not"#,
        r#"age1wrr8jch3fe87 | age1WRR8jch3fe87 | line 15: `keelstack-prod`: `age` is not"#,
        r#"age1wrr8jch3fe87 | age1brr8jch3fe87 | line 15: `keelstack-prod`: `age` is not"#,
        r#"hxn33shujsln | hxn33shujsl | line 15: `keelstack-prod`: `age` is not"#,
        r#"kind = "ci" | kind = "robot" | line 11: unknown variant"#,
        r#"added = "2026-10-27 | added = "2026-09-26 | line 17: `keelstack-prod`: `expires` is more than 13 months after `added`"#,
        r#"2026-10-27"\nexpires = "2027-10-27 | 2026-01-31"\nexpires = "2027-03-01 | 13 months after `added`"#,
        r#"added = "2026-10-27 | added = "2026-10-28 | line 16: `keelstack-prod`: `added` is in the future"#,
        r#"2026-10-27"\nexpires = "2027-10-27 | 2026-10-27"\nexpires = "2026-10-26 | line 17: `keelstack-prod`: `expires` is before `added`"#,
        r#""2027-06-01 | "2027-11-28 | line 23: `owner-safe`: `expires` is more than 13 months after today"#,
        r#""2027-06-01 | "2027-6-1 | line 23: `owner-safe`: `expires` is not a date YYYY-MM-DD"#,
        r#"added = "2026-10-27 | added = "+026-10-27 | `added` is not a date"#,
        r#"path = "prod | path = "/MARK | line 26: rule path must be a relative glob"#,
        r#"path = "prod/** | path = "prod/../preview/** | line 26: rule path must be"#,
        r#"path = "prod/** | path = "prod//** | line 26: rule path must be"#,
        r#"path = "prod/** | path = "./prod/** | line 26: rule path must be"#,
    ];
    for case in cases {
        let [from, to, want] = case.splitn(3, " | ").collect::<Vec<_>>()[..] else {
            panic!("bad case {case}");
        };
        let (from, to) = (from.replace("\\n", "\n"), to.replace("\\n", "\n"));
        let msg = access_err(&access_text().replace(&from, &to));
        assert!(msg.contains(want), "{to}: {msg}");
        let leaked = ["robot", "2027-", "2026-", "MARK"]
            .iter()
            .any(|b| msg.contains(b));
        assert!(!leaked, "{msg}");
    }
    // Exactly 13 months (from `added`, clamped to the month end, and from today) is allowed.
    let text = access_text().replace(
        "2026-10-27\"\nexpires = \"2027-10-27",
        "2026-01-31\"\nexpires = \"2027-02-28",
    );
    let text = text.replace("\"2027-06-01", "\"2027-11-27");
    let text = text.replace("\"2027-10-27\"\n\n", "\"2026-01-01\"\n\n"); // expired: drift, not an error
    Access::parse(&text, "access.toml", NOW).expect("13 months is allowed");
    let no_rules = &access_text()[..access_text().find("[[rule]]").unwrap()];
    assert!(access_err(no_rules).contains("no [[rule]]"));
}
