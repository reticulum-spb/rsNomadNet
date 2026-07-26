use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Parser;

const MIN_AUTH_TOKEN_BYTES: usize = 32;
const MAX_AUTH_TOKEN_BYTES: usize = 1024;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Local HTTP listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub listen: SocketAddr,

    /// Permit binding the authenticated web interface outside loopback.
    #[arg(long)]
    pub allow_remote: bool,

    /// File containing the Web API bearer token.
    #[arg(long)]
    pub auth_token_file: Option<PathBuf>,

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
    pub auth_token_hash: Option<[u8; 32]>,
}

impl AppConfig {
    pub fn from_cli(cli: Cli) -> anyhow::Result<Self> {
        if !cli.listen.ip().is_loopback() && !cli.allow_remote {
            bail!("non-loopback listen address requires --allow-remote");
        }
        if !cli.listen.ip().is_loopback() && cli.auth_token_file.is_none() {
            bail!("non-loopback listen address requires --auth-token-file");
        }

        let state_dir = match cli.state_dir {
            Some(path) => path,
            None => default_state_dir()?,
        };
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("could not create {}", state_dir.display()))?;
        restrict_directory_permissions(&state_dir)?;

        let auth_token_hash = cli
            .auth_token_file
            .as_deref()
            .map(read_auth_token)
            .transpose()?;

        Ok(Self {
            listen: cli.listen,
            offline: cli.offline,
            rns_config: cli.rns_config,
            database_path: state_dir.join("nomadnet.db"),
            identity_path: state_dir.join("identity"),
            auth_token_hash,
        })
    }
}

fn read_auth_token(path: &std::path::Path) -> anyhow::Result<[u8; 32]> {
    reject_insecure_secret_permissions(path)?;
    let value = std::fs::read_to_string(path)
        .with_context(|| format!("could not read auth token from {}", path.display()))?;
    let token = value.trim();
    anyhow::ensure!(
        (MIN_AUTH_TOKEN_BYTES..=MAX_AUTH_TOKEN_BYTES).contains(&token.len()),
        "auth token must contain between {MIN_AUTH_TOKEN_BYTES} and {MAX_AUTH_TOKEN_BYTES} bytes"
    );
    anyhow::ensure!(
        token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')),
        "auth token may only contain URL-safe ASCII characters"
    );
    Ok(rns_crypto::sha::full_hash(token.as_bytes()))
}

pub fn restrict_file_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("could not protect {}", path.display()))?;
    }
    Ok(())
}

fn restrict_directory_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not protect {}", path.display()))?;
    }
    Ok(())
}

fn reject_insecure_secret_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .with_context(|| format!("could not inspect {}", path.display()))?
            .permissions()
            .mode();
        anyhow::ensure!(
            mode & 0o077 == 0,
            "auth token file {} must not be accessible by group or others",
            path.display()
        );
    }
    Ok(())
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
            auth_token_file: None,
            offline: true,
            rns_config: None,
            state_dir: Some(std::env::temp_dir().join("rsnomadnet-config-test")),
        };
        assert!(AppConfig::from_cli(cli).is_err());
    }

    #[test]
    fn remote_bind_requires_authentication() {
        let cli = Cli {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080),
            allow_remote: true,
            auth_token_file: None,
            offline: true,
            rns_config: None,
            state_dir: Some(std::env::temp_dir().join("rsnomadnet-auth-test")),
        };
        assert!(AppConfig::from_cli(cli).is_err());
    }

    #[test]
    fn remote_bind_accepts_a_protected_token_file() {
        let directory = tempfile::tempdir().unwrap();
        let token_path = directory.path().join("token");
        std::fs::write(&token_path, "0123456789abcdef0123456789abcdef\n").unwrap();
        restrict_file_permissions(&token_path).unwrap();
        let cli = Cli {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080),
            allow_remote: true,
            auth_token_file: Some(token_path),
            offline: true,
            rns_config: None,
            state_dir: Some(directory.path().join("state")),
        };
        assert!(AppConfig::from_cli(cli).unwrap().auth_token_hash.is_some());
    }
}
