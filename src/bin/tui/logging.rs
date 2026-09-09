use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

fn create_log(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).with_context(|| {
        format!(
            "could not create log {}; choose a new filename (existing files are not overwritten)",
            path.display()
        )
    })
}

pub(super) fn init(path: &Path) -> anyhow::Result<()> {
    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(value).context("invalid RUST_LOG filter")?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new(
            "warn,rsnomadnet_core=debug,rns_runtime::link_manager=debug,rns_runtime::link_session=debug",
        ),
        Err(error) => return Err(error.into()),
    };
    let file = create_log(path)?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init()
        .map_err(|error| anyhow::anyhow!("could not initialize file logging: {error}"))?;
    tracing::info!(target: "rsnomadnet_core", "TUI diagnostic logging started");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_records_are_written_to_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tui.log");
        let file = create_log(&path).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .finish();
        tracing::subscriber::with_default(subscriber, || tracing::warn!("diagnostic test"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("diagnostic test"));
        assert!(!text.contains('\u{1b}'));
        assert!(create_log(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_log_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("missing");
        let path = temp.path().join("tui.log");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(create_log(&path).is_err());
        assert!(!target.exists());
    }
}
