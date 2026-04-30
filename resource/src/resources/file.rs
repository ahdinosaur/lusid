use std::fmt::{self, Display};

use async_trait::async_trait;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_ctx::{ApplyMode, Context};
use lusid_fs::{self as fs, FsError};
use lusid_operation::{
    Operation,
    operations::file::{FileGroup, FileMode, FileOperation, FilePath, FileSource, FileUser},
};
use lusid_params::{ParseError, ParseParams, StructFields};
use lusid_view::impl_display_render;
use rimu::{Spanned, Value};
use secrecy::ExposeSecret;
use thiserror::Error;

use crate::ResourceType;

#[derive(Debug, Clone)]
pub enum FileParams {
    Sourced {
        source: FilePath,
        path: FilePath,
        mode: Option<FileMode>,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
    Present {
        path: FilePath,
        mode: Option<FileMode>,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
    Absent {
        path: FilePath,
    },
}

impl ParseParams for FileParams {
    fn parse_params(value: Spanned<Value>) -> Result<Self, Spanned<ParseError>> {
        let mut fields = StructFields::new(value)?;
        let state = fields.take_discriminator("state", &["sourced", "present", "absent"])?;
        let out = match state {
            "sourced" => FileParams::Sourced {
                // `source` is a `host-path`; the parser resolves a relative
                // string against the plan's source dir (or accepts a typed
                // `Value::HostPath` from a plan that uses `host_path("./...")`).
                // Either way we lower to a `FilePath` string for the operation
                // layer.
                source: FilePath::new(
                    fields
                        .required_host_path("source")?
                        .to_string_lossy()
                        .into_owned(),
                ),
                path: FilePath::new(fields.required_target_path("path")?),
                mode: fields.optional_u32("mode")?.map(FileMode::new),
                user: fields.optional_string("user")?.map(FileUser::new),
                group: fields.optional_string("group")?.map(FileGroup::new),
            },
            "present" => FileParams::Present {
                path: FilePath::new(fields.required_target_path("path")?),
                mode: fields.optional_u32("mode")?.map(FileMode::new),
                user: fields.optional_string("user")?.map(FileUser::new),
                group: fields.optional_string("group")?.map(FileGroup::new),
            },
            "absent" => FileParams::Absent {
                path: FilePath::new(fields.required_target_path("path")?),
            },
            _ => unreachable!(),
        };
        fields.finish()?;
        Ok(out)
    }
}

impl Display for FileParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileParams::Sourced { source, path, .. } => {
                write!(f, "File::Sourced(source = {source}, path = {path})")
            }
            FileParams::Present { path, .. } => write!(f, "File::Present(path = {path})"),
            FileParams::Absent { path } => write!(f, "File::Absent(path = {path})"),
        }
    }
}

impl_display_render!(FileParams);

#[derive(Debug, Clone)]
pub enum FileResource {
    Sourced {
        source: FilePath,
        path: FilePath,
    },
    /// Contents sourced from a decrypted secret by name; resolved against
    /// [`Context::secrets`] at state/apply time so plaintext never travels
    /// through the resource/change tree. See `@core/secret`.
    Secret {
        name: String,
        path: FilePath,
    },
    Present {
        path: FilePath,
    },
    Absent {
        path: FilePath,
    },
    Mode {
        path: FilePath,
        mode: FileMode,
    },
    User {
        path: FilePath,
        user: FileUser,
    },
    Group {
        path: FilePath,
        group: FileGroup,
    },
}

impl Display for FileResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileResource::Sourced { source, path } => {
                write!(f, "FileSourced({source} -> {path})")
            }
            FileResource::Secret { name, path } => {
                write!(f, "FileSecret(secret = {name} -> {path})")
            }
            FileResource::Present { path } => write!(f, "FilePresent({path})"),
            FileResource::Absent { path } => write!(f, "FileAbsent({path})"),
            FileResource::Mode { path, mode } => write!(f, "FileMode({path}, mode = {mode})"),
            FileResource::User { path, user } => write!(f, "FileUser({path}, user = {user})"),
            FileResource::Group { path, group } => write!(f, "FileGroup({path}, group = {group})"),
        }
    }
}

impl_display_render!(FileResource);

/// Probed state of a [`FileResource`] atom.
///
/// `Sourced` is the unified "matches desired" terminal for both
/// `FileResource::Sourced` and `FileResource::Secret`. The two `NotSourced*`
/// variants split on the apply-mode-derived intent: in
/// [`ApplyMode::Local`](lusid_ctx::ApplyMode::Local) we want a symlink at
/// `path` pointing at `source`; elsewhere we want a byte-for-byte copy. The
/// state probe encodes that intent so [`File::change`] stays a pure
/// dispatch.
#[derive(Debug, Clone)]
pub enum FileState {
    Sourced,
    NotSourcedAsCopy,
    NotSourcedAsSymlink,
    Present,
    Absent,
    ModeCorrect,
    ModeIncorrect,
    UserCorrect,
    UserIncorrect,
    GroupCorrect,
    GroupIncorrect,
}

impl Display for FileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use FileState::*;
        let text = match self {
            Sourced => "Sourced",
            NotSourcedAsCopy => "NotSourcedAsCopy",
            NotSourcedAsSymlink => "NotSourcedAsSymlink",
            Present => "Present",
            Absent => "Absent",
            ModeCorrect => "ModeCorrect",
            ModeIncorrect => "ModeIncorrect",
            UserCorrect => "UserCorrect",
            UserIncorrect => "UserIncorrect",
            GroupCorrect => "GroupCorrect",
            GroupIncorrect => "GroupIncorrect",
        };
        write!(f, "{text}")
    }
}

impl_display_render!(FileState);

#[derive(Error, Debug)]
pub enum FileStateError {
    #[error(transparent)]
    Fs(#[from] FsError),

    /// Fires at state probe time when diffing on-disk contents against a
    /// declared secret. Apply-side twin:
    /// [`FileApplyError::MissingSecret`](lusid_operation::operations::file::FileApplyError::MissingSecret).
    #[error(
        "secret {name:?} referenced by file resource was not found in decrypted secrets bundle"
    )]
    MissingSecret { name: String },
}

#[derive(Debug, Clone)]
pub enum FileChange {
    Write {
        path: FilePath,
        source: FileSource,
    },
    Remove {
        path: FilePath,
    },
    ChangeMode {
        path: FilePath,
        mode: FileMode,
    },
    ChangeOwner {
        path: FilePath,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
}

impl Display for FileChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileChange::Write { path, source } => match source {
                FileSource::Contents(contents) => write!(
                    f,
                    "File::Write(path = {}, source = Contents({} bytes))",
                    path,
                    contents.len()
                ),
                FileSource::Path(source_path) => write!(
                    f,
                    "File::Write(path = {}, source = Path({}))",
                    path, source_path
                ),
                FileSource::Secret(name) => {
                    write!(f, "File::Write(path = {}, source = Secret({}))", path, name)
                }
                FileSource::Symlink(source_path) => write!(
                    f,
                    "File::Write(path = {}, source = Symlink({}))",
                    path, source_path
                ),
            },
            FileChange::Remove { path } => write!(f, "File::Remove(path = {path})"),
            FileChange::ChangeMode { path, mode } => {
                write!(f, "File::ChangeMode(path = {path}, mode = {mode})")
            }
            FileChange::ChangeOwner { path, user, group } => write!(
                f,
                "File::ChangeOwner(path = {path}, user = {user:?}, group = {group:?})"
            ),
        }
    }
}

impl_display_render!(FileChange);

#[derive(Debug, Clone)]
pub struct File;

#[async_trait]
impl ResourceType for File {
    const ID: &'static str = "file";

    type Params = FileParams;
    type Resource = FileResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        match params {
            FileParams::Sourced {
                source,
                path,
                mode,
                user,
                group,
            } => {
                let mut nodes = vec![CausalityTree::leaf(
                    CausalityMeta::id("file".into()),
                    FileResource::Sourced {
                        source,
                        path: path.clone(),
                    },
                )];

                if let Some(mode) = mode {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Mode {
                            path: path.clone(),
                            mode,
                        },
                    ));
                }

                if let Some(user) = user {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::User {
                            path: path.clone(),
                            user,
                        },
                    ))
                }

                if let Some(group) = group {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Group { path, group },
                    ));
                }

                nodes
            }

            FileParams::Present {
                path,
                mode,
                user,
                group,
            } => {
                let mut nodes = vec![CausalityTree::leaf(
                    CausalityMeta::id("file".into()),
                    FileResource::Present { path: path.clone() },
                )];

                if let Some(mode) = mode {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Mode {
                            path: path.clone(),
                            mode,
                        },
                    ));
                }

                if let Some(user) = user {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::User {
                            path: path.clone(),
                            user,
                        },
                    ));
                }

                if let Some(group) = group {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Group { path, group },
                    ));
                }

                nodes
            }

            FileParams::Absent { path } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                FileResource::Absent { path },
            )],
        }
    }

    type State = FileState;
    type StateError = FileStateError;

    async fn state(
        ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        let state = match resource {
            FileResource::Sourced { source, path } => {
                probe_sourced_state(ctx.apply_mode(), source, path).await?
            }

            FileResource::Secret { name, path } => {
                // Secrets always materialise as bytes — the on-disk shape is
                // a regular file regardless of apply mode (a symlink would
                // defeat the encrypted-at-rest model). Apply mode is
                // intentionally ignored here.
                if !fs::path_exists(path.as_path()).await? {
                    FileState::NotSourcedAsCopy
                } else {
                    // Compare the file's current contents against the
                    // decrypted secret plaintext. A missing secret here
                    // (e.g. typo in the plan's `name` field) surfaces as
                    // `MissingSecret` rather than a silent NotSourcedAsCopy.
                    let secret = ctx
                        .secrets()
                        .get(name)
                        .ok_or_else(|| FileStateError::MissingSecret { name: name.clone() })?;
                    let path_contents = fs::read_file_to_bytes(path.as_path()).await?;
                    if path_contents.as_slice() == secret.expose_secret().as_bytes() {
                        FileState::Sourced
                    } else {
                        FileState::NotSourcedAsCopy
                    }
                }
            }

            FileResource::Present { path } | FileResource::Absent { path } => {
                if fs::path_exists(path.as_path()).await? {
                    FileState::Present
                } else {
                    FileState::Absent
                }
            }

            FileResource::Mode { path, mode } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::ModeIncorrect
                } else {
                    let actual_mode = fs::get_mode(path.as_path()).await?;
                    let actual_mode = actual_mode & 0o7777;
                    if actual_mode == mode.as_u32() {
                        FileState::ModeCorrect
                    } else {
                        FileState::ModeIncorrect
                    }
                }
            }

            FileResource::User { path, user } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::UserIncorrect
                } else {
                    let actual_user = fs::get_owner_user(path.as_path()).await?;
                    let actual_user = actual_user.map(|u| u.name.to_string());
                    if actual_user.as_deref() == Some(user.as_str()) {
                        FileState::UserCorrect
                    } else {
                        FileState::UserIncorrect
                    }
                }
            }

            FileResource::Group { path, group } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::GroupIncorrect
                } else {
                    let actual_group = fs::get_owner_group(path.as_path()).await?;
                    let actual_group = actual_group.map(|g| g.name.to_string());
                    if actual_group.as_deref() == Some(group.as_str()) {
                        FileState::GroupCorrect
                    } else {
                        FileState::GroupIncorrect
                    }
                }
            }
        };

        Ok(state)
    }

    type Change = FileChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match (resource, state) {
            (FileResource::Sourced { source, path }, FileState::NotSourcedAsCopy) => {
                Some(FileChange::Write {
                    path: path.clone(),
                    source: FileSource::Path(source.clone()),
                })
            }

            (FileResource::Sourced { source, path }, FileState::NotSourcedAsSymlink) => {
                Some(FileChange::Write {
                    path: path.clone(),
                    source: FileSource::Symlink(source.clone()),
                })
            }

            (FileResource::Sourced { .. }, FileState::Sourced) => None,

            (FileResource::Secret { name, path }, FileState::NotSourcedAsCopy) => {
                Some(FileChange::Write {
                    path: path.clone(),
                    source: FileSource::Secret(name.clone()),
                })
            }

            (FileResource::Secret { .. }, FileState::Sourced) => None,

            (FileResource::Present { path }, FileState::Absent) => Some(FileChange::Write {
                path: path.clone(),
                source: FileSource::Contents(Vec::new()),
            }),

            (FileResource::Present { .. }, FileState::Present) => None,

            (FileResource::Absent { path }, FileState::Present) => {
                Some(FileChange::Remove { path: path.clone() })
            }

            (FileResource::Absent { .. }, FileState::Absent) => None,

            (FileResource::Mode { path, mode }, FileState::ModeIncorrect) => {
                Some(FileChange::ChangeMode {
                    path: path.clone(),
                    mode: *mode,
                })
            }

            (FileResource::Mode { .. }, FileState::ModeCorrect) => None,

            (FileResource::User { path, user }, FileState::UserIncorrect) => {
                Some(FileChange::ChangeOwner {
                    path: path.clone(),
                    user: Some(user.clone()),
                    group: None,
                })
            }

            (FileResource::User { .. }, FileState::UserCorrect) => None,

            (FileResource::Group { path, group }, FileState::GroupIncorrect) => {
                Some(FileChange::ChangeOwner {
                    path: path.clone(),
                    user: None,
                    group: Some(group.clone()),
                })
            }

            (FileResource::Group { .. }, FileState::GroupCorrect) => None,

            _ => {
                // TODO (mw): Return an error. Which means changing the trait's change method.
                // Or, alternatively, we have separate resources for each case, so there's no
                // possible mismatch.
                panic!("Unexpected case in change method for File resource.")
            }
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        let op = match change {
            FileChange::Write { path, source } => {
                Operation::File(FileOperation::Write { path, source })
            }
            FileChange::Remove { path } => Operation::File(FileOperation::Remove { path }),
            FileChange::ChangeMode { path, mode } => {
                Operation::File(FileOperation::ChangeMode { path, mode })
            }
            FileChange::ChangeOwner { path, user, group } => {
                Operation::File(FileOperation::ChangeOwner { path, user, group })
            }
        };

        vec![CausalityTree::leaf(CausalityMeta::default(), op)]
    }
}

/// Probe `path` for whether it already matches `source` per the apply mode.
///
/// In [`ApplyMode::Local`](lusid_ctx::ApplyMode::Local) the desired shape is a
/// symlink at `path` whose target matches `source` exactly (string-wise; we
/// don't canonicalise, since that would falsely accept a regular file at
/// `path` whose contents happen to match).
///
/// In [`ApplyMode::Guest`](lusid_ctx::ApplyMode::Guest) the desired shape is
/// a regular file at `path` with bytes equal to `source`. The probe reads
/// through any existing symlink — so a previously-symlinked path with
/// matching bytes reports `Sourced` and stays as a symlink. That's deliberate:
/// the operator already has the bytes they declared, swapping the on-disk
/// representation would just be churn.
async fn probe_sourced_state(
    mode: ApplyMode,
    source: &FilePath,
    path: &FilePath,
) -> Result<FileState, FileStateError> {
    match mode {
        // Note(cc): the symlink-target comparison is *lexical* — `target`
        // is whatever `readlink(2)` returned, compared as a `PathBuf`
        // against the source path string. We deliberately don't
        // canonicalise: `source` arrives as the absolute resolved
        // host-path (see `params::ParamType::HostPath` coercion), and any
        // pre-existing symlink that `readlink`s to a different *string*
        // — even one that resolves to the same inode — should re-create.
        // Otherwise the operator can never see drift between a plan
        // declaring `./foo` and an existing link declaring something else.
        ApplyMode::Local => match fs::probe_symlink(path.as_path()).await? {
            fs::SymlinkTarget::Symlink(target) if target == source.as_path() => {
                Ok(FileState::Sourced)
            }
            // Wrong-target symlink, regular file, or missing path — all
            // mean "not the symlink we want here, materialise it".
            _ => Ok(FileState::NotSourcedAsSymlink),
        },
        ApplyMode::Guest => {
            if !fs::path_exists(path.as_path()).await? {
                return Ok(FileState::NotSourcedAsCopy);
            }
            let source_contents = fs::read_file_to_bytes(source.as_path()).await?;
            let path_contents = fs::read_file_to_bytes(path.as_path()).await?;
            if source_contents == path_contents {
                Ok(FileState::Sourced)
            } else {
                Ok(FileState::NotSourcedAsCopy)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn file_path(p: &std::path::Path) -> FilePath {
        FilePath::new(p.to_string_lossy().into_owned())
    }

    /// Local mode + the desired symlink already exists pointing at source.
    #[tokio::test]
    async fn local_correct_symlink_reports_sourced() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        tokio::fs::write(&source, b"x").await.unwrap();
        let target = dir.path().join("link.txt");
        tokio::fs::symlink(&source, &target).await.unwrap();

        let state = probe_sourced_state(ApplyMode::Local, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::Sourced));
    }

    /// Local mode + path is a regular file with the right bytes — still
    /// reports NotSourcedAsSymlink because the desired *shape* is a symlink,
    /// not just matching bytes.
    #[tokio::test]
    async fn local_regular_file_reports_not_sourced_as_symlink() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        tokio::fs::write(&source, b"shared").await.unwrap();
        let target = dir.path().join("regular.txt");
        tokio::fs::write(&target, b"shared").await.unwrap();

        let state = probe_sourced_state(ApplyMode::Local, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::NotSourcedAsSymlink));
    }

    /// Local mode + the symlink at path points somewhere else.
    #[tokio::test]
    async fn local_wrong_symlink_target_reports_not_sourced_as_symlink() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        let other = dir.path().join("other.txt");
        tokio::fs::write(&source, b"x").await.unwrap();
        tokio::fs::write(&other, b"y").await.unwrap();
        let target = dir.path().join("link.txt");
        tokio::fs::symlink(&other, &target).await.unwrap();

        let state = probe_sourced_state(ApplyMode::Local, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::NotSourcedAsSymlink));
    }

    /// Local mode + path doesn't exist.
    #[tokio::test]
    async fn local_missing_path_reports_not_sourced_as_symlink() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        tokio::fs::write(&source, b"x").await.unwrap();
        let target = dir.path().join("link.txt");

        let state = probe_sourced_state(ApplyMode::Local, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::NotSourcedAsSymlink));
    }

    /// Guest mode + matching bytes at path.
    #[tokio::test]
    async fn guest_matching_bytes_reports_sourced() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        tokio::fs::write(&source, b"hello").await.unwrap();
        let target = dir.path().join("dest.txt");
        tokio::fs::write(&target, b"hello").await.unwrap();

        let state = probe_sourced_state(ApplyMode::Guest, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::Sourced));
    }

    /// Guest mode + symlink with matching bytes through the link — still
    /// `Sourced`, because the byte-equality check reads through the symlink.
    /// We deliberately don't churn the on-disk shape.
    #[tokio::test]
    async fn guest_symlink_with_matching_bytes_reports_sourced() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        tokio::fs::write(&source, b"shared").await.unwrap();
        let target = dir.path().join("link.txt");
        tokio::fs::symlink(&source, &target).await.unwrap();

        let state = probe_sourced_state(ApplyMode::Guest, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::Sourced));
    }

    /// Guest mode + path bytes diverge from source.
    #[tokio::test]
    async fn guest_byte_mismatch_reports_not_sourced_as_copy() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        let target = dir.path().join("dest.txt");
        tokio::fs::write(&source, b"new").await.unwrap();
        tokio::fs::write(&target, b"old").await.unwrap();

        let state = probe_sourced_state(ApplyMode::Guest, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::NotSourcedAsCopy));
    }

    /// Guest mode + path doesn't exist.
    #[tokio::test]
    async fn guest_missing_path_reports_not_sourced_as_copy() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src.txt");
        tokio::fs::write(&source, b"x").await.unwrap();
        let target = dir.path().join("dest.txt");

        let state = probe_sourced_state(ApplyMode::Guest, &file_path(&source), &file_path(&target))
            .await
            .unwrap();
        assert!(matches!(state, FileState::NotSourcedAsCopy));
    }

    /// Pin the change-emission table: `(Sourced, NotSourcedAsCopy)` lowers to
    /// a byte-copy `Write`, `(Sourced, NotSourcedAsSymlink)` lowers to a
    /// `Symlink` `Write`. These two are easy to swap by accident — the only
    /// thing differentiating a local-mode and a guest-mode apply for a sourced
    /// file.
    #[test]
    fn change_for_not_sourced_as_copy_writes_path_source() {
        let resource = FileResource::Sourced {
            source: FilePath::new("/host/src.txt"),
            path: FilePath::new("/target/dest.txt"),
        };
        let change = File::change(&resource, &FileState::NotSourcedAsCopy).expect("some change");
        match change {
            FileChange::Write {
                path,
                source: FileSource::Path(s),
            } => {
                assert_eq!(path.as_path(), std::path::Path::new("/target/dest.txt"));
                assert_eq!(s.as_path(), std::path::Path::new("/host/src.txt"));
            }
            other => panic!("expected Write{{Path}}, got {other:?}"),
        }
    }

    #[test]
    fn change_for_not_sourced_as_symlink_writes_symlink_source() {
        let resource = FileResource::Sourced {
            source: FilePath::new("/host/src.txt"),
            path: FilePath::new("/target/dest.txt"),
        };
        let change = File::change(&resource, &FileState::NotSourcedAsSymlink).expect("some change");
        match change {
            FileChange::Write {
                path,
                source: FileSource::Symlink(s),
            } => {
                assert_eq!(path.as_path(), std::path::Path::new("/target/dest.txt"));
                assert_eq!(s.as_path(), std::path::Path::new("/host/src.txt"));
            }
            other => panic!("expected Write{{Symlink}}, got {other:?}"),
        }
    }

    // No `change(Sourced, Sourced) -> None` test: the match arm at file.rs
    // `(FileResource::Sourced { .. }, FileState::Sourced) => None,` is
    // structurally enforced (the catch-all `_ => panic!` covers every other
    // pairing), so a redundant unit test would only exercise enum matching.
}
