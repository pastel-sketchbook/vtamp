//! A private, immutable server bundle supplies Now Playing's application identity.
//! No installer, Dock window, global icon cache reset, or signing certificate needed.
use crate::platform::{Paths, private_dir};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use objc2_foundation::NSURL;
use std::{
    collections::hash_map::DefaultHasher,
    ffi::{CString, c_void},
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

const PLIST: &str = include_str!(concat!(env!("OUT_DIR"), "/Info.plist"));
const ICON: &[u8] = include_bytes!("../../assets/vtamp.icns");

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSRegisterURL(url: *const c_void, update: u8) -> i32;
}

fn register(bundle: &Path) -> Result<()> {
    let path = CString::new(bundle.as_os_str().as_bytes())?;
    // NSURL and CFURL are toll-free bridged. Keep the retained NSURL alive for
    // the synchronous public Launch Services call; no global cache reset needed.
    let status = unsafe {
        let url = NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
            std::ptr::NonNull::new(path.as_ptr().cast_mut()).unwrap(),
            true,
            None,
        );
        LSRegisterURL((&*url as *const NSURL).cast(), 1)
    };
    if status != 0 {
        bail!("Launch Services registration failed ({status})");
    }
    Ok(())
}

fn is_bundled(executable: &Path) -> bool {
    let Some(contents) = executable.parent().and_then(Path::parent) else {
        return false;
    };
    contents.file_name().is_some_and(|name| name == "Contents")
        && contents
            .parent()
            .is_some_and(|path| path.extension().is_some_and(|ext| ext == "app"))
        && fs::read(contents.join("Info.plist")).is_ok_and(|data| data == PLIST.as_bytes())
        && fs::read(contents.join("Resources/vtamp.icns")).is_ok_and(|data| data == ICON)
}

pub(super) fn enter() -> Result<()> {
    let source = std::env::current_exe()?;
    if is_bundled(&source) {
        return register(source.ancestors().nth(3).unwrap());
    }
    let root = Paths::discover()?.data.join("macos");
    let executable = prepare(&source, &root, |bundle| {
        // The linker signature identifies an ordinary CLI by a generated name.
        // MediaRemote needs the same identifier as CFBundleIdentifier to find its icon.
        let output = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--identifier", "com.xrath.vtamp"])
            .arg(bundle)
            .output()
            .context("Cannot run codesign")?;
        if !output.status.success() {
            bail!(
                "codesign: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    })?;
    // Preserve PID, arguments, environment, log descriptors and signal behavior.
    // The daemon still acquires its normal per-instance lock after exec.
    Err(Command::new(executable)
        .args(std::env::args_os().skip(1))
        .exec()
        .into())
}

fn prepare(source: &Path, root: &Path, sign: impl FnOnce(&Path) -> Result<()>) -> Result<PathBuf> {
    private_dir(root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("prepare.lock"))?;
    lock.lock_exclusive()?;
    let binary = fs::read(source).context("Cannot read server executable")?;
    let mut hash = DefaultHasher::new();
    binary.hash(&mut hash);
    PLIST.hash(&mut hash);
    ICON.hash(&mut hash);
    let generation = root.join(format!("{:016x}", hash.finish()));
    let app = generation.join("vtamp.app");
    let executable = app.join("Contents/MacOS/vtamp");
    if executable.is_file() && is_bundled(&executable) {
        return Ok(executable);
    }
    // Publish only a complete, signed bundle. Other starters wait on prepare.lock;
    // old generations remain intact while an older server might still use them.
    let staging = root.join(format!(".prepare-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let bundle = staging.join("vtamp.app");
        let contents = bundle.join("Contents");
        fs::create_dir_all(contents.join("MacOS"))?;
        fs::create_dir_all(contents.join("Resources"))?;
        let target = contents.join("MacOS/vtamp");
        fs::write(&target, &binary)?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
        fs::write(contents.join("Info.plist"), PLIST)?;
        fs::write(contents.join("Resources/vtamp.icns"), ICON)?;
        sign(&bundle)?;
        if generation.exists() {
            // Only an incomplete generation can reach this branch.
            fs::remove_dir_all(&generation)?;
        }
        fs::rename(&staging, &generation)?;
        Ok(executable)
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_are_reused_and_updates_do_not_modify_the_running_generation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("vtamp");
        let root = temp.path().join("macos");
        fs::write(&source, b"first executable").unwrap();
        let first = prepare(&source, &root, |bundle| {
            assert!(is_bundled(&bundle.join("Contents/MacOS/vtamp")));
            Ok(())
        })
        .unwrap();
        let reused = prepare(&source, &root, |_| panic!("must not sign twice")).unwrap();
        assert_eq!(first, reused);
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::write(&source, b"second executable").unwrap();
        let second = prepare(&source, &root, |_| Ok(())).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(first).unwrap(), b"first executable");
        assert_eq!(fs::read(second).unwrap(), b"second executable");
    }

    #[test]
    fn failed_signing_never_publishes_a_bundle_or_leaves_staging_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("vtamp");
        let root = temp.path().join("macos");
        fs::write(&source, b"executable").unwrap();
        assert!(prepare(&source, &root, |_| bail!("signing failed")).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1); // Only prepare.lock.
        assert!(prepare(&source, &root, |_| Ok(())).is_ok());
    }

    #[test]
    fn concurrent_starters_publish_one_complete_bundle() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("vtamp");
        let root = temp.path().join("macos");
        fs::write(&source, b"executable").unwrap();
        let signed = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        prepare(&source, &root, |_| {
                            signed.fetch_add(1, Ordering::Relaxed);
                            Ok(())
                        })
                        .unwrap()
                    })
                })
                .collect();
            let paths: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            assert!(paths.iter().all(|p| p == &paths[0] && is_bundled(p)));
        });
        assert_eq!(signed.load(Ordering::Relaxed), 1);
    }
}
