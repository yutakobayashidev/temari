use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub model: ModelConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub base_url: String,
    pub name: String,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    pub api_key_env: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
            path: path.display().to_string(),
            source,
        })?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(Error::InvalidConfig(format!(
                "unsupported config version {}; expected 1",
                self.version
            )));
        }
        if self.model.name.trim().is_empty() {
            return Err(Error::InvalidConfig("model.name must not be empty".into()));
        }

        self.model.validate_endpoint()?;

        Ok(())
    }
}

impl ModelConfig {
    pub(crate) fn validate_endpoint(&self) -> Result<(), Error> {
        let url = Url::parse(&self.base_url)
            .map_err(|error| Error::InvalidConfig(format!("invalid model.base_url: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::InvalidConfig(
                "model.base_url must use http or https".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::InvalidConfig(
                "model.base_url must not contain credentials".into(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| Error::InvalidConfig("model.base_url must contain a host".into()))?;
        if !self
            .allowed_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
        {
            return Err(Error::InvalidConfig(format!(
                "model host {host:?} is not present in model.allowed_hosts"
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            version: 1,
            model: ModelConfig {
                base_url: "http://127.0.0.1:11434/v1".into(),
                name: "local".into(),
                allowed_hosts: vec!["127.0.0.1".into()],
                api_key_env: None,
            },
        }
    }

    #[test]
    fn accepts_explicitly_allowed_model_host() {
        config().validate().unwrap();
    }

    #[test]
    fn rejects_unapproved_model_host() {
        let mut config = config();
        config.model.base_url = "https://api.example.com/v1".into();

        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("not present")
        );
    }
}
