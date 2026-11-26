//! Repository addressing on a remote: `organization/name` (ADR-0035).
//!
//! The organization is a pure namespace, not an account: sekai has no
//! user database, so both parts of an address are always explicit. Each
//! part is one validated name component, the shape a hosted repository
//! directory takes under `<root>/<organization>/<name>/`.

use alloc::string::String;
use core::{fmt, str::FromStr};

use crate::error::{RepoAddressError, RepoNameError};
use crate::name;

/// Validated single component of a repository address: an organization
/// or a repository name.
///
/// Names are 1-255 bytes of ASCII alphanumerics plus `.`, `_`, `-`:
/// one path component, so no `/` separator and no `.` / `..` traversal.
/// Ordering and uniqueness are byte-wise and case-sensitive, so `Main`
/// and `main` are distinct names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoName(String);

impl RepoName {
    /// Longest accepted name, in bytes (names are ASCII-only).
    pub const MAX_LEN: usize = name::MAX_LEN;

    /// Validate `raw` into one address component.
    ///
    /// The shared grammar check runs first, so an over-long name reports
    /// [`RepoNameError::TooLong`] even when it also holds a non-ASCII
    /// character. `/` passes that check (ref and scope names carry it)
    /// but never a single component.
    pub fn parse(raw: &str) -> Result<Self, RepoNameError> {
        name::check(raw).map_err(RepoNameError::from)?;
        if raw.contains('/') {
            return Err(RepoNameError::BadChar('/'));
        }
        if matches!(raw, "." | "..") {
            return Err(RepoNameError::Traversal);
        }
        Ok(Self(String::from(raw)))
    }

    /// Borrow the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepoName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RepoName {
    type Err = RepoNameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// A repository address on a remote: `organization/name`, both parts
/// required. There is no default organization - sekai has no account
/// model to derive one from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoAddress {
    organization: RepoName,
    name: RepoName,
}

impl RepoAddress {
    /// Address from already-validated parts.
    pub const fn new(organization: RepoName, name: RepoName) -> Self {
        Self { organization, name }
    }

    /// Validate `raw` as `organization/name`. Exactly one `/` separates
    /// the parts; a second one lands in the name and fails its grammar.
    pub fn parse(raw: &str) -> Result<Self, RepoAddressError> {
        let (organization, name) = raw
            .split_once('/')
            .ok_or(RepoAddressError::MissingSeparator)?;
        Ok(Self {
            organization: RepoName::parse(organization).map_err(RepoAddressError::Organization)?,
            name: RepoName::parse(name).map_err(RepoAddressError::Name)?,
        })
    }

    /// The namespace the repository lives in.
    pub const fn organization(&self) -> &RepoName {
        &self.organization
    }

    /// The repository name within its organization.
    pub const fn name(&self) -> &RepoName {
        &self.name
    }
}

impl fmt::Display for RepoAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.organization, self.name)
    }
}

impl FromStr for RepoAddress {
    type Err = RepoAddressError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn parses_single_components() {
        for raw in ["my-org", "my_project", "repo.v2", "A1", "a"] {
            assert_eq!(RepoName::parse(raw).unwrap().as_str(), raw);
        }
        assert!(RepoName::parse(&"a".repeat(RepoName::MAX_LEN)).is_ok());
    }

    #[test]
    fn refuses_names_outside_the_grammar() {
        assert_eq!(RepoName::parse(""), Err(RepoNameError::Empty));
        assert_eq!(
            RepoName::parse(&"a".repeat(RepoName::MAX_LEN + 1)),
            Err(RepoNameError::TooLong(RepoName::MAX_LEN))
        );
        assert_eq!(RepoName::parse("a b"), Err(RepoNameError::BadChar(' ')));
        assert_eq!(RepoName::parse("é"), Err(RepoNameError::BadChar('é')));
        assert_eq!(RepoName::parse("a/b"), Err(RepoNameError::BadChar('/')));
        assert_eq!(RepoName::parse("."), Err(RepoNameError::Traversal));
        assert_eq!(RepoName::parse(".."), Err(RepoNameError::Traversal));
        // Only the exact traversal spellings fail; dots inside a name are fine.
        assert!(RepoName::parse("...").is_ok());
        assert!(RepoName::parse("a..b").is_ok());
    }

    #[test]
    fn parses_addresses_with_both_parts() {
        let address = RepoAddress::parse("my-org/my-project").unwrap();
        assert_eq!(address.organization().as_str(), "my-org");
        assert_eq!(address.name().as_str(), "my-project");
        assert_eq!(address.to_string(), "my-org/my-project");
        assert_eq!(
            address,
            RepoAddress::new(
                RepoName::parse("my-org").unwrap(),
                RepoName::parse("my-project").unwrap()
            )
        );
    }

    #[test]
    fn refuses_addresses_without_two_valid_parts() {
        assert_eq!(
            RepoAddress::parse("my-project"),
            Err(RepoAddressError::MissingSeparator)
        );
        assert_eq!(
            RepoAddress::parse("/name"),
            Err(RepoAddressError::Organization(RepoNameError::Empty))
        );
        assert_eq!(
            RepoAddress::parse("org/"),
            Err(RepoAddressError::Name(RepoNameError::Empty))
        );
        assert_eq!(
            RepoAddress::parse("a/b/c"),
            Err(RepoAddressError::Name(RepoNameError::BadChar('/')))
        );
        assert_eq!(
            RepoAddress::parse("é/x"),
            Err(RepoAddressError::Organization(RepoNameError::BadChar('é')))
        );
        assert_eq!(
            RepoAddress::parse("org/.."),
            Err(RepoAddressError::Name(RepoNameError::Traversal))
        );
    }
}
