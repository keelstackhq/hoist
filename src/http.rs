//! The only module that talks to the network. Reads use `get_json`/`get_all`; writes use
//! `send_json` (or `send_bytes` for raw bodies). Errors and logs never carry headers, tokens or
//! request bodies.

use crate::auth::Secret;
use crate::error::HoistError;
use serde_json::Value;
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Github,
    Cloudflare,
    /// Release asset uploads (`uploads.github.com`).
    GithubUploads,
    /// The npm registry; reads are anonymous.
    Npm,
    /// The crates.io API; every request carries [`CRATES_USER_AGENT`].
    Crates,
    /// The GitHub Actions OIDC token endpoint (`ACTIONS_ID_TOKEN_REQUEST_URL`); always built
    /// with [`Http::with_base`].
    ActionsOidc,
}

/// crates.io answers 403 to requests without an identifying User-Agent.
pub const CRATES_USER_AGENT: &str = concat!(
    "hoist/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/keelstackhq/hoist)"
);

impl Service {
    fn name(self) -> &'static str {
        match self {
            Service::Github => "GitHub",
            Service::Cloudflare => "Cloudflare",
            Service::GithubUploads => "GitHub uploads",
            Service::Npm => "npm registry",
            Service::Crates => "crates.io",
            Service::ActionsOidc => "GitHub Actions OIDC",
        }
    }

    fn base_from_env(self) -> String {
        let (var, default) = match self {
            Service::Github => ("HOIST_GITHUB_API", "https://api.github.com"),
            Service::Cloudflare => (
                "HOIST_CLOUDFLARE_API",
                "https://api.cloudflare.com/client/v4",
            ),
            Service::GithubUploads => ("HOIST_GITHUB_UPLOADS", "https://uploads.github.com"),
            Service::Npm => ("HOIST_NPM_REGISTRY", "https://registry.npmjs.org"),
            Service::Crates => ("HOIST_CRATES_API", "https://crates.io"),
            Service::ActionsOidc => panic!(
                "Service::ActionsOidc has no base URL from env; build it with Http::with_base"
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
    /// 403 (or an exhausted rate limit): the API's own message, else the status text.
    Forbidden(String),
}

/// A write method. Reads go through `get_json` / `get_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }

    /// `POST` creates and is not idempotent: it is never retried.
    fn retries(self) -> bool {
        !matches!(self, Method::Post)
    }
}

/// A successful write. `body` is `None` for 204 or an empty body.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub status: u16,
    pub body: Option<Value>,
}

/// Percent-encode one URL path segment: everything except `A-Za-z0-9-._~`.
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A failed call. `Display` names the service, method and path (no query) only.
#[derive(Debug)]
pub enum HttpError {
    Transport {
        service: Service,
        method: &'static str,
        path: String,
        detail: String,
    },
    InvalidJson {
        service: Service,
        method: &'static str,
        path: String,
    },
    Status {
        service: Service,
        path: String,
        status: u16,
    },
    /// More pages than the cap, or a pager that does not advance.
    PageLimit { service: Service, path: String },
    /// `result_info.page` is not the page that was requested.
    BadPage {
        service: Service,
        path: String,
        got: u64,
        wanted: u32,
    },
    /// A `Link` target that is not on the configured API base.
    BadLink { service: Service, path: String },
    /// 401: the token is invalid or expired. Never degraded to "unknown", which would let a
    /// plan with a dead token read as in sync.
    Unauthorized { service: Service },
    /// A request hoist refused to follow (a redirect off the API host): deliberate, not a
    /// network failure.
    Refused {
        service: Service,
        path: String,
        detail: &'static str,
    },
    /// A write the API refused (4xx/5xx other than 401). `message` is the API's own message,
    /// never the request body.
    Rejected {
        service: Service,
        method: Method,
        path: String,
        status: u16,
        message: String,
    },
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Unauthorized { service } => write!(
                f,
                "{} rejected the token (401); check {}",
                service.name(),
                match service {
                    Service::Github | Service::GithubUploads => "GITHUB_TOKEN/GH_TOKEN",
                    Service::Cloudflare => "CLOUDFLARE_API_TOKEN",
                    Service::Npm | Service::Crates => "the registry credentials",
                    Service::ActionsOidc =>
                        "ACTIONS_ID_TOKEN_REQUEST_TOKEN (the job needs `permissions: id-token: write`)",
                }
            ),
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
            HttpError::Refused {
                service,
                path,
                detail,
            } => write!(f, "{} GET {}: {detail}", service.name(), bare(path)),
            HttpError::Transport {
                service,
                method,
                path,
                detail,
            } => write!(
                f,
                "{} request {method} {} failed: {detail}. Check your network and the API base URL",
                service.name(),
                bare(path)
            ),
            HttpError::InvalidJson {
                service,
                method,
                path,
            } => write!(
                f,
                "{} returned invalid JSON for {method} {}",
                service.name(),
                bare(path)
            ),
            HttpError::Rejected {
                service,
                method,
                path,
                status,
                message,
            } => write!(
                f,
                "{} rejected {} {} ({status}): {message}",
                service.name(),
                method.as_str(),
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

/// Why `get_same_origin` stopped: a redirect it refused, or a failed request.
enum SameOrigin {
    Refused(&'static str),
    Ureq(ureq::Error),
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
    /// Writes never follow redirects: ureq would turn a 303 into an unauthenticated GET.
    write_agent: ureq::Agent,
    verbose: bool,
    max_sleep_ms: Option<u64>,
    max_pages: u32,
    /// Refuse a response reached through a redirect to another host (secrets-repo content).
    same_host_only: bool,
}

/// The one agent configuration: status codes are data, TLS is validated against the platform
/// roots (rustls, no OpenSSL).
fn build_agent(follow_redirects: bool) -> ureq::Agent {
    let max_redirects = if follow_redirects { 10 } else { 0 };
    ureq::Agent::config_builder()
        .max_redirects(max_redirects)
        .max_redirects_will_error(false)
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
        .into()
}

/// Anonymous GET of a public URL (a deployed site); returns the final status code. No
/// `Authorization` header is ever sent, so no token can reach these hosts. The error is a
/// short description without the URL query or any header.
pub fn probe(url: &str) -> Result<u16, String> {
    build_agent(true)
        .get(url)
        .header("User-Agent", concat!("hoist/", env!("CARGO_PKG_VERSION")))
        .call()
        .map(|r| r.status().as_u16())
        .map_err(|e| e.to_string())
}

const MAX_ATTEMPTS: u32 = 3;
const JSON: &str = "application/json";
const MAX_RETRY_AFTER_SECS: u64 = 60;

impl Http {
    /// Base URL from `HOIST_GITHUB_API`, `HOIST_CLOUDFLARE_API`, `HOIST_GITHUB_UPLOADS`,
    /// `HOIST_NPM_REGISTRY` or `HOIST_CRATES_API`; sleep cap from
    /// `HOIST_MAX_SLEEP_MS`.
    pub fn new(service: Service, token: Secret) -> Http {
        let mut http = Http::with_base(service, token, &service.base_from_env());
        http.max_sleep_ms = std::env::var("HOIST_MAX_SLEEP_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok());
        http
    }

    pub fn with_base(service: Service, token: Secret, base: &str) -> Http {
        Http {
            service,
            token,
            base: base.trim_end_matches('/').to_string(),
            agent: build_agent(true),
            write_agent: build_agent(false),
            verbose: false,
            max_sleep_ms: None,
            max_pages: 1000,
            same_host_only: false,
        }
    }

    /// A client for the same service and base as `other`, with its sleep cap, page cap and
    /// verbosity, but another token (e.g. a freshly minted one).
    pub fn with_base_like(other: &Http, token: Secret) -> Http {
        let mut http = Http::with_base(other.service, token, &other.base);
        http.max_sleep_ms = other.max_sleep_ms;
        http.max_pages = other.max_pages;
        http.verbose = other.verbose;
        http
    }

    /// Sleep `secs` seconds, capped like a rate-limit sleep (`HOIST_MAX_SLEEP_MS`).
    pub fn pause(&self, secs: u64) {
        self.sleep(secs);
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

    /// GETs follow only redirects that keep the base's scheme, host and port (with the token,
    /// e.g. a renamed repo's 301); a redirect anywhere else fails without being requested.
    pub fn same_host_only(mut self) -> Http {
        self.same_host_only = true;
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
            Service::Cloudflare => self.get_all_cloudflare(path),
            _ => self.get_all_github(path, array_key),
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
        self.invalid_for("GET", path)
    }

    fn invalid_for(&self, method: &'static str, path: &str) -> HttpError {
        HttpError::InvalidJson {
            service: self.service,
            method,
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
            let resp = self.send("GET", path, None, JSON)?;
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
                401 => Err(HttpError::Unauthorized {
                    service: self.service,
                }),
                403 => Ok(Err(Fetched::Forbidden(hint(&resp.body, 403)))),
                status => Err(HttpError::Status {
                    service: self.service,
                    path: path.to_string(),
                    status,
                }),
            };
        }
    }

    /// A redirect off the API host; the one refusal a moved or renamed repo explains.
    const ANOTHER_HOST: &'static str = "redirected to another host; refused; if the repo moved or was renamed, update its owner/name";

    /// GET `url` without automatic redirects. Up to 5 redirects are followed, with the token,
    /// only while they keep the base's scheme, host and port; any other is refused unrequested.
    fn get_same_origin(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, SameOrigin> {
        use ureq::http::Uri;
        let bad = SameOrigin::Refused;
        let origin = |u: &Uri| (u.scheme().cloned(), u.authority().cloned());
        let base: Uri = self
            .base
            .parse()
            .map_err(|_| bad("the API base is not a URL"))?;
        let mut url: Uri = url.parse().map_err(|_| bad("not a URL"))?;
        for _ in 0..=5 {
            let started = std::time::Instant::now();
            let resp = self
                .headers(self.write_agent.get(&url.to_string()))
                .call()
                .map_err(SameOrigin::Ureq)?;
            let to = resp.headers().get("location").and_then(|l| l.to_str().ok());
            let (true, Some(to)) = (resp.status().is_redirection(), to) else {
                return Ok(resp);
            };
            if self.verbose {
                // Each hop by its path: a query (`?ref=`) never reaches the log.
                let ms = started.elapsed().as_millis();
                let status = resp.status().as_u16().to_string();
                eprintln!("{}", log_line("GET", url.path(), &status, ms));
            }
            // A browser reads `/\host` as `//host`: another host, refused as one.
            if to.starts_with("/\\") {
                return Err(bad(Self::ANOTHER_HOST));
            }
            let to = match to {
                // `//host/...` is a network-path reference: another host, scheme kept.
                _ if to.starts_with("//") => {
                    format!("{}:{to}", base.scheme_str().unwrap_or("https"))
                }
                _ if to.starts_with('/') => format!(
                    "{}://{}{to}",
                    base.scheme_str().unwrap_or("https"),
                    url.authority().map_or("", |a| a.as_str())
                ),
                _ => to.to_string(),
            };
            url = to
                .parse()
                .map_err(|_| bad("redirected to an invalid URL; refused"))?;
            if origin(&url) != origin(&base) {
                return Err(bad(Self::ANOTHER_HOST));
            }
        }
        Err(bad("too many redirects; refused"))
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

    /// One write. 2xx is `Ok`; 401 is `Unauthorized`; any other failure is `Rejected`.
    /// `Put`, `Patch` and `Delete` retry on a rate limit (max 3 attempts); `Post` never does.
    pub fn send_json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Written, HttpError> {
        let payload = body.map(|b| serde_json::to_vec(b).expect("a JSON value serializes"));
        self.write(method, path, payload.as_deref(), JSON)
    }

    /// One write with a raw body (a release asset), sent as `content_type`. Same rules as
    /// `send_json`: no redirects, `Post` never retried; the body never appears in errors or logs.
    pub fn send_bytes(
        &self,
        method: Method,
        path: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<Written, HttpError> {
        self.write(method, path, Some(bytes), content_type)
    }

    fn write(
        &self,
        method: Method,
        path: &str,
        payload: Option<&[u8]>,
        content_type: &str,
    ) -> Result<Written, HttpError> {
        let mut attempt = 1;
        loop {
            let resp = self.send(method.as_str(), path, payload, content_type)?;
            let limited = resp.status == 429
                || (resp.status == 403 && (resp.retry_after.is_some() || resp.rate_remaining_zero));
            if limited && method.retries() && attempt < MAX_ATTEMPTS {
                self.sleep(resp.retry_after.unwrap_or(1));
                attempt += 1;
                continue;
            }
            return match resp.status {
                200..=299 => {
                    let body = if resp.body.iter().all(u8::is_ascii_whitespace) {
                        None
                    } else {
                        Some(
                            serde_json::from_slice(&resp.body)
                                .map_err(|_| self.invalid_for(method.as_str(), path))?,
                        )
                    };
                    Ok(Written {
                        status: resp.status,
                        body: if resp.status == 204 { None } else { body },
                    })
                }
                401 => Err(HttpError::Unauthorized {
                    service: self.service,
                }),
                status => Err(HttpError::Rejected {
                    service: self.service,
                    method,
                    path: path.to_string(),
                    status,
                    message: hint(&resp.body, status),
                }),
            };
        }
    }

    fn headers<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let github = matches!(self.service, Service::Github | Service::GithubUploads);
        let accept = if github {
            "application/vnd.github+json"
        } else {
            "application/json"
        };
        let agent = match self.service {
            Service::Crates => CRATES_USER_AGENT,
            _ => concat!("hoist/", env!("CARGO_PKG_VERSION")),
        };
        let mut request = request.header("Accept", accept).header("User-Agent", agent);
        // An empty token means an anonymous request (registry reads).
        if !self.token.expose().is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.token.expose()));
        }
        if github {
            request = request.header("X-GitHub-Api-Version", "2022-11-28");
        }
        request
    }

    fn send(
        &self,
        method: &'static str,
        path: &str,
        body: Option<&[u8]>,
        content_type: &str,
    ) -> Result<Response, HttpError> {
        let url = format!("{}{path}", self.base);
        let started = Instant::now();
        let json = |r: ureq::RequestBuilder<ureq::typestate::WithBody>| {
            let r = r.header("Content-Type", content_type);
            match body {
                Some(bytes) => r.send(bytes),
                None => r.send_empty(),
            }
        };
        let result = match method {
            "POST" => json(self.headers(self.write_agent.post(&url))),
            "PUT" => json(self.headers(self.write_agent.put(&url))),
            "PATCH" => json(self.headers(self.write_agent.patch(&url))),
            "DELETE" => {
                let r = self.headers(self.write_agent.delete(&url));
                match body {
                    Some(_) => json(r.force_send_body()),
                    None => r.call(),
                }
            }
            _ if self.same_host_only => match self.get_same_origin(&url) {
                Ok(resp) => Ok(resp),
                Err(SameOrigin::Ureq(e)) => Err(e),
                Err(SameOrigin::Refused(detail)) => {
                    let (service, path) = (self.service, path.to_string());
                    return Err(HttpError::Refused {
                        service,
                        path,
                        detail,
                    });
                }
            },
            _ => self.headers(self.agent.get(&url)).call(),
        };
        let transport = |detail: String| HttpError::Transport {
            service: self.service,
            method,
            path: path.to_string(),
            detail,
        };
        let mut response = match result {
            Ok(response) => response,
            Err(err) => {
                if self.verbose {
                    eprintln!(
                        "{}",
                        log_line(method, path, "error", started.elapsed().as_millis())
                    );
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
                log_line(
                    method,
                    path,
                    &status.to_string(),
                    started.elapsed().as_millis()
                )
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

fn log_line(method: &str, path: &str, outcome: &str, ms: u128) -> String {
    format!("{method} {} {outcome} {ms}ms", bare(path))
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
                    .and_then(|e| e.get("message").or_else(|| e.get("detail")))
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
                method: "GET",
                path: path(),
                detail: "refused".into(),
            },
            HttpError::InvalidJson {
                service,
                method: "GET",
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
        assert_eq!(log_line("GET", &path(), "200", 12), "GET /x 200 12ms");
        assert_eq!(log_line("PUT", &path(), "error", 12), "PUT /x error 12ms");
        let rejected = HttpError::Rejected {
            service,
            method: Method::Put,
            path: path(),
            status: 422,
            message: "Validation Failed".into(),
        };
        let text = rejected.to_string();
        assert!(!text.contains("SECRETCURSOR"), "{text}");
        assert_eq!(text, "GitHub rejected PUT /x (422): Validation Failed");
        let unauthorized = HttpError::Unauthorized { service };
        assert!(!unauthorized.to_string().contains("SECRETCURSOR"));
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

#[cfg(test)]
mod oidc_service_tests {
    use super::*;

    #[test]
    #[should_panic(expected = "Http::with_base")]
    fn actions_oidc_needs_an_explicit_base() {
        let _ = Http::new(Service::ActionsOidc, Secret::from(""));
    }
}
