//! Async filesystem helpers used by lusid operations and resources.
//!
//! Every function wraps a `tokio::fs` / `nix` / `filetime` call with a contextual
//! [`FsError`] variant — errors always include the offending path(s), so diagnostics
//! don't require parsing raw `io::Error` messages.
//!
//! Highlights:
//! - [`write_file_atomic`] / [`copy_file_atomic`]: write to a sibling temp file, copy
//!   destination metadata (or source metadata, respectively), then rename. This means
//!   readers never observe a half-written file.
//! - [`change_owner`] / [`change_owner_by_id`]: uid/gid changes, Unix-only.
//! - [`copy_dir`]: iterative async walk that recreates the source tree at the
//!   destination, preserving symlinks verbatim. Portable across Linux and macOS.

use nix::unistd::{Group, User};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use filetime::FileTime;
use thiserror::Error;
use tokio::fs::{self};
use tokio::io::AsyncWriteExt;

#[derive(Error, Debug)]
pub enum FsError {
    #[error("Cannot create directory '{path}': {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot read directory '{path}': {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot iterate directory '{path}': {source}")]
    ReadDirEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot remove directory '{path}': {source}")]
    RemoveDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot read metadata '{path}': {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot change mode '{path}' to {mode}: {source}")]
    ChangeMode {
        path: PathBuf,
        mode: u32,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot set permissions '{path}': {source}")]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot change owner '{path}' to user {uid:?} + group {gid:?}: {source}")]
    ChangeOwner {
        path: PathBuf,
        uid: Option<u32>,
        gid: Option<u32>,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to get user from name: {user}")]
    UserFromName {
        user: String,
        #[source]
        source: nix::Error,
    },

    #[error("Failed to get user from uid: {uid}")]
    UserFromUid {
        uid: u32,
        #[source]
        source: nix::Error,
    },

    #[error("User not found: {user}")]
    UserNotFound { user: String },

    #[error("Failed to get group from name: {group}")]
    GroupFromName {
        group: String,
        #[source]
        source: nix::Error,
    },

    #[error("Failed to get group from gid: {gid}")]
    GroupFromGid {
        gid: u32,
        #[source]
        source: nix::Error,
    },

    #[error("Group not found: {group}")]
    GroupNotFound { group: String },

    #[error("Cannot write directory '{path}' (read-only)")]
    ReadOnlyDir { path: PathBuf },

    #[error("Cannot create file '{path}': {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot open file '{path}': {source}")]
    OpenFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot determine if path exists '{path}': {source}")]
    PathExists {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot write file '{path}': {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot read file '{path}': {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot copy file from '{from}' to '{to}': {source}")]
    CopyFile {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot rename file from '{from}' to '{to}': {source}")]
    RenameFile {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot delete file '{path}': {source}")]
    RemoveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot create symlink from '{from}' to '{to}': {source}")]
    CreateSymlink {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Cannot read symlink '{path}': {source}")]
    ReadSymlink {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to set file times: {source}")]
    SetFileTimes {
        #[source]
        source: std::io::Error,
    },
}

pub async fn create_dir<P: AsRef<Path>>(path: P) -> Result<(), FsError> {
    let p = path.as_ref();
    fs::create_dir_all(p)
        .await
        .map_err(|source| FsError::CreateDir {
            path: p.to_path_buf(),
            source,
        })
}

/// Recursively copy the contents of `from` into `to`, mirroring the tree layout.
///
/// - Regular files are copied via [`tokio::fs::copy`], which preserves permission bits.
/// - Symbolic links are recreated as symlinks pointing at the original target; the
///   target is *not* dereferenced, so dangling links round-trip cleanly.
/// - Missing parent directories in `to` are created as needed.
/// - Ownership and timestamps are not preserved — this matches `cp -R` without `-p`
///   and avoids spurious `EPERM` when running unprivileged.
/// - Special files (block/char devices, sockets, fifos) are silently skipped.
///
/// Iterative rather than recursive so the walk stays cheap and doesn't need boxed
/// futures.
pub async fn copy_dir<F: AsRef<Path>, T: AsRef<Path>>(from: F, to: T) -> Result<(), FsError> {
    let from_root = from.as_ref().to_path_buf();
    let to_root = to.as_ref().to_path_buf();

    create_dir(&to_root).await?;

    // Each entry is a subdirectory (relative to `from_root`) whose contents we still
    // need to process. The empty path represents the root itself.
    let mut pending: Vec<PathBuf> = vec![PathBuf::new()];

    while let Some(rel_dir) = pending.pop() {
        let src_dir = from_root.join(&rel_dir);

        let mut entries = fs::read_dir(&src_dir)
            .await
            .map_err(|source| FsError::ReadDir {
                path: src_dir.clone(),
                source,
            })?;

        while let Some(entry) =
            entries
                .next_entry()
                .await
                .map_err(|source| FsError::ReadDirEntry {
                    path: src_dir.clone(),
                    source,
                })?
        {
            let src_path = entry.path();
            let rel_child = rel_dir.join(entry.file_name());
            let dest_path = to_root.join(&rel_child);

            let file_type = entry
                .file_type()
                .await
                .map_err(|source| FsError::Metadata {
                    path: src_path.clone(),
                    source,
                })?;

            if file_type.is_symlink() {
                let target =
                    fs::read_link(&src_path)
                        .await
                        .map_err(|source| FsError::ReadSymlink {
                            path: src_path.clone(),
                            source,
                        })?;
                fs::symlink(&target, &dest_path).await.map_err(|source| {
                    FsError::CreateSymlink {
                        from: target,
                        to: dest_path,
                        source,
                    }
                })?;
            } else if file_type.is_dir() {
                create_dir(&dest_path).await?;
                pending.push(rel_child);
            } else if file_type.is_file() {
                fs::copy(&src_path, &dest_path)
                    .await
                    .map_err(|source| FsError::CopyFile {
                        from: src_path,
                        to: dest_path,
                        source,
                    })?;
            }
        }
    }

    Ok(())
}

pub async fn read_dir<P: AsRef<Path>>(path: P) -> Result<Vec<PathBuf>, FsError> {
    let p = path.as_ref();
    let mut dir = fs::read_dir(p).await.map_err(|source| FsError::ReadDir {
        path: p.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    while let Some(entry) = dir
        .next_entry()
        .await
        .map_err(|source| FsError::ReadDirEntry {
            path: p.to_path_buf(),
            source,
        })?
    {
        entries.push(entry.path());
    }
    Ok(entries)
}

pub async fn remove_dir<P: AsRef<Path>>(path: P) -> Result<(), FsError> {
    let p = path.as_ref();
    fs::remove_dir_all(p)
        .await
        .map_err(|source| FsError::RemoveDir {
            path: p.to_path_buf(),
            source,
        })
}

pub async fn setup_directory_access<P: AsRef<Path>>(path: P) -> Result<(), FsError> {
    let p = path.as_ref();
    create_dir(p).await?;
    let permission = fs::metadata(p)
        .await
        .map_err(|source| FsError::Metadata {
            path: p.to_path_buf(),
            source,
        })?
        .permissions();
    if permission.readonly() {
        return Err(FsError::ReadOnlyDir {
            path: p.to_path_buf(),
        });
    }
    Ok(())
}

pub async fn get_mode<P: AsRef<Path>>(path: P) -> Result<u32, FsError> {
    let p = path.as_ref();
    let metadata = fs::metadata(p).await.map_err(|source| FsError::Metadata {
        path: p.to_path_buf(),
        source,
    })?;
    Ok(metadata.permissions().mode())
}

pub async fn change_mode<P: AsRef<Path>>(path: P, mode: u32) -> Result<(), FsError> {
    let p = path.as_ref();
    let mut permissions = fs::metadata(p)
        .await
        .map_err(|source| FsError::Metadata {
            path: p.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(p, permissions)
        .await
        .map_err(|source| FsError::ChangeMode {
            path: p.to_path_buf(),
            mode,
            source,
        })?;
    Ok(())
}

#[cfg(unix)]
pub async fn change_owner_by_id<P: AsRef<Path>>(
    path: P,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), FsError> {
    let p = path.as_ref();

    let std_file = &std::fs::File::open(p).map_err(|source| FsError::OpenFile {
        path: p.to_path_buf(),
        source,
    })?;

    std::os::unix::fs::fchown(std_file, uid, gid).map_err(|source| FsError::ChangeOwner {
        path: p.to_path_buf(),
        uid,
        gid,
        source,
    })
}

#[cfg(unix)]
pub async fn change_owner<P: AsRef<Path>>(
    path: P,
    user: Option<&str>,
    group: Option<&str>,
) -> Result<(), FsError> {
    let uid = match user {
        Some(user) => Some(
            User::from_name(user)
                .map_err(|source| FsError::UserFromName {
                    user: user.into(),
                    source,
                })?
                .ok_or_else(|| FsError::UserNotFound { user: user.into() })?
                .uid
                .as_raw(),
        ),
        None => None,
    };

    let gid = match group {
        Some(group) => Some(
            Group::from_name(group)
                .map_err(|source| FsError::GroupFromName {
                    group: group.into(),
                    source,
                })?
                .ok_or_else(|| FsError::GroupNotFound {
                    group: group.into(),
                })?
                .gid
                .as_raw(),
        ),
        None => None,
    };

    change_owner_by_id(path, uid, gid).await
}

pub async fn get_owner_user<P: AsRef<Path>>(path: P) -> Result<Option<User>, FsError> {
    let p = path.as_ref();
    let metadata = fs::metadata(p).await.map_err(|source| FsError::Metadata {
        path: p.to_path_buf(),
        source,
    })?;
    let uid = metadata.uid();
    User::from_uid(uid.into()).map_err(|source| FsError::UserFromUid { uid, source })
}

pub async fn get_owner_group<P: AsRef<Path>>(path: P) -> Result<Option<Group>, FsError> {
    let p = path.as_ref();
    let metadata = fs::metadata(p).await.map_err(|source| FsError::Metadata {
        path: p.to_path_buf(),
        source,
    })?;
    let gid = metadata.gid();
    Group::from_gid(gid.into()).map_err(|source| FsError::GroupFromGid { gid, source })
}

pub async fn create_file<P: AsRef<Path>>(path: P) -> Result<tokio::fs::File, FsError> {
    let p = path.as_ref();
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(p)
        .await
        .map_err(|source| FsError::CreateFile {
            path: p.to_path_buf(),
            source,
        })
}

pub async fn open_file<P: AsRef<Path>>(path: P) -> Result<tokio::fs::File, FsError> {
    let p = path.as_ref();
    fs::File::open(p).await.map_err(|source| FsError::OpenFile {
        path: p.to_path_buf(),
        source,
    })
}

pub async fn path_exists<P: AsRef<Path>>(path: P) -> Result<bool, FsError> {
    let p = path.as_ref();
    fs::try_exists(p)
        .await
        .map_err(|source| FsError::PathExists {
            path: p.to_path_buf(),
            source,
        })
}

pub async fn write_file<P: AsRef<Path>>(path: P, data: &[u8]) -> Result<(), FsError> {
    let p = path.as_ref();
    let mut file = create_file(p).await?;
    file.write_all(data)
        .await
        .map_err(|source| FsError::WriteFile {
            path: p.to_path_buf(),
            source,
        })?;
    file.flush().await.map_err(|source| FsError::WriteFile {
        path: p.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Atomically write `data` to `path`.
///
/// Strategy: write to a sibling temp file, copy existing destination metadata onto the
/// temp file (permissions, owner, times — so users don't see a file whose mode changed
/// when it shouldn't have), then rename. Readers never observe a partial write.
pub async fn write_file_atomic<P: AsRef<Path>>(path: P, data: &[u8]) -> Result<(), FsError> {
    let dest_path = path.as_ref();
    let temp_path = temporary_path_for(dest_path);

    // Write file contents to temporary path in same directory as destination path.
    write_file(&temp_path, data).await?;

    if path_exists(dest_path).await? {
        // Copy metadata from destination path.
        copy_metadata(dest_path, &temp_path).await?;
    }

    // Rename temporary path to destination path.
    rename_file(&temp_path, dest_path).await?;

    Ok(())
}

pub async fn copy_metadata<Src: AsRef<Path>, Dest: AsRef<Path>>(
    src: Src,
    dest: Dest,
) -> Result<(), FsError> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    let src_metadata = fs::metadata(src)
        .await
        .map_err(|source| FsError::Metadata {
            path: src.to_path_buf(),
            source,
        })?;

    // Copy permissions.
    fs::set_permissions(dest, src_metadata.permissions())
        .await
        .map_err(|source| FsError::SetPermissions {
            path: dest.to_path_buf(),
            source,
        })?;

    // Copy ownership
    change_owner_by_id(dest, Some(src_metadata.uid()), Some(src_metadata.gid())).await?;

    // Copy file times
    let atime = FileTime::from_last_access_time(&src_metadata);
    let mtime = FileTime::from_last_modification_time(&src_metadata);
    let std_file = &std::fs::File::open(dest).map_err(|source| FsError::OpenFile {
        path: dest.to_path_buf(),
        source,
    })?;
    filetime::set_file_handle_times(std_file, Some(atime), Some(mtime))
        .map_err(|source| FsError::SetFileTimes { source })?;

    Ok(())
}

pub async fn read_file_to_string<P: AsRef<Path>>(path: P) -> Result<String, FsError> {
    let p = path.as_ref();
    fs::read_to_string(p)
        .await
        .map_err(|source| FsError::ReadFile {
            path: p.to_path_buf(),
            source,
        })
}

pub async fn read_file_to_bytes<P: AsRef<Path>>(path: P) -> Result<Vec<u8>, FsError> {
    let p = path.as_ref();
    fs::read(p).await.map_err(|source| FsError::ReadFile {
        path: p.to_path_buf(),
        source,
    })
}

/// Atomically copy `from` → `to`, preserving the source's permissions/owner/times.
///
/// Unlike [`write_file_atomic`], this copies metadata from the *source* (since the
/// whole point is replicating it). The sibling-temp-then-rename dance is identical.
pub async fn copy_file_atomic<F: AsRef<Path>, T: AsRef<Path>>(
    from: F,
    to: T,
) -> Result<(), FsError> {
    let from_path = from.as_ref();
    let to_path = to.as_ref();

    let temp_path = temporary_path_for(to_path);

    {
        let mut source_file =
            fs::File::open(from_path)
                .await
                .map_err(|source| FsError::OpenFile {
                    path: from_path.to_path_buf(),
                    source,
                })?;
        let mut destination_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_path)
            .await
            .map_err(|source| FsError::OpenFile {
                path: temp_path.to_path_buf(),
                source,
            })?;

        tokio::io::copy(&mut source_file, &mut destination_file)
            .await
            .map_err(|source| FsError::CopyFile {
                from: from_path.to_path_buf(),
                to: temp_path.to_path_buf(),
                source,
            })?;
        destination_file
            .flush()
            .await
            .map_err(|source| FsError::CopyFile {
                from: from_path.to_path_buf(),
                to: temp_path.to_path_buf(),
                source,
            })?;

        // Copy metadata from destination path.
        copy_metadata(from_path, &temp_path).await?;
    }

    rename_file(&temp_path, to_path).await?;

    Ok(())
}

pub async fn rename_file<F: AsRef<Path>, T: AsRef<Path>>(from: F, to: T) -> Result<(), FsError> {
    let from_p = from.as_ref();
    let to_p = to.as_ref();
    fs::rename(from_p, to_p)
        .await
        .map_err(|source| FsError::RenameFile {
            from: from_p.to_path_buf(),
            to: to_p.to_path_buf(),
            source,
        })
}

pub async fn remove_file<P: AsRef<Path>>(path: P) -> Result<(), FsError> {
    let p = path.as_ref();
    fs::remove_file(p)
        .await
        .map_err(|source| FsError::RemoveFile {
            path: p.to_path_buf(),
            source,
        })
}

pub async fn create_symlink<F: AsRef<Path>, T: AsRef<Path>>(from: F, to: T) -> Result<(), FsError> {
    let from_path = from.as_ref();
    let to_path = to.as_ref();

    fs::symlink(from_path, to_path)
        .await
        .map_err(|source| FsError::CreateSymlink {
            from: from_path.to_path_buf(),
            to: to_path.to_path_buf(),
            source,
        })
}

// Sibling temp path: `<path>.<nanos>.tmp`. Nanosecond resolution is effectively
// collision-free for sequential writes to the same target; parallel writers to the
// same target would be racing anyway and the final `rename` is atomic.
fn temporary_path_for(path: &Path) -> PathBuf {
    let time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    path.with_extension(format!("{time}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn copy_dir_mirrors_nested_tree() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("sub")).await.unwrap();
        fs::write(src.join("top.txt"), b"top").await.unwrap();
        fs::write(src.join("sub/nested.txt"), b"nested")
            .await
            .unwrap();

        copy_dir(&src, &dst).await.unwrap();

        assert_eq!(fs::read(dst.join("top.txt")).await.unwrap(), b"top");
        assert_eq!(
            fs::read(dst.join("sub/nested.txt")).await.unwrap(),
            b"nested"
        );
    }

    #[tokio::test]
    async fn copy_dir_preserves_symlinks_verbatim() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir(&src).await.unwrap();
        fs::write(src.join("real.txt"), b"real").await.unwrap();
        // Relative target, and a dangling link — both should round-trip unchanged.
        fs::symlink("real.txt", src.join("link.txt")).await.unwrap();
        fs::symlink("nowhere", src.join("dangling")).await.unwrap();

        copy_dir(&src, &dst).await.unwrap();

        let link_meta = fs::symlink_metadata(dst.join("link.txt")).await.unwrap();
        assert!(link_meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst.join("link.txt")).await.unwrap(),
            PathBuf::from("real.txt")
        );

        let dangling_meta = fs::symlink_metadata(dst.join("dangling")).await.unwrap();
        assert!(dangling_meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst.join("dangling")).await.unwrap(),
            PathBuf::from("nowhere")
        );
    }

    #[tokio::test]
    async fn copy_dir_merges_into_existing_destination() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir(&src).await.unwrap();
        fs::write(src.join("new.txt"), b"new").await.unwrap();

        fs::create_dir(&dst).await.unwrap();
        fs::write(dst.join("existing.txt"), b"existing")
            .await
            .unwrap();

        copy_dir(&src, &dst).await.unwrap();

        assert_eq!(fs::read(dst.join("new.txt")).await.unwrap(), b"new");
        assert_eq!(
            fs::read(dst.join("existing.txt")).await.unwrap(),
            b"existing"
        );
    }
}
