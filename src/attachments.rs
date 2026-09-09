//! LXMF attachments are held in memory until accepted, never in the database.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lxmf_core::constants::FIELD_FILE_ATTACHMENTS;
use lxmf_core::message::LxMessage;

pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;
const MAX_PENDING_FILES: usize = 32;
const OFFER_TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOffer {
    pub id: String,
    pub sender: String,
    pub name: String,
    pub size: usize,
    pub verified: bool,
}

struct Pending {
    offer: FileOffer,
    data: Vec<u8>,
    received: Instant,
}

#[derive(Default)]
pub(crate) struct Inbox {
    pending: BTreeMap<String, Pending>,
}

fn safe_name(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or("");
    let name: String = leaf
        .chars()
        .filter(|c| !c.is_control() && *c != ':')
        .take(120)
        .collect();
    let name = name.trim().trim_start_matches('.');
    if name.is_empty() {
        "attachment.bin".into()
    } else {
        name.into()
    }
}

pub(crate) fn read_file(path: &Path) -> anyhow::Result<(String, Vec<u8>)> {
    anyhow::ensure!(std::fs::metadata(path)?.is_file(), "select a regular file");
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "select a regular file");
    anyhow::ensure!(
        metadata.len() <= MAX_FILE_BYTES as u64,
        "file exceeds 16 MiB limit"
    );
    let mut data = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut data)?;
    anyhow::ensure!(data.len() <= MAX_FILE_BYTES, "file exceeds 16 MiB limit");
    Ok((
        safe_name(&path.file_name().unwrap_or_default().to_string_lossy()),
        data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxmf_core::constants::DeliveryMethod;

    fn message(name: &str, data: &[u8]) -> LxMessage {
        let mut message = LxMessage::new([1; 16], [2; 16], "", "File", DeliveryMethod::Direct);
        attach(&mut message, name, data.to_vec()).unwrap();
        message
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::generate())
            .unwrap();
        LxMessage::unpack(&message.pack().unwrap()).unwrap()
    }

    #[test]
    fn native_lxmf_file_is_not_saved_until_acceptance_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("files");
        let mut inbox = Inbox::default();
        let offers = inbox.receive(&message("../../note.txt", b"hello")).unwrap();
        assert_eq!(offers[0].name, "note.txt");
        assert_eq!(offers[0].size, 5);
        assert!(!directory.exists());
        let path = inbox
            .decide(&offers[0].id, true, &directory)
            .unwrap()
            .unwrap();
        assert_eq!(path, directory.join("note.txt"));
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(inbox.offers().is_empty());
        let next = inbox.receive(&message("note.txt", b"another")).unwrap();
        let second = inbox
            .decide(&next[0].id, true, &directory)
            .unwrap()
            .unwrap();
        assert_ne!(path, second);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert_eq!(std::fs::read(&second).unwrap(), b"another");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn refusal_expiry_and_duplicates_do_not_write_files() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("files");
        let mut inbox = Inbox::default();
        let message = message("a.bin", b"data");
        let offers = inbox.receive(&message).unwrap();
        inbox.receive(&message).unwrap();
        assert_eq!(inbox.offers().len(), 1);
        assert!(
            inbox
                .decide(&offers[0].id, false, &directory)
                .unwrap()
                .is_none()
        );
        assert!(inbox.offers().is_empty());
        assert!(!directory.exists());
        inbox.receive(&message).unwrap();
        inbox.pending.values_mut().next().unwrap().received = Instant::now() - OFFER_TTL;
        assert!(inbox.decide(&offers[0].id, true, &directory).is_err());
        assert!(!directory.exists());
    }

    #[test]
    fn selected_file_is_read_directly_and_large_or_non_regular_files_fail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.bin");
        let mut source = std::fs::File::create(&path).unwrap();
        source.write_all(b"payload").unwrap();
        assert_eq!(
            read_file(&path).unwrap(),
            ("data.bin".into(), b"payload".to_vec())
        );
        source.set_len(MAX_FILE_BYTES as u64 + 1).unwrap();
        assert!(read_file(&path).is_err());
        assert!(read_file(temp.path()).is_err());
    }

    #[test]
    fn malformed_and_oversized_attachments_are_rejected_without_pending_offers() {
        let mut inbox = Inbox::default();
        assert!(
            inbox
                .receive(&message("large", &vec![0; MAX_FILE_BYTES + 1]))
                .is_err()
        );
        let mut invalid = message("test", b"ok");
        invalid
            .set_msgpack_field(FIELD_FILE_ATTACHMENTS, vec![0xc0])
            .unwrap();
        assert!(inbox.receive(&invalid).is_err());
        assert!(inbox.offers().is_empty());
        assert_eq!(safe_name("..\\..\\evil\0.txt"), "evil.txt");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_directory_and_does_not_follow_existing_file_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("files");
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &directory).unwrap();
        let mut inbox = Inbox::default();
        let offers = inbox.receive(&message("test", b"hello")).unwrap();
        assert!(inbox.decide(&offers[0].id, true, &directory).is_err());
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        std::os::unix::fs::symlink(temp.path().join("outside"), target.join("test")).unwrap();
        let saved = inbox.decide(&offers[0].id, true, &target).unwrap().unwrap();
        assert_eq!(saved, target.join("1-test"));
        assert!(!temp.path().join("outside").exists());
    }
}

pub(crate) fn attach(message: &mut LxMessage, name: &str, data: Vec<u8>) -> anyhow::Result<()> {
    let value = rmpv::Value::Array(vec![rmpv::Value::Array(vec![
        name.into(),
        rmpv::Value::Binary(data),
    ])]);
    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &value)?;
    message.set_msgpack_field(FIELD_FILE_ATTACHMENTS, encoded)?;
    Ok(())
}

impl Inbox {
    fn expire(&mut self) {
        self.pending.retain(|_, p| p.received.elapsed() < OFFER_TTL);
    }

    pub(crate) fn offers(&mut self) -> Vec<FileOffer> {
        self.expire();
        self.pending.values().map(|p| p.offer.clone()).collect()
    }

    pub(crate) fn receive(&mut self, message: &LxMessage) -> anyhow::Result<Vec<FileOffer>> {
        self.expire();
        let Some(encoded) = message.get_field(FIELD_FILE_ATTACHMENTS) else {
            return Ok(Vec::new());
        };
        anyhow::ensure!(
            encoded.len() <= MAX_PENDING_BYTES,
            "attachments exceed memory limit"
        );
        let value = rmpv::decode::read_value(&mut std::io::Cursor::new(encoded))?;
        let files = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid LXMF file attachments"))?;
        anyhow::ensure!(
            files.len() + self.pending.len() <= MAX_PENDING_FILES,
            "too many pending files"
        );
        let hash = message
            .hash
            .or(message.message_id)
            .ok_or_else(|| anyhow::anyhow!("missing message hash"))?;
        let mut additions = Vec::new();
        let mut total: usize = self.pending.values().map(|p| p.data.len()).sum();
        for (index, file) in files.iter().enumerate() {
            let pair = file
                .as_array()
                .filter(|p| p.len() == 2)
                .ok_or_else(|| anyhow::anyhow!("invalid attachment entry"))?;
            let name = pair[0]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid attachment name"))?;
            let data = pair[1]
                .as_slice()
                .ok_or_else(|| anyhow::anyhow!("invalid attachment data"))?;
            anyhow::ensure!(
                data.len() <= MAX_FILE_BYTES,
                "attachment exceeds 16 MiB limit"
            );
            total += data.len();
            anyhow::ensure!(
                total <= MAX_PENDING_BYTES,
                "pending attachments exceed 64 MiB limit"
            );
            let offer = FileOffer {
                id: format!("{}:{index}", hex::encode(hash)),
                sender: hex::encode(message.source_hash),
                name: safe_name(name),
                size: data.len(),
                verified: message.signature_validated,
            };
            additions.push(Pending {
                offer,
                data: data.to_vec(),
                received: Instant::now(),
            });
        }
        let offers = additions.iter().map(|p| p.offer.clone()).collect();
        for pending in additions {
            self.pending
                .entry(pending.offer.id.clone())
                .or_insert(pending);
        }
        Ok(offers)
    }

    pub(crate) fn decide(
        &mut self,
        id: &str,
        accept: bool,
        directory: &Path,
    ) -> anyhow::Result<Option<PathBuf>> {
        self.expire();
        if !accept {
            self.pending.remove(id);
            return Ok(None);
        }
        let pending = self
            .pending
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("file offer expired or already handled"))?;
        std::fs::create_dir_all(directory)?;
        anyhow::ensure!(
            !std::fs::symlink_metadata(directory)?
                .file_type()
                .is_symlink(),
            "files directory must not be a symbolic link"
        );
        crate::config::restrict_directory_permissions(directory)?;
        for suffix in 0..10_000 {
            let name = if suffix == 0 {
                pending.offer.name.clone()
            } else {
                format!("{suffix}-{}", pending.offer.name)
            };
            let path = directory.join(name);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            };
            if let Err(e) = file.write_all(&pending.data).and_then(|_| file.sync_all()) {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Err(e.into());
            }
            self.pending.remove(id);
            return Ok(Some(path));
        }
        anyhow::bail!("too many files with the same name")
    }
}
