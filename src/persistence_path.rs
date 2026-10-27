//! Select durable node storage without silently abandoning legacy fabrics.
use anyhow::{Context, bail};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(crate) fn resolve() -> anyhow::Result<PathBuf> {
    select(
        std::env::var_os("VALETUDO_MATTER_PERSISTENCE"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
        &std::env::temp_dir().join("valetudo-matter"),
    )
}

// Pure environment inputs avoid process-global environment mutation in tests.
// Only directory entries are inspected; fabric contents are never read or logged.
fn select(
    explicit: Option<OsString>,
    xdg: Option<OsString>,
    home: Option<OsString>,
    legacy: &Path,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_empty() {
            bail!("VALETUDO_MATTER_PERSISTENCE must not be empty");
        }
        return Ok(PathBuf::from(path));
    }
    let base = match xdg.filter(|p| !p.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => match home.filter(|p| !p.is_empty()) {
            Some(path) => PathBuf::from(path).join(".local/state"),
            None => bail!(
                "Set VALETUDO_MATTER_PERSISTENCE to a durable directory: neither XDG_STATE_HOME nor HOME is available"
            ),
        },
    };
    if !base.is_absolute() {
        bail!(
            "The default state home must be absolute; set VALETUDO_MATTER_PERSISTENCE to a durable directory"
        );
    }
    let durable = base.join("valetudo-matter");
    if durable == legacy {
        bail!(
            "The default state directory resolves to legacy temporary storage; set VALETUDO_MATTER_PERSISTENCE explicitly to a durable directory"
        );
    }
    match std::fs::read_dir(legacy) {
        Ok(mut entries) => {
            if let Some(entry) = entries.next() {
                entry.context("Cannot inspect legacy Matter storage")?;
                bail!(
                    "Legacy Matter state exists at {}. Refusing to select {} and abandon existing fabrics. Stop the bridge, preserve/copy the complete legacy directory to durable storage, then set VALETUDO_MATTER_PERSISTENCE to the chosen directory. No files were moved or changed",
                    legacy.display(),
                    durable.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => {
            return Err(error).context(
                "Cannot inspect legacy Matter storage; set VALETUDO_MATTER_PERSISTENCE explicitly",
            );
        }
    }
    Ok(durable)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "matter-path-test-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn value(path: &Path) -> Option<OsString> {
        Some(path.as_os_str().to_owned())
    }
    #[test]
    fn explicit_override_preserves_s5_and_relative_paths_without_inspecting_legacy() {
        for path in ["/mnt/data/valetudo-matter/persistence", "relative-state"] {
            assert_eq!(
                select(
                    Some(path.into()),
                    None,
                    None,
                    Path::new("/nonexistent/legacy")
                )
                .unwrap(),
                PathBuf::from(path)
            );
        }
        assert!(select(Some(OsString::new()), None, None, Path::new("/unused")).is_err());
    }
    #[test]
    fn new_install_uses_absolute_xdg_or_home_and_accepts_existing_durable_state() {
        let scratch = Scratch::new();
        let legacy = scratch.0.join("legacy");
        let state = scratch.0.join("state");
        let home = scratch.0.join("home");
        assert_eq!(
            select(None, value(&state), value(&home), &legacy).unwrap(),
            state.join("valetudo-matter")
        );
        assert_eq!(
            select(None, None, value(&home), &legacy).unwrap(),
            home.join(".local/state/valetudo-matter")
        );
        std::fs::create_dir(&legacy).unwrap();
        let durable = state.join("valetudo-matter");
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(durable.join("synthetic-key"), b"unchanged").unwrap();
        for _ in 0..2 {
            assert_eq!(select(None, value(&state), None, &legacy).unwrap(), durable);
        }
        assert_eq!(
            std::fs::read(durable.join("synthetic-key")).unwrap(),
            b"unchanged"
        );
    }
    #[test]
    fn nonempty_legacy_blocks_fresh_and_conflicting_durable_stores_without_changes() {
        let scratch = Scratch::new();
        let legacy = scratch.0.join("legacy");
        let state = scratch.0.join("state");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("synthetic-key"), b"legacy").unwrap();
        assert!(
            select(None, value(&state), None, &legacy)
                .unwrap_err()
                .to_string()
                .contains("Refusing")
        );
        assert!(!state.exists());
        let durable = state.join("valetudo-matter");
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(durable.join("synthetic-key"), b"durable").unwrap();
        assert!(select(None, value(&state), None, &legacy).is_err());
        assert_eq!(
            select(value(&durable), None, None, &legacy).unwrap(),
            durable
        );
        assert_eq!(
            std::fs::read(legacy.join("synthetic-key")).unwrap(),
            b"legacy"
        );
        assert_eq!(
            std::fs::read(durable.join("synthetic-key")).unwrap(),
            b"durable"
        );
    }
    #[test]
    fn invalid_or_missing_state_home_never_falls_back_to_temporary_storage() {
        let scratch = Scratch::new();
        let legacy = scratch.0.join("valetudo-matter");
        assert!(select(None, None, None, &legacy).is_err());
        assert!(select(None, Some("relative".into()), None, &legacy).is_err());
        assert!(select(None, None, Some("relative".into()), &legacy).is_err());
        assert!(select(None, value(&scratch.0), None, &legacy).is_err());
    }
}
