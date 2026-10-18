//! Anonymous registry reads: does a package, or one of its versions, exist on npm or
//! crates.io (spec §6, §13). A 404 means absent; any other failure is `Fetch::Unknown`, never
//! an error, so a registry outage cannot fail `plan` or `check`.

use super::{Package, PkgKind};
use crate::error::HoistError;
use crate::github::Fetch;
use crate::http::{Fetched, Http, seg};

/// npm's form of a scoped name: `@scope%2Fname` (the `@` stays, the `/` is encoded).
fn npm_name(name: &str) -> String {
    match name.strip_prefix('@') {
        Some(rest) => format!("@{}", seg(rest)),
        None => seg(name),
    }
}

fn path(p: &Package, version: Option<&str>) -> String {
    let base = match p.kind {
        PkgKind::Npm => format!("/{}", npm_name(&p.name)),
        PkgKind::Crate => format!("/api/v1/crates/{}", seg(&p.name)),
    };
    match version {
        Some(v) => format!("{base}/{}", seg(v)),
        None => base,
    }
}

fn exists(http: &Http, path: &str) -> Result<Fetch<bool>, HoistError> {
    Ok(match http.get_json(path) {
        Ok(Fetched::Ok(_)) => Fetch::Got(true),
        Ok(Fetched::NotFound) => Fetch::Got(false),
        Ok(Fetched::Forbidden(why)) => Fetch::Unknown(format!("registry refused the read: {why}")),
        Err(err) => Fetch::Unknown(err.to_string()),
    })
}

/// Whether the package exists on its registry (`http` is the npm or crates.io client).
pub fn package_exists(http: &Http, p: &Package) -> Result<Fetch<bool>, HoistError> {
    exists(http, &path(p, None))
}

/// Whether version `v` of the package exists on its registry.
pub fn version_exists(http: &Http, p: &Package, v: &str) -> Result<Fetch<bool>, HoistError> {
    exists(http, &path(p, Some(v)))
}
