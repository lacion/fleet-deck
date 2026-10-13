//! [`ArtifactService`]: the publish and read operations (R12, DL-165).

use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::artifacts::{
    AUTHOR_OWNER, Artifact, ArtifactDetail, ArtifactFile, ArtifactFileQuery, ArtifactGetQuery,
    ArtifactList, ArtifactListQuery, ArtifactPublish, ArtifactPublished, MAX_AUTHOR_CHARS,
    MAX_CHUNK_BYTES, MAX_TITLE_CHARS, code,
};
use fleetdeck_proto::files::MAX_PATH_CHARS;
use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::{FileType, Mode, OFlags};

use super::{ArtifactError, stage};
use crate::manager::policy::{Policy, WorkspaceError};
use crate::store::Store;
use crate::store::artifacts::{CommitError, NewVersion};
use crate::workspace::files::{open_root, private_root};

/// The artifact service of one station.
#[derive(Debug, Clone)]
pub struct ArtifactService {
    store: Store,
    policy: Arc<Policy>,
    /// `<station data>/artifacts`.
    root: PathBuf,
}

fn internal(e: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::new("internal", e.to_string())
}

fn no_control(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn unique() -> Result<String, ArtifactError> {
    let mut bytes = [0u8; 12];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| internal("no randomness"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl ArtifactService {
    /// A service that keeps its bytes in `root`. Leftovers of a publish
    /// that a crash interrupted are removed.
    #[must_use]
    pub fn new(store: Store, policy: Arc<Policy>, root: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(root.join(".staging"));
        Self {
            store,
            policy,
            root,
        }
    }

    /// The root and the relative path of a publish request.
    async fn resolve(
        &self,
        session: Option<&SessionId>,
        path: &str,
    ) -> Result<(PathBuf, String), ArtifactError> {
        if path.is_empty() || path.chars().count() > MAX_PATH_CHARS || path.contains('\0') {
            return Err(ArtifactError::new(
                code::INVALID_PATH,
                "a path has 1 to 4096 characters and no NUL",
            ));
        }
        let session_root = match session {
            None => None,
            Some(id) => {
                let cwd = self
                    .store
                    .summary(id.clone())
                    .await
                    .map_err(internal)?
                    .and_then(|s| s.cwd)
                    .ok_or_else(|| {
                        ArtifactError::new(
                            code::UNKNOWN_SESSION,
                            format!("session `{id}` is unknown or has no working directory"),
                        )
                    })?;
                Some(self.policy.workspace(&cwd).map_err(|e| match e {
                    WorkspaceError::Missing(_) => {
                        ArtifactError::new(code::NOT_FOUND, e.to_string())
                    }
                    WorkspaceError::Outside(_) => {
                        ArtifactError::new(code::OUTSIDE_ROOTS, e.to_string())
                    }
                })?)
            }
        };
        let given = Path::new(path);
        if given
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(ArtifactError::new(
                code::INVALID_PATH,
                "a path has no `..` component",
            ));
        }
        let absolute = if given.is_absolute() {
            given.to_path_buf()
        } else {
            let Some(root) = &session_root else {
                return Err(ArtifactError::new(
                    code::INVALID_PATH,
                    "a path without a session must be absolute",
                ));
            };
            root.join(given)
        };
        // Symlinked directories resolve here; the copy then walks the
        // canonical path without following any symlink.
        let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
            return Err(ArtifactError::new(
                code::INVALID_PATH,
                "the path names no file",
            ));
        };
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| ArtifactError::new(code::NOT_FOUND, format!("`{path}` is not found")))?;
        let full = parent.join(name);
        let allowed = self.policy.allowed_roots();
        let root = session_root
            .iter()
            .chain(allowed.iter())
            .find(|r| full.starts_with(r))
            .cloned()
            .ok_or_else(|| {
                ArtifactError::new(
                    code::OUTSIDE_ROOTS,
                    "the file is outside the session workspace and the allowed roots",
                )
            })?;
        if private_root(&root, allowed) {
            return Err(ArtifactError::new(
                code::OUTSIDE_ROOTS,
                "the root is a private directory or lies inside one",
            ));
        }
        let rel = full
            .strip_prefix(&root)
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| ArtifactError::new(code::INVALID_PATH, "the path is not UTF-8"))?
            .to_owned();
        Ok((root, rel))
    }

    /// `artifacts.publish` and the `publish_artifact` MCP tool.
    ///
    /// # Errors
    /// [`ArtifactError`] with a code of [`code`].
    pub async fn publish(&self, req: ArtifactPublish) -> Result<ArtifactPublished, ArtifactError> {
        let author = match req.author.as_deref().map(str::trim) {
            None => AUTHOR_OWNER.to_owned(),
            Some(a) if !a.is_empty() && a.chars().count() <= MAX_AUTHOR_CHARS && no_control(a) => {
                a.to_owned()
            }
            Some(_) => {
                return Err(ArtifactError::new(
                    code::INVALID_PARAMS,
                    "`author` has 1 to 64 characters and no control character",
                ));
            }
        };
        let title = match req.title.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(t) if no_control(t) => Some(t.chars().take(MAX_TITLE_CHARS).collect::<String>()),
            Some(_) => {
                return Err(ArtifactError::new(
                    code::INVALID_PARAMS,
                    "`title` has no control character",
                ));
            }
        };
        let (root, rel) = self.resolve(req.session_id.as_ref(), &req.path).await?;
        let default_title = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
        let staging = stage::staging_dir(&self.root, &unique()?);
        let (src_root, src_rel, dir) = (root, rel, staging.clone());
        let staged = tokio::task::spawn_blocking(move || stage::stage(&src_root, &src_rel, &dir))
            .await
            .map_err(internal)?;
        let staged = match staged {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        };
        let committed = self
            .store
            .artifact_commit(NewVersion {
                staged,
                root: self.root.clone(),
                session_id: req.session_id,
                title,
                default_title,
                artifact_id: req.artifact_id,
                author,
            })
            .await;
        // On success the staging directory moved; otherwise it is waste.
        let _ = std::fs::remove_dir_all(&staging);
        match committed {
            Ok(c) => Ok(ArtifactPublished {
                artifact: c.artifact,
                version: c.version,
                created: c.created,
            }),
            Err(CommitError::UnknownArtifact(id)) => Err(ArtifactError::new(
                code::UNKNOWN_ARTIFACT,
                format!("unknown artifact `{id}`"),
            )),
            Err(CommitError::UnknownSession(id)) => Err(ArtifactError::new(
                code::UNKNOWN_SESSION,
                format!("unknown session `{id}`"),
            )),
            Err(e) => Err(internal(e)),
        }
    }

    /// `artifacts.list`.
    ///
    /// # Errors
    /// `internal`.
    pub async fn list(&self, query: ArtifactListQuery) -> Result<ArtifactList, ArtifactError> {
        let artifacts = self
            .store
            .artifacts_of(query.session_id)
            .await
            .map_err(internal)?;
        Ok(ArtifactList { artifacts })
    }

    async fn detail(
        &self,
        id: &str,
    ) -> Result<(Artifact, Vec<fleetdeck_proto::artifacts::ArtifactVersion>), ArtifactError> {
        self.store
            .artifact_detail(id.to_owned())
            .await
            .map_err(internal)?
            .ok_or_else(|| {
                ArtifactError::new(code::UNKNOWN_ARTIFACT, format!("unknown artifact `{id}`"))
            })
    }

    /// `artifacts.get`.
    ///
    /// # Errors
    /// `unknown_artifact` or `internal`.
    pub async fn get(&self, query: ArtifactGetQuery) -> Result<ArtifactDetail, ArtifactError> {
        let (artifact, versions) = self.detail(&query.artifact_id).await?;
        let wanted = query.version.unwrap_or(artifact.latest_version);
        let version = versions
            .iter()
            .find(|v| v.version == wanted)
            .cloned()
            .ok_or_else(|| {
                ArtifactError::new(
                    code::UNKNOWN_ARTIFACT,
                    format!("artifact `{}` has no version {wanted}", query.artifact_id),
                )
            })?;
        Ok(ArtifactDetail {
            artifact,
            version,
            versions,
        })
    }

    /// `artifacts.file`: one chunk of one file of a version.
    ///
    /// # Errors
    /// `unknown_artifact`, `not_found`, `invalid_params` or `internal`.
    pub async fn file(&self, query: ArtifactFileQuery) -> Result<ArtifactFile, ArtifactError> {
        let (_, versions) = self.detail(&query.artifact_id).await?;
        let version = versions
            .iter()
            .find(|v| v.version == query.version)
            .ok_or_else(|| {
                ArtifactError::new(
                    code::UNKNOWN_ARTIFACT,
                    format!(
                        "artifact `{}` has no version {}",
                        query.artifact_id, query.version
                    ),
                )
            })?;
        if !version.files.contains(&query.path) {
            return Err(ArtifactError::new(
                code::NOT_FOUND,
                format!("`{}` is not a file of this version", query.path),
            ));
        }
        let offset = query.offset.unwrap_or(0);
        let length = query.length.unwrap_or(MAX_CHUNK_BYTES);
        if length == 0 || length > MAX_CHUNK_BYTES {
            return Err(ArtifactError::new(
                code::INVALID_PARAMS,
                format!("`length` is 1 to {MAX_CHUNK_BYTES}"),
            ));
        }
        let dir = self
            .root
            .join(&query.artifact_id)
            .join(query.version.to_string());
        let path = query.path.clone();
        tokio::task::spawn_blocking(move || read_chunk(&dir, &path, offset, length))
            .await
            .map_err(internal)?
    }
}

fn read_chunk(
    dir: &Path,
    path: &str,
    offset: u64,
    length: u64,
) -> Result<ArtifactFile, ArtifactError> {
    let missing = || ArtifactError::new(code::NOT_FOUND, format!("`{path}` is missing"));
    let base: OwnedFd = open_root(dir).map_err(|_| missing())?;
    let segments: Vec<&str> = path.split('/').collect();
    let Some((name, parents)) = segments.split_last() else {
        return Err(missing());
    };
    let mut parent = rustix::fs::openat(
        &base,
        c".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| missing())?;
    for segment in parents {
        parent = rustix::fs::openat(
            &parent,
            *segment,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| missing())?;
    }
    let fd = rustix::fs::openat(
        &parent,
        *name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )
    .map_err(|_| missing())?;
    let stat = rustix::fs::fstat(fd.as_fd()).map_err(|_| missing())?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(missing());
    }
    let size = u64::try_from(stat.st_size).unwrap_or(0);
    if offset > size {
        return Err(ArtifactError::new(
            code::INVALID_PARAMS,
            format!("`offset` is past the end of the file ({size} bytes)"),
        ));
    }
    let mut file = std::fs::File::from(fd);
    file.seek(SeekFrom::Start(offset)).map_err(internal)?;
    let mut buf = Vec::new();
    file.take(length).read_to_end(&mut buf).map_err(internal)?;
    let got = buf.len() as u64;
    Ok(ArtifactFile {
        path: path.to_owned(),
        size,
        offset,
        length: got,
        data_base64: STANDARD.encode(&buf),
        eof: offset + got >= size,
    })
}
