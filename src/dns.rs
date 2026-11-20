//! `[cloudflare.dns]` (spec §6.3): config validation, the Fly address read, the live zone read,
//! the diff into `dns_record` items and the typed writes. Self-contained: the shared modules
//! only call `plan` (read and diff), `execute` (one write) and `validate` (config).

use crate::config::Managed;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use crate::report::{Action, HumanStep, Item};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::IpAddr;
use std::process::Stdio;

pub const SOURCE: &str = "hoist.toml:cloudflare.dns";
const DASHBOARD: &str = "https://dash.cloudflare.com/";
const SPF: &str = "v=spf1 -all";
const DMARC: &str = "v=DMARC1; p=reject";

/// `targets`: a list of IP addresses, or the string `"fly"`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Targets {
    Name(String),
    Ips(Vec<String>),
}

/// `[cloudflare.dns]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsCfg {
    pub zone: String,
    pub host: String,
    pub targets: Targets,
    #[serde(default)]
    pub fly_app: Option<String>,
    #[serde(default)]
    pub proxied: Option<bool>,
    #[serde(default)]
    pub no_mail: bool,
}

/// Check the section; the message says how to fix it (callers make it a config error).
pub fn validate(cfg: &DnsCfg) -> Result<(), String> {
    let at = "`[cloudflare.dns]`";
    if cfg.proxied == Some(true) {
        return Err(format!(
            "{at} `proxied = true` is not supported: a proxied record breaks Let's Encrypt HTTP \
             challenges and replaces the client IP; remove it (DNS only is the default)"
        ));
    }
    let (zone, host) = (cfg.zone.to_lowercase(), cfg.host.to_lowercase());
    if zone.is_empty() || host.is_empty() {
        return Err(format!("{at} `zone` and `host` must not be empty"));
    }
    if host != zone && !host.ends_with(&format!(".{zone}")) {
        return Err(format!(
            "{at} host `{}` is not in zone `{}`: use the zone itself or a name under it",
            cfg.host, cfg.zone
        ));
    }
    match &cfg.targets {
        Targets::Name(n) if n != "fly" => Err(format!(
            "{at} `targets = \"{n}\"` is not valid: use a list of IP addresses or \"fly\""
        )),
        Targets::Name(_) => Ok(()),
        Targets::Ips(list) => {
            if cfg.fly_app.is_some() {
                return Err(format!(
                    "{at} `fly_app` only applies with `targets = \"fly\"`"
                ));
            }
            if list.is_empty() {
                return Err(format!(
                    "{at} `targets` is empty: list at least one IP address"
                ));
            }
            for ip in list {
                ip.parse::<IpAddr>().map_err(|_| {
                    format!("{at} target `{ip}` is not an IP address (IPv4 or IPv6)")
                })?;
            }
            Ok(())
        }
    }?;
    if matches!(cfg.targets, Targets::Name(_)) && cfg.fly_app.as_deref() == Some("") {
        return Err(format!("{at} `fly_app` must not be empty"));
    }
    if let Some(app) = cfg.fly_app.as_deref().filter(|a| !a.is_empty())
        && !crate::fly::valid_app(app)
    {
        return Err(format!(
            "{at} `fly_app = \"{app}\"` is not an app name: use letters, digits, `.`, `_` and `-`"
        ));
    }
    Ok(())
}

/// One record hoist wants: `ty` is `A`, `AAAA` or `TXT`.
#[derive(Debug, Clone, PartialEq)]
struct Want {
    ty: &'static str,
    name: String,
    content: String,
}

impl Want {
    fn key(&self) -> String {
        format!("{} {} {}", self.name, self.ty, self.content)
    }
}

fn address_want(host: &str, ip: IpAddr) -> Want {
    Want {
        ty: if ip.is_ipv4() { "A" } else { "AAAA" },
        name: host.to_string(),
        content: ip.to_string(),
    }
}

/// `fly ips list [-a app] --json`: every public address (`v4`, `shared_v4`, `v6`).
fn fly_addresses(app: Option<&str>) -> Result<Vec<IpAddr>, String> {
    let mut args = vec!["ips", "list"];
    if let Some(app) = app {
        args.extend(["-a", app]);
    }
    args.push("--json");
    let shown = format!("fly {}", args.join(" "));
    let output = crate::fly::fly_command(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run `{shown}`: {e}; install fly or set HOIST_FLY"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let line = err.lines().next().unwrap_or("").trim();
        return Err(format!(
            "`{shown}` failed ({}){}{line}",
            output.status,
            if line.is_empty() { "" } else { ": " }
        ));
    }
    let list: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("`{shown}` did not print JSON: {e}"))?;
    let list = list
        .as_array()
        .ok_or_else(|| format!("`{shown}` did not print a JSON array"))?;
    let mut ips = Vec::new();
    for entry in list {
        let ty = entry.get("Type").and_then(Value::as_str).unwrap_or("");
        if !matches!(ty, "v4" | "shared_v4" | "v6") {
            continue;
        }
        let ip = entry
            .get("Address")
            .and_then(Value::as_str)
            .and_then(|a| a.parse::<IpAddr>().ok())
            .ok_or_else(|| format!("`{shown}` listed an address hoist cannot read"))?;
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    if ips.is_empty() {
        return Err(format!("`{shown}` lists no public IP address"));
    }
    Ok(ips)
}

/// The A/AAAA targets, or why they cannot be known.
fn targets(cfg: &DnsCfg) -> Result<Vec<IpAddr>, String> {
    match &cfg.targets {
        Targets::Name(_) => fly_addresses(cfg.fly_app.as_deref()),
        Targets::Ips(list) => {
            let mut ips: Vec<IpAddr> = Vec::new();
            for ip in list.iter().filter_map(|s| s.parse::<IpAddr>().ok()) {
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
            Ok(ips)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Rec {
    id: String,
    ty: String,
    name: String,
    content: String,
    proxied: bool,
}

fn parse_rec(v: &Value) -> Option<Rec> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Rec {
        id: text("id")?,
        ty: text("type")?,
        name: text("name")?.to_lowercase(),
        content: text("content").unwrap_or_default(),
        proxied: v.get("proxied").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// A TXT value as Cloudflare may return it: without the surrounding quotes.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

enum Live {
    /// Why the zone or its records cannot be read.
    Unreadable(String),
    Read {
        zone_id: String,
        /// Records at the host and at `_dmarc.<host>` (only read with `no_mail`).
        host: Vec<Rec>,
        dmarc: Vec<Rec>,
    },
}

fn records(http: &Http, zone_id: &str, name: &str) -> Result<Result<Vec<Rec>, String>, HttpError> {
    let path = format!("/zones/{}/dns_records?name={}", seg(zone_id), seg(name));
    Ok(match http.get_all(&path, None)? {
        Fetched::Ok(Value::Array(list)) => Ok(list.iter().filter_map(parse_rec).collect()),
        Fetched::Ok(_) => Err(format!("unexpected response from GET {path}")),
        Fetched::NotFound => Err(format!("GET {path} not found (404)")),
        Fetched::Forbidden(hint) => Err(format!("cannot read DNS records of `{name}`: {hint}")),
    })
}

fn read_live(http: &Http, cfg: &DnsCfg) -> Result<Live, HttpError> {
    let zone_path = format!("/zones?name={}", seg(&cfg.zone));
    let zone_id = match http.get_json(&zone_path)? {
        Fetched::Ok(v) => match v
            .pointer("/result/0/id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            Some(id) => id,
            None => {
                return Ok(Live::Unreadable(format!(
                    "zone `{}` is not visible to this token (GET {zone_path} lists none)",
                    cfg.zone
                )));
            }
        },
        Fetched::NotFound => {
            return Ok(Live::Unreadable(format!("GET {zone_path} not found (404)")));
        }
        Fetched::Forbidden(hint) => {
            return Ok(Live::Unreadable(format!(
                "cannot read zone `{}`: {hint}",
                cfg.zone
            )));
        }
    };
    let host = match records(http, &zone_id, &cfg.host)? {
        Ok(r) => r,
        Err(why) => return Ok(Live::Unreadable(why)),
    };
    let dmarc = match cfg.no_mail {
        false => Vec::new(),
        true => match records(http, &zone_id, &format!("_dmarc.{}", cfg.host))? {
            Ok(r) => r,
            Err(why) => return Ok(Live::Unreadable(why)),
        },
    };
    Ok(Live::Read {
        zone_id,
        host,
        dmarc,
    })
}

/// What `apply` does for one `dns_record` item.
#[derive(Debug, Clone, PartialEq)]
pub enum DnsOp {
    Create {
        ty: String,
        name: String,
        content: String,
    },
    Unproxy {
        id: String,
    },
    SetContent {
        id: String,
        content: String,
    },
    Delete {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DnsWrite {
    pub zone_id: String,
    /// The `dns_record` item key this write stands for.
    pub key: String,
    pub op: DnsOp,
}

pub struct DnsPlan {
    pub items: Vec<Item>,
    pub steps: Vec<HumanStep>,
    pub writes: Vec<DnsWrite>,
}

struct Out {
    managed: Managed,
    adopt: bool,
    plan: DnsPlan,
}

impl Out {
    fn item(&mut self, key: &str, action: Action, reason: String) {
        // An externally managed zone is never written: create and update are drift.
        let action = match (self.managed, action) {
            (Managed::External, Action::Create | Action::Update) => Action::Drift,
            (_, a) => a,
        };
        self.plan.items.push(Item {
            area: "cloudflare".into(),
            kind: "dns_record".into(),
            key: key.to_string(),
            action,
            reason,
            source: SOURCE.into(),
            verify: None,
        });
    }

    fn write(&mut self, zone_id: &str, key: &str, op: DnsOp) {
        if self.managed == Managed::Hoist {
            self.plan.writes.push(DnsWrite {
                zone_id: zone_id.to_string(),
                key: key.to_string(),
                op,
            });
        }
    }

    fn manual_step(&mut self, zone: &str, w: &Want, why: &str) {
        self.item(&w.key(), Action::Unknown, why.to_string());
        self.plan.steps.push(HumanStep {
            id: format!("dns:{}", w.key()),
            title: format!(
                "in zone {zone} add {} {} {}, DNS only (grey cloud), TTL auto",
                w.ty, w.name, w.content
            ),
            url: DASHBOARD.into(),
            status: "pending".into(),
        });
    }
}

/// An SPF or DMARC record: the wanted one is kept or updated, the others are reported.
fn text_record(out: &mut Out, zone_id: &str, live: &[Rec], want: &Want, prefix: &str) {
    let ours: Vec<&Rec> = live
        .iter()
        .filter(|r| {
            r.ty == "TXT"
                && r.name == want.name
                && unquote(&r.content).to_lowercase().starts_with(prefix)
        })
        .collect();
    let chosen = ours
        .iter()
        .find(|r| unquote(&r.content) == want.content)
        .or(ours.first())
        .copied();
    let key = want.key();
    match chosen {
        None => {
            out.item(&key, Action::Create, "missing; would add the record".into());
            out.write(
                zone_id,
                &key,
                DnsOp::Create {
                    ty: "TXT".into(),
                    name: want.name.clone(),
                    content: want.content.clone(),
                },
            );
        }
        Some(r) if unquote(&r.content) == want.content => {
            out.item(&key, Action::Ok, "present".into());
        }
        Some(r) => {
            out.item(
                &key,
                Action::Update,
                format!(
                    "content: live `{}`, want `{}`",
                    unquote(&r.content),
                    want.content
                ),
            );
            out.write(
                zone_id,
                &key,
                DnsOp::SetContent {
                    id: r.id.clone(),
                    content: want.content.clone(),
                },
            );
        }
    }
    for r in ours
        .iter()
        .filter(|r| Some(r.id.as_str()) != chosen.map(|c| c.id.as_str()))
    {
        out.item(
            &format!("{} TXT {}", r.name, unquote(&r.content)),
            Action::Drift,
            format!("a second `{prefix}` record; delete it by hand"),
        );
    }
}

fn address_records(out: &mut Out, zone_id: &str, live: &[Rec], host: &str, ips: &[IpAddr]) {
    let cname = live.iter().find(|r| r.ty == "CNAME" && r.name == host);
    for ip in ips {
        let want = address_want(host, *ip);
        let key = want.key();
        let have = live.iter().find(|r| {
            r.ty == want.ty && r.name == host && r.content.parse::<IpAddr>().ok() == Some(*ip)
        });
        match have {
            Some(r) if r.proxied => {
                out.item(
                    &key,
                    Action::Drift,
                    "proxied (orange cloud); want DNS only".into(),
                );
                out.write(zone_id, &key, DnsOp::Unproxy { id: r.id.clone() });
            }
            Some(_) => out.item(&key, Action::Ok, "present, DNS only".into()),
            None => match cname {
                // A CNAME cannot coexist with an address record: the human step comes first.
                Some(_) => out.item(
                    &key,
                    Action::Pending,
                    format!("blocked by the CNAME at {host}; delete it first"),
                ),
                None => {
                    out.item(&key, Action::Create, "missing; would add the record".into());
                    out.write(
                        zone_id,
                        &key,
                        DnsOp::Create {
                            ty: want.ty.into(),
                            name: host.to_string(),
                            content: want.content.clone(),
                        },
                    );
                }
            },
        }
    }
    for r in live
        .iter()
        .filter(|r| r.name == host && matches!(r.ty.as_str(), "A" | "AAAA"))
    {
        let target = r
            .content
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|ip| ips.contains(&ip));
        if !target {
            let key = format!("{} {} {}", r.name, r.ty, r.content);
            match out.adopt {
                true => out.item(&key, Action::Drift, "not a target; kept (--adopt)".into()),
                false => {
                    out.item(&key, Action::Drift, "not a target".into());
                    out.write(zone_id, &key, DnsOp::Delete { id: r.id.clone() });
                }
            }
        }
    }
    if let Some(c) = cname {
        out.item(
            &format!("{} CNAME {}", c.name, c.content),
            Action::Drift,
            "a CNAME cannot coexist with A/AAAA records; delete it by hand".into(),
        );
        out.plan.steps.push(HumanStep {
            id: format!("dns-cname:{host}"),
            title: format!(
                "delete the CNAME at {host} (to {}) in the Cloudflare dashboard",
                c.content
            ),
            url: DASHBOARD.into(),
            status: "pending".into(),
        });
    }
}

/// Read the zone (with `http`, absent without a Cloudflare token) and diff `[cloudflare.dns]`.
/// GET requests only. A 401 is an error; a missing token, an unreadable zone or a failing
/// `fly` make the affected records `unknown` (spec §6.3).
pub fn plan(d: &Desired, http: Option<&Http>) -> Result<DnsPlan, HoistError> {
    let Some(cfg) = &d.cloudflare.dns else {
        return Ok(DnsPlan {
            items: Vec::new(),
            steps: Vec::new(),
            writes: Vec::new(),
        });
    };
    let host = cfg.host.to_lowercase();
    let mut out = Out {
        managed: d.cloudflare.managed,
        adopt: d.adopt,
        plan: DnsPlan {
            items: Vec::new(),
            steps: Vec::new(),
            writes: Vec::new(),
        },
    };
    let ips = targets(cfg);
    let mail: Vec<(Want, &str)> = match cfg.no_mail {
        false => Vec::new(),
        true => vec![
            (
                Want {
                    ty: "TXT",
                    name: host.clone(),
                    content: SPF.into(),
                },
                "v=spf1",
            ),
            (
                Want {
                    ty: "TXT",
                    name: format!("_dmarc.{host}"),
                    content: DMARC.into(),
                },
                "v=dmarc1",
            ),
        ],
    };
    let live = match http {
        Some(http) => read_live(http, cfg)?,
        None => Live::Unreadable(
            "no Cloudflare token; set CLOUDFLARE_API_TOKEN (Zone Read, DNS Write on the zone)"
                .into(),
        ),
    };
    match live {
        Live::Unreadable(why) => {
            match &ips {
                Ok(ips) => {
                    for ip in ips {
                        out.manual_step(&cfg.zone, &address_want(&host, *ip), &why);
                    }
                }
                Err(e) => out.item(&format!("{host} A/AAAA"), Action::Unknown, e.clone()),
            }
            for (want, _) in &mail {
                out.manual_step(&cfg.zone, want, &why);
            }
        }
        Live::Read {
            zone_id,
            host: host_live,
            dmarc,
        } => {
            match &ips {
                Ok(ips) => address_records(&mut out, &zone_id, &host_live, &host, ips),
                Err(e) => out.item(&format!("{host} A/AAAA"), Action::Unknown, e.clone()),
            }
            for (want, prefix) in &mail {
                let live = if want.name == host {
                    &host_live
                } else {
                    &dmarc
                };
                text_record(&mut out, &zone_id, live, want, prefix);
            }
        }
    }
    Ok(out.plan)
}

/// Run one write.
pub fn execute(http: &Http, w: &DnsWrite) -> Result<(), HoistError> {
    let zone = seg(&w.zone_id);
    let base = format!("/zones/{zone}/dns_records");
    match &w.op {
        DnsOp::Create { ty, name, content } => {
            let mut body = json!({"type": ty, "name": name, "content": content, "ttl": 1});
            if ty != "TXT" {
                body["proxied"] = json!(false);
            }
            http.send_json(Method::Post, &base, Some(&body))?;
        }
        DnsOp::Unproxy { id } => {
            http.send_json(
                Method::Patch,
                &format!("{base}/{}", seg(id)),
                Some(&json!({"proxied": false})),
            )?;
        }
        DnsOp::SetContent { id, content } => {
            http.send_json(
                Method::Patch,
                &format!("{base}/{}", seg(id)),
                Some(&json!({"content": content})),
            )?;
        }
        DnsOp::Delete { id } => {
            http.send_json(Method::Delete, &format!("{base}/{}", seg(id)), None)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml: &str) -> DnsCfg {
        toml::from_str(toml).expect("parses")
    }

    #[test]
    fn unquote_strips_one_pair_of_quotes() {
        assert_eq!(unquote("\"v=spf1 -all\""), "v=spf1 -all");
        assert_eq!(unquote("v=spf1 -all"), "v=spf1 -all");
    }

    #[test]
    fn validate_accepts_the_zone_and_names_under_it() {
        let ok = |host: &str| {
            validate(&cfg(&format!(
                "zone = \"Example.com\"\nhost = \"{host}\"\ntargets = [\"::1\"]"
            )))
        };
        assert!(ok("example.com").is_ok());
        assert!(ok("a.b.example.com").is_ok());
        assert!(ok("notexample.com").is_err());
    }
}
