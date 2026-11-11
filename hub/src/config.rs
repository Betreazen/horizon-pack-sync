//! The hub's secrets, from the environment.

const MIN_SERVER_KEY_LENGTH: usize = 32;
const VARIABLES: [&str; 3] = ["PACK_GITHUB_REPO", "PACK_GITHUB_TOKEN", "PACK_SERVER_KEY"];

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum PackConfigError {
    #[error("all PACK_* variables are needed, {0} is not set")]
    Incomplete(&'static str),
    #[error("PACK_SERVER_KEY is too short, it needs {MIN_SERVER_KEY_LENGTH} characters or more")]
    WeakServerKey,
}

/// The private repository, a token that can read it, and the game server's key.
#[derive(Clone, PartialEq, Eq)]
pub struct PackConfig {
    pub github_repo: String,
    pub github_token: String,
    pub server_key: String,
}

/// Written by hand so the token and key cannot reach a log.
impl std::fmt::Debug for PackConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackConfig")
            .field("github_repo", &self.github_repo)
            .finish_non_exhaustive()
    }
}

impl PackConfig {
    /// None when no variable is set, so a site can run without the pack API.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, PackConfigError> {
        let read = |name: &str| lookup(name).filter(|value| !value.is_empty());
        if VARIABLES.iter().all(|name| read(name).is_none()) {
            return Ok(None);
        }
        let required = |name: &'static str| read(name).ok_or(PackConfigError::Incomplete(name));
        let config = Self {
            github_repo: required("PACK_GITHUB_REPO")?,
            github_token: required("PACK_GITHUB_TOKEN")?,
            server_key: required("PACK_SERVER_KEY")?,
        };
        if config.server_key.trim().len() < MIN_SERVER_KEY_LENGTH {
            return Err(PackConfigError::WeakServerKey);
        }
        Ok(Some(config))
    }

    pub fn from_env() -> Result<Option<Self>, PackConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    const KEY: &str = "0123456789012345678901234567890123";

    #[test]
    fn no_variables_means_no_pack_api() {
        assert_eq!(PackConfig::from_lookup(lookup(&[])), Ok(None));
    }

    #[test]
    fn half_a_configuration_is_an_error() {
        let result = PackConfig::from_lookup(lookup(&[("PACK_GITHUB_REPO", "o/r")]));
        assert_eq!(
            result,
            Err(PackConfigError::Incomplete("PACK_GITHUB_TOKEN"))
        );
    }

    #[test]
    fn a_complete_configuration_is_read() {
        let config = PackConfig::from_lookup(lookup(&[
            ("PACK_GITHUB_REPO", "o/r"),
            ("PACK_GITHUB_TOKEN", "t"),
            ("PACK_SERVER_KEY", KEY),
        ]))
        .expect("valid")
        .expect("present");
        assert_eq!(config.github_repo, "o/r");
    }

    #[test]
    fn a_short_server_key_is_refused() {
        let result = PackConfig::from_lookup(lookup(&[
            ("PACK_GITHUB_REPO", "o/r"),
            ("PACK_GITHUB_TOKEN", "t"),
            ("PACK_SERVER_KEY", "   short   "),
        ]));
        assert_eq!(result, Err(PackConfigError::WeakServerKey));
    }

    #[test]
    fn debug_output_hides_the_secrets() {
        let config = PackConfig {
            github_repo: "o/r".into(),
            github_token: "hidden-token".into(),
            server_key: "hidden-key".into(),
        };
        assert!(!format!("{config:?}").contains("hidden"));
    }
}
