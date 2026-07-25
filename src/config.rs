use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Local HTTP listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub listen: SocketAddr,

    /// Permit binding the unauthenticated web interface outside loopback.
    #[arg(long)]
    pub allow_remote: bool,

    /// Run without starting rsReticulum.
    #[arg(long)]
    pub offline: bool,

    /// rsReticulum configuration directory.
    #[arg(long)]
    pub rns_config: Option<PathBuf>,

    /// Application data directory.
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub listen: SocketAddr,
    pub offline: bool,
    pub rns_config: Option<PathBuf>,
    pub database_path: PathBuf,
    pub identity_path: PathBuf,
}

impl AppConfig {
    pub fn from_cli(cli: Cli) -> anyhow::Result<Self> {
        if !cli.listen.ip().is_loopback() && !cli.allow_remote {
            bail!("non-loopback listen address requires --allow-remote");
        }

        let state_dir = match cli.state_dir {
            Some(path) => path,
            None => default_state_dir()?,
        };
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("could not create {}", state_dir.display()))?;

        Ok(Self {
            listen: cli.listen,
            offline: cli.offline,
            rns_config: cli.rns_config,
            database_path: state_dir.join("nomadnet.db"),
            identity_path: state_dir.join("identity"),
        })
    }
}

fn default_state_dir() -> anyhow::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".rsNomadNet"));
    }
    if cfg!(windows) {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(profile).join(".rsNomadNet"));
        }
    }
    Ok(std::env::current_dir()
        .context("could not resolve current directory")?
        .join(".rsNomadNet"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn remote_bind_requires_explicit_permission() {
        let cli = Cli {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080),
            allow_remote: false,
            offline: true,
            rns_config: None,
            state_dir: Some(std::env::temp_dir().join("rsnomadnet-config-test")),
        };
        assert!(AppConfig::from_cli(cli).is_err());
    }
}
