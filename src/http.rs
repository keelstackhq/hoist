//! The only module that talks to the network. GET only in M1. Errors and logs never carry
//! headers, tokens or bodies.

use crate::auth::Secret;
use crate::error::HoistError;
use serde_json::Value;
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Github,
    Cloudflare,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Service::Github => "GitHub",
            Service::Cloudflare => "Cloudflare",
        }
    }

    fn base_from_env(self) -> String {
        let (var, default) = match self {
            Service::Github => ("HOIST_GITHUB_API", "https://api.github.com"),
            Service::Cloudflare => (
                "HOIST_CLOUDFLARE_API",
                "https://api.cloudflare.com/client/v4",
            ),
        };
        std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| default.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    Ok(Value),
    NotFound,
    /// Permission hint from the response body, else the status text.
    Forbidden(String),
}

/// A failed call. `Display` names the service and path only.
#[derive(Debug)]
pub enum HttpError {
    Transport {
        service: Service,
        path: String,
        detail: String,
    },
    InvalidJson {
        service: Service,
        path: String,
    },
    Status {
        service: Service,
        path: String,
        status: u16,
    },
    /// More pages than the cap, or a pager that does not advance.
    PageLimit {
        service: Service,
        path: String,
    },
    /// `result_info.page` is not the page that was requested.
    BadPage {
        service: Service,
        path: String,
        got: u64,
        wanted: u32,
    },
    /// A `Link` target that is not on the configured API base.
    BadLink {
        service: Service,
        path: String,
    },
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::PageLimit { service, path } => write!(
                f,
                "{} pagination for GET {} did not finish within the page limit; refusing to \
                 return a partial list",
                service.name(),
                bare(path)
            ),
            HttpError::BadLink { service, path } => write!(
                f,
                "{} sent a pagination link outside the API base for GET {}",
                service.name(),
                bare(path)
            ),
            HttpError::Transport {
                service,
                path,
                detail,
            } => write!(
                f,
                "{} request GET {} failed: {detail}. Check your network and the API base URL",
                service.name(),
                bare(path)
            ),
            HttpError::InvalidJson { service, path } => write!(
                f,
                "{} returned invalid JSON for GET {}",
                service.name(),
                bare(path)
            ),
            HttpError::BadPage {
                service,
                path,
                got,
                wanted,
            } => write!(
                f,
                "{} returned page {got} when page {wanted} was requested for GET {}",
                service.name(),
                bare(path)
            ),
            HttpError::Status {
                service,
                path,
                status,
            } => write!(
                f,
                "{} returned unexpected status {status} for GET {}",
                service.name(),
                bare(path)
            ),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<HttpError> for HoistError {
    fn from(err: HttpError) -> Self {
        HoistError::Network(err.to_string())
    }
}

struct Response {
    status: u16,
    body: Vec<u8>,
    link_next: Option<String>,
    retry_after: Option<u64>,
    rate_remaining_zero: bool,
}

pub struct Http {
    service: Service,
    token: Secret,
    base: String,
    agent: ureq::Agent,
    verbose: bool,
    max_sleep_ms: Option<u64>,
    max_pages: u32,
}

const MAX_ATTEMPTS: u32 = 3;
const MAX_RETRY_AFTER_SECS: u64 = 60;

impl Http {
    /// Base URL from `HOIST_GITHUB_API` / `HOIST_CLOUDFLARE_API`; sleep cap from
    /// `HOIST_MAX_SLEEP_MS`.
    pub fn new(service: Service, token: Secret) -> Http {
        let mut http = Http::with_base(service, token, &service.base_from_env());
        http.max_sleep_ms = std::env::var("HOIST_MAX_SLEEP_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok());
        http
    }

    pub fn with_base(service: Service, token: Secret, base: &str) -> Http {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .build()
            .into();
        Http {
            service,
            token,
            base: base.trim_end_matches('/').to_string(),
            agent,
            verbose: false,
            max_sleep_ms: None,
            max_pages: 1000,
        }
    }

    /// Log `GET <path> <status> <ms>` to stderr for every request.
    pub fn verbose(mut self, on: bool) -> Http {
        self.verbose = on;
        self
    }

    /// Upper bound on a rate-limit sleep, in milliseconds (tests use 0).
    pub fn max_sleep_ms(mut self, ms: u64) -> Http {
        self.max_sleep_ms = Some(ms);
        self
    }

    /// Cap on pages followed by `get_all` (default 1000); hitting it is an error.
    pub fn max_pages(mut self, n: u32) -> Http {
        self.max_pages = n;
        self
    }

    pub fn get_json(&self, path: &str) -> Result<Fetched, HttpError> {
        match self.fetch(path)? {
            Err(fetched) => Ok(fetched),
            Ok(resp) => Ok(Fetched::Ok(self.parse(path, &resp.body)?)),
        }
    }

    /// All pages of a list endpoint as one JSON array. GitHub follows `Link: rel="next"`;
    /// `array_key` names the array inside an object body (else the body is the array).
    /// Cloudflare concatenates `result` across `result_info.page/total_pages`.
    pub fn get_all(&self, path: &str, array_key: Option<&str>) -> Result<Fetched, HttpError> {
        match self.service {
            Service::Github => self.get_all_github(path, array_key),
            Service::Cloudflare => self.get_all_cloudflare(path),
        }
    }

    fn get_all_github(&self, path: &str, array_key: Option<&str>) -> Result<Fetched, HttpError> {
        let mut items = Vec::new();
        let mut current = with_query(path, "per_page=100");
        let mut pages = 0u32;
        loop {
            let resp = match self.fetch(&current)? {
                Err(fetched) => return Ok(fetched),
                Ok(resp) => resp,
            };
            let body = self.parse(&current, &resp.body)?;
            let page = match array_key {
                Some(key) => body.get(key).cloned(),
                None => Some(body),
            };
            match page {
                Some(Value::Array(mut more)) => items.append(&mut more),
                _ => return Err(self.invalid(&current)),
            }
            pages += 1;
            let Some(link) = resp.link_next else { break };
            if pages >= self.max_pages {
                return Err(self.page_limit(&current));
            }
            current = self.relative(&link).ok_or_else(|| HttpError::BadLink {
                service: self.service,
                path: current.clone(),
            })?;
        }
        Ok(Fetched::Ok(Value::Array(items)))
    }

    fn get_all_cloudflare(&self, path: &str) -> Result<Fetched, HttpError> {
        let mut items = Vec::new();
        let mut page = 1u32;
        loop {
            let current = with_query(path, &format!("page={page}"));
            let resp = match self.fetch(&current)? {
                Err(fetched) => return Ok(fetched),
                Ok(resp) => resp,
            };
            let body = self.parse(&current, &resp.body)?;
            match body.get("result") {
                Some(Value::Array(more)) => items.extend(more.iter().cloned()),
                _ => return Err(self.invalid(&current)),
            }
            let info = body.get("result_info");
            let at = info.and_then(|i| i.get("page")).and_then(Value::as_u64);
            let total = info
                .and_then(|i| i.get("total_pages"))
                .and_then(Value::as_u64);
            // Our own counter drives the loop; a server that does not echo the page we
            // asked for (a stuck pager) is an error, never an endless loop.
            if let Some(got) = at.filter(|&got| got != u64::from(page)) {
                return Err(HttpError::BadPage {
                    service: self.service,
                    path: current,
                    got,
                    wanted: page,
                });
            }
            match total {
                Some(total) if u64::from(page) < total => {
                    if page >= self.max_pages {
                        return Err(self.page_limit(&current));
                    }
                    page += 1;
                }
                _ => break,
            }
        }
        Ok(Fetched::Ok(Value::Array(items)))
    }

    /// Turn a `Link` target into a request path: relative paths pass through, absolute URLs
    /// must stay on the configured base (the token is never sent elsewhere).
    fn relative(&self, link: &str) -> Option<String> {
        if link.starts_with('/') {
            return Some(link.to_string());
        }
        link.strip_prefix(&self.base)
            .filter(|rest| rest.starts_with('/'))
            .map(str::to_string)
    }

    fn page_limit(&self, path: &str) -> HttpError {
        HttpError::PageLimit {
            service: self.service,
            path: path.to_string(),
        }
    }

    fn invalid(&self, path: &str) -> HttpError {
        HttpError::InvalidJson {
            service: self.service,
            path: path.to_string(),
        }
    }

    fn parse(&self, path: &str, body: &[u8]) -> Result<Value, HttpError> {
        serde_json::from_slice(body).map_err(|_| self.invalid(path))
    }

    /// One logical GET with rate-limit retries. `Err(fetched)` is a non-success outcome that
    /// callers return as is.
    fn fetch(&self, path: &str) -> Result<Result<Response, Fetched>, HttpError> {
        let mut attempt = 1;
        loop {
            let resp = self.send(path)?;
            let limited = resp.status == 429
                || (resp.status == 403 && (resp.retry_after.is_some() || resp.rate_remaining_zero));
            if limited {
                if attempt >= MAX_ATTEMPTS {
                    return Ok(Err(Fetched::Forbidden("rate limited".to_string())));
                }
                self.sleep(resp.retry_after.unwrap_or(1));
                attempt += 1;
                continue;
            }
            return match resp.status {
                200..=299 => Ok(Ok(resp)),
                404 => Ok(Err(Fetched::NotFound)),
                401 => Ok(Err(Fetched::Forbidden("token rejected (401)".to_string()))),
                403 => Ok(Err(Fetched::Forbidden(hint(&resp.body, 403)))),
                status => Err(HttpError::Status {
                    service: self.service,
                    path: path.to_string(),
                    status,
                }),
            };
        }
    }

    fn sleep(&self, retry_after_secs: u64) {
        let mut ms = retry_after_secs.min(MAX_RETRY_AFTER_SECS) * 1000;
        if let Some(cap) = self.max_sleep_ms {
            ms = ms.min(cap);
        }
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }

    fn send(&self, path: &str) -> Result<Response, HttpError> {
        let url = format!("{}{path}", self.base);
        let accept = match self.service {
            Service::Github => "application/vnd.github+json",
            Service::Cloudflare => "application/json",
        };
        let mut request = self
            .agent
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token.expose()))
            .header("Accept", accept)
            .header("User-Agent", concat!("hoist/", env!("CARGO_PKG_VERSION")));
        if self.service == Service::Github {
            request = request.header("X-GitHub-Api-Version", "2022-11-28");
        }
        let started = Instant::now();
        let result = request.call();
        let transport = |detail: String| HttpError::Transport {
            service: self.service,
            path: path.to_string(),
            detail,
        };
        let mut response = match result {
            Ok(response) => response,
            Err(err) => {
                if self.verbose {
                    eprintln!("{}", log_line(path, "error", started.elapsed().as_millis()));
                }
                return Err(transport(err.to_string()));
            }
        };
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let link_next = header("link").and_then(|v| parse_link_next(&v));
        let retry_after = header("retry-after").and_then(|v| v.trim().parse().ok());
        let rate_remaining_zero = header("x-ratelimit-remaining").is_some_and(|v| v.trim() == "0");
        let body = response
            .body_mut()
            .read_to_vec()
            .map_err(|err| transport(err.to_string()))?;
        if self.verbose {
            eprintln!(
                "{}",
                log_line(path, &status.to_string(), started.elapsed().as_millis())
            );
        }
        Ok(Response {
            status,
            body,
            link_next,
            retry_after,
            rate_remaining_zero,
        })
    }
}

/// The path without its query string (cursors and filters stay out of logs and errors).
fn bare(path: &str) -> &str {
    path.split('?').next().unwrap_or(path)
}

fn log_line(path: &str, outcome: &str, ms: u128) -> String {
    format!("GET {} {outcome} {ms}ms", bare(path))
}

fn with_query(path: &str, param: &str) -> String {
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("{path}{sep}{param}")
}

/// Permission hint: GitHub `message`, Cloudflare `errors[0].message`, else the status text.
fn hint(body: &[u8], status: u16) -> String {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    let from_body = json.as_ref().and_then(|v| {
        v.get("message")
            .and_then(Value::as_str)
            .or_else(|| {
                v.get("errors")
                    .and_then(|e| e.get(0))
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
    });
    from_body.unwrap_or_else(|| match status {
        403 => "Forbidden".to_string(),
        other => format!("status {other}"),
    })
}

/// The `rel="next"` target of a `Link` header.
fn parse_link_next(value: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| p.trim().replace(' ', "") == "rel=\"next\"");
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then(|| target.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_log_drop_the_query() {
        let path = || "/x?after=SECRETCURSOR".to_string();
        let service = Service::Github;
        let errors = [
            HttpError::Transport {
                service,
                path: path(),
                detail: "refused".into(),
            },
            HttpError::InvalidJson {
                service,
                path: path(),
            },
            HttpError::Status {
                service,
                path: path(),
                status: 500,
            },
            HttpError::PageLimit {
                service,
                path: path(),
            },
            HttpError::BadLink {
                service,
                path: path(),
            },
            HttpError::BadPage {
                service,
                path: path(),
                got: 1,
                wanted: 2,
            },
        ];
        for err in errors {
            let text = err.to_string();
            assert!(!text.contains("SECRETCURSOR"), "{text}");
            assert!(text.contains("GET /x"), "{text}");
        }
        assert_eq!(log_line(&path(), "200", 12), "GET /x 200 12ms");
        assert_eq!(log_line(&path(), "error", 12), "GET /x error 12ms");
    }

    #[test]
    fn link_next_is_found() {
        let header = r#"<https://x/a?page=1>; rel="prev", <https://x/a?page=3>; rel="next""#;
        assert_eq!(
            parse_link_next(header).as_deref(),
            Some("https://x/a?page=3")
        );
        assert_eq!(parse_link_next(r#"<https://x/a>; rel="last""#), None);
    }

    #[test]
    fn hint_reads_github_and_cloudflare_bodies() {
        assert_eq!(hint(br#"{"message":"nope"}"#, 403), "nope");
        assert_eq!(
            hint(br#"{"errors":[{"message":"bad scope"}]}"#, 403),
            "bad scope"
        );
        assert_eq!(hint(b"<html>", 403), "Forbidden");
    }
}
