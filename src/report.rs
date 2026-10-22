use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Ok,
    Drift,
    Pending,
    Unknown,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Update => "update",
            Action::Ok => "ok",
            Action::Drift => "drift",
            Action::Pending => "pending",
            Action::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verify {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub area: String,
    pub kind: String,
    pub key: String,
    pub action: Action,
    pub reason: String,
    pub source: String,
    pub verify: Option<Verify>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HumanStep {
    pub id: String,
    pub title: String,
    pub url: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub command: String,
    pub items: Vec<Item>,
    pub human_steps: Vec<HumanStep>,
    pub exit_code: i32,
    /// Text printed after the table in table mode (the `import` TOML); never part of `--json`.
    pub text: Option<String>,
    /// The next step, printed on stderr after the table in table mode; never part of `--json`.
    /// Boxed so `Report` stays small enough to be an `Err` variant (clippy result_large_err).
    #[serde(skip)]
    pub hint: Option<Box<str>>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    version: u32,
    command: &'a str,
    ok: bool,
    exit_code: i32,
    items: &'a [Item],
    human_steps: &'a [HumanStep],
}

/// The report as one pretty JSON object (no trailing newline).
pub fn render_json(report: &Report) -> String {
    let view = JsonReport {
        version: 1,
        command: &report.command,
        ok: report.ok(),
        exit_code: report.exit_code,
        items: &report.items,
        human_steps: &report.human_steps,
    };
    serde_json::to_string_pretty(&view).expect("report serializes")
}

/// Human table: resource, action, reason, source; then human steps. Ends with a newline.
pub fn render_table(report: &Report) -> String {
    let mut rows: Vec<[String; 4]> = vec![[
        "RESOURCE".into(),
        "ACTION".into(),
        "REASON".into(),
        "SOURCE".into(),
    ]];
    for item in &report.items {
        rows.push([
            format!("{}/{} {}", item.area, item.kind, item.key),
            item.action.as_str().to_string(),
            item.reason.clone(),
            item.source.clone(),
        ]);
    }
    let mut widths = [0usize; 4];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    if report.items.is_empty() {
        out.push_str("no items\n");
    } else {
        for row in &rows {
            let line: Vec<String> = row
                .iter()
                .zip(widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect();
            out.push_str(line.join("  ").trim_end());
            out.push('\n');
        }
    }
    let failed: Vec<&Item> = report
        .items
        .iter()
        .filter(|i| i.verify.as_ref().is_some_and(|v| !v.ok))
        .collect();
    if !failed.is_empty() {
        out.push_str("\nfailed checks:\n");
        for i in failed {
            let detail = i.verify.as_ref().map_or("", |v| v.detail.as_str());
            out.push_str(&format!("  {}/{} {}: {detail}\n", i.area, i.kind, i.key));
        }
    }
    if !report.human_steps.is_empty() {
        out.push_str("\nhuman steps:\n");
        for step in &report.human_steps {
            out.push_str(&format!(
                "  [{}] {} ({}): {}\n",
                step.status, step.title, step.id, step.url
            ));
        }
    }
    if let Some(text) = &report.text {
        out.push_str(text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            command: "plan".into(),
            items: vec![Item {
                area: "github".into(),
                kind: "ruleset".into(),
                key: "hoist: default branch".into(),
                action: Action::Drift,
                reason: "missing check".into(),
                source: "hoist.toml".into(),
                verify: None,
            }],
            human_steps: vec![],
            exit_code: 1,
            text: None,
            hint: None,
        }
    }

    #[test]
    fn json_has_version_ok_and_lowercase_action() {
        let v: serde_json::Value = serde_json::from_str(&render_json(&sample())).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["ok"], false);
        assert_eq!(v["items"][0]["action"], "drift");
        assert!(v["items"][0]["verify"].is_null());
    }

    #[test]
    fn table_lists_failed_checks_after_the_items() {
        let mut r = sample();
        r.items[0].action = Action::Ok;
        r.items[0].verify = Some(Verify {
            ok: false,
            detail: "x@1.0.0 is not visible on npm".into(),
        });
        let mut passed = r.items[0].clone();
        passed.key = "other".into();
        passed.verify = Some(Verify {
            ok: true,
            detail: "other is fine".into(),
        });
        r.items.push(passed);
        let t = render_table(&r);
        assert!(
            t.contains("\nfailed checks:\n  github/ruleset hoist: default branch: x@1.0.0 is not visible on npm\n"),
            "{t}"
        );
        assert!(!t.contains("other is fine"), "{t}");
        assert!(!render_table(&sample()).contains("failed checks"));
    }

    #[test]
    fn table_lists_item_and_ends_with_newline() {
        let t = render_table(&sample());
        assert!(t.starts_with("RESOURCE"));
        assert!(t.contains("github/ruleset hoist: default branch"));
        assert!(t.ends_with('\n'));
    }
}
