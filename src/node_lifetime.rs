//! Persistent, best-effort boot count of the Matter application node.
//! This is deliberately not the vacuum MCU's or Linux host's reboot count.
use anyhow::Context;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(crate) struct NodeLifetime {
    pub(crate) reboot_count: u16,
}
impl NodeLifetime {
    pub(crate) fn start(directory: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join("node-boot-count");
        let previous = match fs::File::open(&path) {
            Ok(mut file) => {
                let mut value = String::new();
                Read::by_ref(&mut file)
                    .take(32)
                    .read_to_string(&mut value)?;
                Some(
                    value
                        .trim()
                        .parse::<u16>()
                        .context("Invalid persisted Matter boot count")?,
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        // First observed lifetime is boot zero; upgrades retain a lower-bound count.
        let reboot_count = previous.map(|v| v.saturating_add(1)).unwrap_or(0);
        // Unique temporary files let a power-loss orphan coexist with a valid
        // committed counter. Never discard/reset that counter to recover.
        let temporary = directory.join(format!(
            "node-boot-count.{}.{:016x}.new",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .context("Cannot exclusively create Matter boot count update")?;
        writeln!(file, "{reboot_count}")?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        fs::File::open(directory)?.sync_all()?;
        Ok(Self { reboot_count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "valetudo-node-life-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn restarts_increment_saturate_and_preserve_unrelated_fabrics() {
        let p = directory();
        fs::write(p.join("fabric"), b"unchanged").unwrap();
        assert_eq!(NodeLifetime::start(&p).unwrap().reboot_count, 0);
        assert_eq!(NodeLifetime::start(&p).unwrap().reboot_count, 1);
        fs::write(p.join("node-boot-count"), "65535\n").unwrap();
        assert_eq!(NodeLifetime::start(&p).unwrap().reboot_count, 65535);
        assert_eq!(fs::read(p.join("fabric")).unwrap(), b"unchanged");
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn interrupted_update_does_not_prevent_restart_or_reset_counter() {
        let p = directory();
        fs::write(p.join("node-boot-count"), "41\n").unwrap();
        fs::write(p.join("node-boot-count.new"), "42\n").unwrap();
        fs::write(p.join("node-boot-count.9999.abandoned.new"), "incomplete").unwrap();
        assert_eq!(NodeLifetime::start(&p).unwrap().reboot_count, 42);
        assert_eq!(NodeLifetime::start(&p).unwrap().reboot_count, 43);
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn corrupt_counter_is_not_reset() {
        let p = directory();
        fs::write(p.join("node-boot-count"), "corrupt").unwrap();
        assert!(NodeLifetime::start(&p).is_err());
        assert_eq!(
            fs::read_to_string(p.join("node-boot-count")).unwrap(),
            "corrupt"
        );
        fs::remove_dir_all(p).unwrap();
    }
}
