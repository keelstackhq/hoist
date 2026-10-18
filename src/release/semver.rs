//! `MAJOR.MINOR.PATCH` versions. Pre-release and build metadata are not supported.

use super::commits::Bump;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(text: &str) -> Result<Version, String> {
        if text.contains(['-', '+']) {
            return Err(format!(
                "version `{text}`: pre-release versions are not supported; use MAJOR.MINOR.PATCH"
            ));
        }
        let bad = || format!("version `{text}` is not MAJOR.MINOR.PATCH");
        let parts: Vec<&str> = text.split('.').collect();
        if parts.len() != 3 {
            return Err(bad());
        }
        let mut nums = [0u64; 3];
        for (slot, part) in nums.iter_mut().zip(&parts) {
            let plain = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'));
            if !plain {
                return Err(bad());
            }
            *slot = part.parse().map_err(|_| bad())?;
        }
        Ok(Version {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
        })
    }

    /// The next version for `bump`. Below 1.0.0 a major bump is a minor bump.
    pub fn bump(self, bump: Bump) -> Version {
        let Version {
            major,
            minor,
            patch,
        } = self;
        match bump {
            Bump::None => self,
            Bump::Patch => Version {
                patch: patch + 1,
                ..self
            },
            Bump::Minor => Version {
                minor: minor + 1,
                patch: 0,
                ..self
            },
            Bump::Major if major == 0 => Version {
                minor: minor + 1,
                patch: 0,
                ..self
            },
            Bump::Major => Version {
                major: major + 1,
                minor: 0,
                patch: 0,
            },
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
