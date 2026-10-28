//! Static client releases: immutable, verified copies of both browser clients kept in a
//! directory the server serves from, switched atomically without restarting the server.
//!
//! ```text
//! <root>/releases/<id>/release.json            manifest, never served
//! <root>/releases/<id>/client/dist/...         the map client
//! <root>/releases/<id>/claims-client/dist/...  the claims client
//! <root>/current -> releases/<id>              switched by renaming a new link over it
//! <root>/history                               one activated id per line, oldest first
//! <root>/image                                 the image last reconciled at a start
//! <root>/.lock                                 serializes every change
//! ```
//!
//! A release's id is derived from its files and the backend it was built against, so the
//! same release always installs to the same place and retries converge.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, RwLock};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::asset_version::{asset_content_version, source_commit};

/// Directory of releases to serve; unset keeps serving the clients built into the image.
pub(crate) const RELEASES_ENV: &str = "SEQUOIA_CLIENT_RELEASES";
/// The server's backend key, written next to the binary by the image build.
const BACKEND_KEY_FILE: &str = "backend-key";
const MANIFEST: &str = "release.json";
const HISTORY: &str = "history";
const IMAGE: &str = "image";
/// Releases kept for rollback and for pages still holding older HTML: current and the two
/// most recently active before it.
const RETAINED_RELEASES: usize = 3;

/// A served client: where its files live in a release, and the URL prefix they appear under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ServedClient {
    pub(crate) dir: &'static str,
    mount: &'static str,
}

pub(crate) const MAP_CLIENT: ServedClient = ServedClient {
    dir: "client/dist",
    mount: "/",
};
pub(crate) const CLAIMS_CLIENT: ServedClient = ServedClient {
    dir: "claims-client/dist",
    mount: "/claims-app/",
};
const SERVED_CLIENTS: [ServedClient; 2] = [MAP_CLIENT, CLAIMS_CLIENT];

/// Why a release operation was refused.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReleaseError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("manifest {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("release does not match its manifest: {0}")]
    Mismatch(String),
    #[error("release {0} is not installed")]
    Missing(String),
    #[error("the current release is {actual}, not the expected {expected}")]
    Stale { expected: String, actual: String },
    #[error("release {release} was built for backend {release_key}; this server is {server_key}")]
    Incompatible {
        release: String,
        release_key: String,
        server_key: String,
    },
    #[error("this server has no backend key, so it cannot check a release's compatibility")]
    NoBackendKey,
    #[error("there is no earlier release to roll back to")]
    NoRollbackTarget,
    #[error("{0}")]
    Usage(String),
}

type Result<T> = std::result::Result<T, ReleaseError>;

fn io_error(context: impl Into<String>) -> impl FnOnce(io::Error) -> ReleaseError {
    let context = context.into();
    move |source| ReleaseError::Io { context, source }
}

/// One file's identity in a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileDigest {
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

/// What a release contains and what it needs. A bundle arrives with `source_commit`,
/// `backend_key` and `files`; installation verifies those and fills in the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    #[serde(default)]
    pub(crate) id: String,
    pub(crate) source_commit: String,
    pub(crate) backend_key: String,
    #[serde(default)]
    pub(crate) asset_version: Option<String>,
    pub(crate) files: BTreeMap<String, FileDigest>,
}

impl Manifest {
    /// The id the files and backend key determine.
    fn derived_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.backend_key.as_bytes());
        hasher.update(b"\n");
        for (path, digest) in &self.files {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(digest.sha256.as_bytes());
            hasher.update(b"\0");
            hasher.update(digest.size.to_string().as_bytes());
            hasher.update(b"\n");
        }
        hex(&hasher.finalize())[..16].to_owned()
    }
}

/// The image a server start reconciled: the source revision baked into it, and the release
/// its own clients make.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ImageRecord {
    source_commit: String,
    release: String,
}

impl ImageRecord {
    /// Images are the same when built from the same revision. Rebuilds of one revision can
    /// differ in their compressed copies' timestamps, so content decides only when either
    /// revision is unknown.
    fn is_same_image(&self, other: &Self) -> bool {
        if self.source_commit.is_empty() || other.source_commit.is_empty() {
            return self.release == other.release;
        }
        self.source_commit == other.source_commit
    }
}

/// How a successful activation changed the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Activation {
    pub(crate) current: String,
    pub(crate) previous: Option<String>,
    pub(crate) changed: bool,
    pub(crate) pruned: Vec<String>,
}

/// What the caller believes is current, checked before changing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Expected {
    Nothing,
    Release(String),
}

impl Expected {
    fn parse(value: &str) -> Self {
        match value {
            "none" => Self::Nothing,
            id => Self::Release(id.to_owned()),
        }
    }

    fn matches(&self, current: Option<&str>) -> bool {
        match (self, current) {
            (Self::Nothing, None) => true,
            (Self::Release(expected), Some(current)) => expected == current,
            (Self::Nothing, Some(_)) | (Self::Release(_), None) => false,
        }
    }

    fn describe(&self) -> &str {
        match self {
            Self::Nothing => "none",
            Self::Release(id) => id,
        }
    }
}

/// The releases directory and every operation on it.
#[derive(Clone, Debug)]
pub(crate) struct ReleaseStore {
    root: PathBuf,
}

impl ReleaseStore {
    /// A store rooted at `root`, created if missing.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Io`] when the directories cannot be created.
    pub(crate) fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { root: root.into() };
        for dir in [store.releases(), store.incoming()] {
            fs::create_dir_all(&dir).map_err(io_error(format!("create {}", dir.display())))?;
        }
        Ok(store)
    }

    fn releases(&self) -> PathBuf {
        self.root.join("releases")
    }

    fn incoming(&self) -> PathBuf {
        self.root.join(".incoming")
    }

    /// The directory of an installed release.
    pub(crate) fn release_dir(&self, id: &str) -> PathBuf {
        self.releases().join(id)
    }

    /// The id `current` points at, if any.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Io`] when `current` exists but is not a readable link.
    pub(crate) fn current(&self) -> Result<Option<String>> {
        match fs::read_link(self.root.join("current")) {
            Ok(target) => Ok(target
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error("read the current release")(error)),
        }
    }

    /// The installed manifest of `id`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Missing`] for an unknown id and [`ReleaseError::Manifest`]
    /// for an unreadable manifest.
    pub(crate) fn manifest(&self, id: &str) -> Result<Manifest> {
        if !is_release_id(id) {
            return Err(ReleaseError::Missing(id.to_owned()));
        }
        read_manifest(&self.release_dir(id).join(MANIFEST)).map_err(|error| match error {
            ReleaseError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                ReleaseError::Missing(id.to_owned())
            }
            other => other,
        })
    }

    /// Installed release ids, most recently active first, then any never activated.
    pub(crate) fn installed(&self) -> Result<Vec<String>> {
        let mut ids: Vec<String> = Vec::new();
        for id in self.history()?.into_iter().rev() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let entries =
            fs::read_dir(self.releases()).map_err(io_error("list the installed releases"))?;
        let mut present = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_error("list the installed releases"))?;
            if let Some(name) = entry
                .file_name()
                .to_str()
                .filter(|name| is_release_id(name))
            {
                present.push(name.to_owned());
            }
        }
        ids.retain(|id| present.contains(id));
        present.sort();
        for id in present {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn history(&self) -> Result<Vec<String>> {
        match fs::read_to_string(self.root.join(HISTORY)) {
            Ok(text) => Ok(text
                .lines()
                .filter(|line| is_release_id(line))
                .map(str::to_owned)
                .collect()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(io_error("read the release history")(error)),
        }
    }

    /// The release a rollback would activate: the latest activated release other than the
    /// current one that is still installed. If an activation's history entry was lost, the
    /// last entry is the release it replaced, which is again the right target.
    pub(crate) fn rollback_target(&self) -> Result<Option<String>> {
        let current = self.current()?;
        Ok(self
            .history()?
            .into_iter()
            .rev()
            .find(|id| Some(id.as_str()) != current.as_deref() && self.release_dir(id).is_dir()))
    }

    /// Install the bundle in `staged`, a directory on the store's filesystem holding a
    /// bundle manifest and both clients. On success the directory has been moved into the
    /// store or, if that exact release was already installed, removed.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Mismatch`] for missing, extra, altered or non-regular files,
    /// or a page referencing a missing or differently hashed asset.
    pub(crate) fn install_staged(&self, staged: &Path) -> Result<(String, bool)> {
        let result = self.install_staged_inner(staged);
        if staged.exists() {
            let _ = fs::remove_dir_all(staged);
        }
        result
    }

    fn install_staged_inner(&self, staged: &Path) -> Result<(String, bool)> {
        let mut manifest = read_manifest(&staged.join(MANIFEST))?;
        let files = digest_tree(staged)?;
        compare_files(&manifest.files, &files)?;
        for client in SERVED_CLIENTS {
            check_page_integrity(staged, client)?;
        }
        manifest.id = manifest.derived_id();
        manifest.asset_version = asset_content_version(
            staged,
            &[Path::new(MAP_CLIENT.dir), Path::new(CLAIMS_CLIENT.dir)],
        );
        let target = self.release_dir(&manifest.id);
        if target.is_dir() {
            self.verify(&manifest.id)?;
            return Ok((manifest.id, false));
        }
        write_json(&staged.join(MANIFEST), &manifest)?;
        fs::rename(staged, &target).map_err(io_error(format!("install {}", manifest.id)))?;
        sync_dir(&self.releases())?;
        Ok((manifest.id, true))
    }

    /// Rename a release aside before deleting it, so no request reads a half-removed release.
    fn remove_release(&self, id: &str) -> Result<()> {
        let aside = self.incoming().join(format!("removed-{id}"));
        fs::rename(self.release_dir(id), &aside).map_err(io_error(format!("remove {id}")))?;
        fs::remove_dir_all(&aside).map_err(io_error(format!("remove {id}")))
    }

    /// Install a bundle read as a tar stream from `input`, extracted by the system `tar`.
    pub(crate) fn install_tar(&self, input: impl Read) -> Result<(String, bool)> {
        let staged = self.new_incoming()?;
        let extracted = extract_tar(input, &staged);
        if let Err(error) = extracted {
            let _ = fs::remove_dir_all(&staged);
            return Err(error);
        }
        self.install_staged(&staged)
    }

    /// Write an installed, verified release to `output` as a tar stream: the base a CSS-only
    /// release is built from.
    pub(crate) fn export_tar(&self, id: &str, mut output: impl Write) -> Result<()> {
        self.verify(id)?;
        let archive = Command::new("tar")
            .args(["-c", "--sort=name", "-C"])
            .arg(self.release_dir(id))
            .arg(".")
            .stderr(Stdio::inherit())
            .output()
            .map_err(io_error("start tar"))?;
        if !archive.status.success() {
            return Err(ReleaseError::Mismatch(format!(
                "tar could not archive {id} ({})",
                archive.status
            )));
        }
        output
            .write_all(&archive.stdout)
            .and_then(|()| output.flush())
            .map_err(io_error("write the release archive"))
    }

    /// Install an image's clients, found under `app_root` and described by `manifest`, unless
    /// that exact release is already installed.
    fn install_embedded(&self, app_root: &Path, manifest: &Manifest) -> Result<(String, bool)> {
        let id = manifest.derived_id();
        if self.release_dir(&id).is_dir() {
            return Ok((id, false));
        }
        let staged = self.new_incoming()?;
        let copied = manifest.files.keys().try_for_each(|path| {
            let destination = staged.join(path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(io_error(format!("create {path}")))?;
            }
            fs::copy(app_root.join(path), &destination)
                .map(drop)
                .map_err(io_error(format!("copy {path}")))
        });
        if let Err(error) = copied.and_then(|()| write_json(&staged.join(MANIFEST), manifest)) {
            let _ = fs::remove_dir_all(&staged);
            return Err(error);
        }
        self.install_staged(&staged)
    }

    /// Re-hash an installed release against its manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Mismatch`] when any file or the id no longer matches.
    pub(crate) fn verify(&self, id: &str) -> Result<Manifest> {
        let manifest = self.manifest(id)?;
        compare_files(&manifest.files, &digest_tree(&self.release_dir(id))?)?;
        if manifest.derived_id() != id {
            return Err(ReleaseError::Mismatch(format!(
                "release {id} manifest describes {}",
                manifest.derived_id()
            )));
        }
        Ok(manifest)
    }

    /// Make `id` current if the current release is `expected` and `id` suits `server_key`.
    /// Activating the release that is already current changes nothing, so a retried
    /// activation succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Stale`] when another release became current,
    /// [`ReleaseError::Incompatible`] for a release built against another backend and
    /// [`ReleaseError::Missing`] or [`ReleaseError::Mismatch`] for a bad release.
    pub(crate) fn activate(
        &self,
        id: &str,
        expected: &Expected,
        server_key: &str,
    ) -> Result<Activation> {
        let _lock = self.lock()?;
        let current = self.current()?;
        if current.as_deref() == Some(id) {
            return Ok(Activation {
                current: id.to_owned(),
                previous: self.rollback_target()?,
                changed: false,
                pruned: Vec::new(),
            });
        }
        if !expected.matches(current.as_deref()) {
            return Err(ReleaseError::Stale {
                expected: expected.describe().to_owned(),
                actual: current.unwrap_or_else(|| "none".to_owned()),
            });
        }
        self.switch(id, server_key)
    }

    /// Activate the rollback target if the current release is `expected`.
    ///
    /// # Errors
    ///
    /// As [`ReleaseStore::activate`], plus [`ReleaseError::NoRollbackTarget`].
    pub(crate) fn rollback(&self, expected: &Expected, server_key: &str) -> Result<Activation> {
        let _lock = self.lock()?;
        let current = self.current()?;
        if !expected.matches(current.as_deref()) {
            return Err(ReleaseError::Stale {
                expected: expected.describe().to_owned(),
                actual: current.unwrap_or_else(|| "none".to_owned()),
            });
        }
        let target = self
            .rollback_target()?
            .ok_or(ReleaseError::NoRollbackTarget)?;
        self.switch(&target, server_key)
    }

    /// At server start: activate the image's own clients when this image has not been
    /// reconciled before, or when the current release was built for a different backend.
    /// Otherwise the current release, possibly a later static release or a rollback, stays.
    /// Which image was last reconciled is recorded apart from the releases, so retention
    /// pruning the image's own clients does not make a restart look like a new deployment.
    /// An image is its source revision: a revert to earlier client bytes is a new image,
    /// and a rebuild of the same revision is not.
    pub(crate) fn reconcile_embedded(
        &self,
        app_root: &Path,
        source_commit: &str,
        server_key: &str,
    ) -> Result<Option<Activation>> {
        let embedded = embedded_manifest(app_root, source_commit, server_key)?;
        let image = embedded.derived_id();
        let started = ImageRecord {
            source_commit: source_commit.to_owned(),
            release: image.clone(),
        };
        let _lock = self.lock()?;
        let current = self.current()?;
        let compatible = current.as_deref().is_some_and(|id| {
            self.manifest(id)
                .is_ok_and(|manifest| manifest.backend_key == server_key)
        });
        if compatible
            && self
                .reconciled_image()?
                .is_some_and(|recorded| recorded.is_same_image(&started))
        {
            return Ok(None);
        }
        self.install_embedded(app_root, &embedded)?;
        let activation = if current.as_deref() == Some(image.as_str()) {
            None
        } else {
            Some(self.switch(&image, server_key)?)
        };
        self.record_image(&started)?;
        Ok(activation)
    }

    fn reconciled_image(&self) -> Result<Option<ImageRecord>> {
        match fs::read(self.root.join(IMAGE)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error("read the reconciled image")(error)),
        }
    }

    fn record_image(&self, image: &ImageRecord) -> Result<()> {
        let staged = self.root.join(".image.new");
        write_json(&staged, image)?;
        fs::rename(&staged, self.root.join(IMAGE))
            .map_err(io_error("record the reconciled image"))?;
        sync_dir(&self.root)
    }

    /// Point `current` at `id`, record it and prune. The caller holds the lock.
    fn switch(&self, id: &str, server_key: &str) -> Result<Activation> {
        let manifest = self.verify(id)?;
        if manifest.backend_key != server_key {
            return Err(ReleaseError::Incompatible {
                release: id.to_owned(),
                release_key: manifest.backend_key,
                server_key: server_key.to_owned(),
            });
        }
        let previous = self.current()?;
        let staged_link = self.root.join(".current.new");
        let _ = fs::remove_file(&staged_link);
        std::os::unix::fs::symlink(Path::new("releases").join(id), &staged_link)
            .map_err(io_error("link the new release"))?;
        fs::rename(&staged_link, self.root.join("current"))
            .map_err(io_error("switch the current release"))?;
        sync_dir(&self.root)?;
        let mut history = File::options()
            .create(true)
            .append(true)
            .open(self.root.join(HISTORY))
            .map_err(io_error("open the release history"))?;
        writeln!(history, "{id}")
            .and_then(|()| history.sync_all())
            .map_err(io_error("record the activation"))?;
        let pruned = self.prune(id)?;
        Ok(Activation {
            current: id.to_owned(),
            previous,
            changed: true,
            pruned,
        })
    }

    /// Remove previously active releases outside the retained window, and releases never
    /// activated that were installed before `current`: abandoned candidates. A release
    /// installed after it may still be waiting for its own activation, so it stays.
    fn prune(&self, current: &str) -> Result<Vec<String>> {
        let history = self.history()?;
        let installed_at = |id: &str| {
            fs::metadata(self.release_dir(id))
                .and_then(|metadata| metadata.modified())
                .ok()
        };
        let current_installed = installed_at(current);
        let mut keep = vec![current.to_owned()];
        for id in history.iter().rev() {
            if keep.len() == RETAINED_RELEASES {
                break;
            }
            if !keep.contains(id) {
                keep.push(id.clone());
            }
        }
        let mut pruned = Vec::new();
        for id in self.installed()? {
            let abandoned = !history.contains(&id) && installed_at(&id) < current_installed;
            if !keep.contains(&id) && (history.contains(&id) || abandoned) {
                self.remove_release(&id)?;
                pruned.push(id);
            }
        }
        Ok(pruned)
    }

    fn lock(&self) -> Result<File> {
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".lock"))
            .map_err(io_error("open the release lock"))?;
        lock.lock().map_err(io_error("take the release lock"))?;
        Ok(lock)
    }

    fn new_incoming(&self) -> Result<PathBuf> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = self
            .incoming()
            .join(format!("{}-{nanos}", std::process::id()));
        fs::create_dir(&dir).map_err(io_error("create an incoming release"))?;
        Ok(dir)
    }
}

fn is_release_id(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The release the clients built into an image under `app_root` make.
fn embedded_manifest(app_root: &Path, source_commit: &str, backend_key: &str) -> Result<Manifest> {
    let mut files = BTreeMap::new();
    for client in SERVED_CLIENTS {
        for (path, digest) in digest_tree(&app_root.join(client.dir))? {
            files.insert(format!("{}/{path}", client.dir), digest);
        }
    }
    Ok(Manifest {
        id: String::new(),
        source_commit: source_commit.to_owned(),
        backend_key: backend_key.to_owned(),
        asset_version: None,
        files,
    })
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let bytes = fs::read(path).map_err(io_error(format!("read {}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|source| ReleaseError::Manifest {
        path: path.to_owned(),
        source,
    })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| ReleaseError::Manifest {
        path: path.to_owned(),
        source,
    })?;
    fs::write(path, bytes).map_err(io_error(format!("write {}", path.display())))
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(io_error(format!("sync {}", dir.display())))
}

/// Every regular file under `root` except a top-level manifest, keyed by its `/`-separated
/// relative path. Links and special files are refused: a release is plain bytes.
fn digest_tree(root: &Path) -> Result<BTreeMap<String, FileDigest>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).map_err(io_error(format!("list {}", dir.display())))?;
        for entry in entries {
            let entry = entry.map_err(io_error(format!("list {}", dir.display())))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(io_error(format!("inspect {}", path.display())))?;
            let relative = path
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| ReleaseError::Mismatch(format!("unusable path {}", path.display())))?
                .to_owned();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                if relative != MANIFEST {
                    files.insert(relative, digest_file(&path)?);
                }
            } else {
                return Err(ReleaseError::Mismatch(format!(
                    "{relative} is not a regular file"
                )));
            }
        }
    }
    Ok(files)
}

fn digest_file(path: &Path) -> Result<FileDigest> {
    let mut file = File::open(path).map_err(io_error(format!("open {}", path.display())))?;
    let mut hasher = Sha256::new();
    let size =
        io::copy(&mut file, &mut hasher).map_err(io_error(format!("read {}", path.display())))?;
    Ok(FileDigest {
        sha256: hex(&hasher.finalize()),
        size,
    })
}

fn compare_files(
    expected: &BTreeMap<String, FileDigest>,
    actual: &BTreeMap<String, FileDigest>,
) -> Result<()> {
    if let Some(path) = expected.keys().find(|path| !actual.contains_key(*path)) {
        return Err(ReleaseError::Mismatch(format!("{path} is missing")));
    }
    if let Some(path) = actual.keys().find(|path| !expected.contains_key(*path)) {
        return Err(ReleaseError::Mismatch(format!(
            "{path} is not in the manifest"
        )));
    }
    if let Some((path, _)) = expected
        .iter()
        .find(|(path, digest)| actual[*path] != **digest)
    {
        return Err(ReleaseError::Mismatch(format!(
            "{path} does not match its digest"
        )));
    }
    for client in SERVED_CLIENTS {
        let page = format!("{}/index.html", client.dir);
        if !actual.contains_key(&page) {
            return Err(ReleaseError::Mismatch(format!("{page} is missing")));
        }
    }
    Ok(())
}

/// Each `integrity` reference in a client's page must name a file in the same release with
/// exactly that digest, or the browser would refuse to load it.
fn check_page_integrity(release: &Path, client: ServedClient) -> Result<()> {
    let dist = release.join(client.dir);
    let page = fs::read_to_string(dist.join("index.html"))
        .map_err(io_error(format!("read {}/index.html", client.dir)))?;
    for (url, integrity) in integrity_references(&page) {
        let relative = url
            .strip_prefix(client.mount)
            .filter(|path| {
                !path.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
            })
            .ok_or_else(|| {
                ReleaseError::Mismatch(format!("{} references {url} outside it", client.dir))
            })?;
        let bytes = fs::read(dist.join(relative)).map_err(|_| {
            ReleaseError::Mismatch(format!("{} references missing {url}", client.dir))
        })?;
        if !integrity_matches(&integrity, &bytes) {
            return Err(ReleaseError::Mismatch(format!(
                "{} integrity for {url} does not match",
                client.dir
            )));
        }
    }
    Ok(())
}

fn integrity_references(page: &str) -> Vec<(String, String)> {
    page.split('<')
        .skip(1)
        .filter_map(|tag| {
            let tag = tag.split('>').next()?;
            let integrity = attribute(tag, "integrity")?;
            let url = attribute(tag, "href").or_else(|| attribute(tag, "src"))?;
            Some((url.to_owned(), integrity.to_owned()))
        })
        .collect()
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let length = tag[start..].find('"')?;
    Some(&tag[start..start + length])
}

fn integrity_matches(integrity: &str, bytes: &[u8]) -> bool {
    integrity.split_whitespace().any(|token| {
        let Some((algorithm, expected)) = token.split_once('-') else {
            return false;
        };
        let digest = match algorithm {
            "sha256" => Sha256::digest(bytes).to_vec(),
            "sha384" => Sha384::digest(bytes).to_vec(),
            "sha512" => Sha512::digest(bytes).to_vec(),
            _ => return false,
        };
        base64::engine::general_purpose::STANDARD.encode(digest) == expected
    })
}

fn extract_tar(input: impl Read, destination: &Path) -> Result<()> {
    let mut child = Command::new("tar")
        .args(["-x", "--no-same-owner", "--no-same-permissions", "-C"])
        .arg(destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(io_error("start tar"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ReleaseError::Usage("tar has no input".into()))?;
    let mut input = input;
    let copied = io::copy(&mut input, &mut stdin);
    drop(stdin);
    let status = child.wait().map_err(io_error("wait for tar"))?;
    // tar may exit once it reads the archive's end, before the writer's trailing padding;
    // the extracted files are checked against the manifest either way.
    match copied {
        Err(error) if !(error.kind() == io::ErrorKind::BrokenPipe && status.success()) => {
            return Err(io_error("stream the bundle")(error));
        }
        _ => {}
    }
    if !status.success() {
        return Err(ReleaseError::Mismatch(format!(
            "tar could not extract the bundle ({status})"
        )));
    }
    Ok(())
}

/// This server's backend key, from the file the image build writes.
pub(crate) fn server_backend_key() -> Result<String> {
    let key = fs::read_to_string(BACKEND_KEY_FILE).map_err(|_| ReleaseError::NoBackendKey)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(ReleaseError::NoBackendKey);
    }
    Ok(key.to_owned())
}

/// What the server serves from the store: the current release and the others still retained,
/// cached until `current` changes.
#[derive(Debug)]
pub(crate) struct ServedReleases {
    store: ReleaseStore,
    snapshot: RwLock<Option<Arc<Snapshot>>>,
}

/// The served releases at one moment, current first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) releases: Vec<ServedRelease>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ServedRelease {
    pub(crate) id: String,
    pub(crate) dir: PathBuf,
    pub(crate) asset_version: Option<String>,
}

impl Snapshot {
    /// The current release.
    pub(crate) fn current(&self) -> &ServedRelease {
        &self.releases[0]
    }
}

impl ServedReleases {
    pub(crate) fn new(store: ReleaseStore) -> Self {
        Self {
            store,
            snapshot: RwLock::new(None),
        }
    }

    /// The releases being served now, or `None` when nothing is current.
    pub(crate) fn snapshot(&self) -> Option<Arc<Snapshot>> {
        let current = self.store.current().ok().flatten()?;
        if let Some(snapshot) = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|snapshot| snapshot.current().id == current)
        {
            return Some(Arc::clone(snapshot));
        }
        let mut ids = self.store.installed().ok()?;
        ids.retain(|id| *id != current);
        ids.insert(0, current.clone());
        let releases = ids
            .into_iter()
            .filter_map(|id| {
                let manifest = self.store.manifest(&id).ok()?;
                Some(ServedRelease {
                    dir: self.store.release_dir(&id),
                    id,
                    asset_version: manifest.asset_version,
                })
            })
            .collect::<Vec<_>>();
        if releases.first().map(|first| first.id.as_str()) != Some(current.as_str()) {
            return None;
        }
        let snapshot = Arc::new(Snapshot { releases });
        *self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&snapshot));
        Some(snapshot)
    }
}

/// Open the configured store and reconcile it with the image's clients. `None` serves the
/// clients built into the image: no store is configured, or it could not be used safely.
pub(crate) fn serve_from_environment() -> Option<ServedReleases> {
    let root = std::env::var_os(RELEASES_ENV).filter(|root| !root.is_empty())?;
    let prepared = server_backend_key().and_then(|key| {
        let store = ReleaseStore::open(PathBuf::from(&root))?;
        let commit = source_commit().unwrap_or_default();
        let activation = store.reconcile_embedded(Path::new(""), &commit, &key)?;
        Ok((store, activation))
    });
    match prepared {
        Ok((store, activation)) => {
            match activation {
                Some(activation) => {
                    tracing::info!(current = %activation.current, "activated the image's clients");
                }
                None => {
                    tracing::info!(current = ?store.current().ok().flatten(), "serving client releases")
                }
            }
            Some(ServedReleases::new(store))
        }
        Err(error) => {
            tracing::error!(%error, "client releases unavailable; serving the image's clients");
            None
        }
    }
}

/// `sequoia-server client-release <command>`: run release operations in the server's own
/// container, printing JSON. Returns the process exit code.
pub(crate) fn run_cli(args: &[String]) -> i32 {
    match cli(args) {
        Ok(serde_json::Value::Null) => 0,
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(error) => {
            eprintln!("client-release: {error}");
            match error {
                ReleaseError::Usage(_) => 2,
                _ => 1,
            }
        }
    }
}

const USAGE: &str = "usage: client-release status | install | export <id> | \
activate <id> --expect <id|none> | rollback --expect <id|none>";

fn cli(args: &[String]) -> Result<serde_json::Value> {
    let root = std::env::var_os(RELEASES_ENV)
        .filter(|root| !root.is_empty())
        .ok_or_else(|| ReleaseError::Usage(format!("{RELEASES_ENV} is not set")))?;
    let store = ReleaseStore::open(PathBuf::from(root))?;
    let expected = |rest: &[String]| match rest {
        [flag, value] if flag == "--expect" => Ok(Expected::parse(value)),
        _ => Err(ReleaseError::Usage(USAGE.to_owned())),
    };
    match args {
        [command] if command == "status" => status(&store),
        [command] if command == "install" => {
            let (id, installed) = store.install_tar(io::stdin().lock())?;
            Ok(serde_json::json!({ "id": id, "installed": installed }))
        }
        [command, id] if command == "export" => {
            store.export_tar(id, io::stdout().lock())?;
            Ok(serde_json::Value::Null)
        }
        [command, id, rest @ ..] if command == "activate" => {
            let activation = store.activate(id, &expected(rest)?, &server_backend_key()?)?;
            Ok(serde_json::to_value(activation).unwrap_or_default())
        }
        [command, rest @ ..] if command == "rollback" => {
            let activation = store.rollback(&expected(rest)?, &server_backend_key()?)?;
            Ok(serde_json::to_value(activation).unwrap_or_default())
        }
        _ => Err(ReleaseError::Usage(USAGE.to_owned())),
    }
}

fn status(store: &ReleaseStore) -> Result<serde_json::Value> {
    let current = store.current()?;
    let releases = store
        .installed()?
        .into_iter()
        .map(|id| {
            let manifest = store.manifest(&id).ok();
            serde_json::json!({
                "id": id,
                "current": current.as_deref() == Some(id.as_str()),
                "source_commit": manifest.as_ref().map(|manifest| manifest.source_commit.clone()),
                "backend_key": manifest.as_ref().map(|manifest| manifest.backend_key.clone()),
                "asset_version": manifest.and_then(|manifest| manifest.asset_version),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "current": current,
        "rollback_target": store.rollback_target()?,
        "server_backend_key": server_backend_key().ok(),
        "releases": releases,
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const KEY: &str = "backend-a";

    /// A release bundle in `dir`: both pages, a stylesheet they reference with integrity, and
    /// a tile whose bytes set the asset version.
    pub(crate) fn write_bundle(dir: &Path, css: &str, tile: &str) {
        let sri = format!(
            "sha384-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha384::digest(css.as_bytes()))
        );
        let css_name = format!("input-{}.css", &hex(&Sha256::digest(css.as_bytes()))[..16]);
        let map = dir.join(MAP_CLIENT.dir);
        let claims = dir.join(CLAIMS_CLIENT.dir);
        fs::create_dir_all(map.join("tiles")).expect("create map dist");
        fs::create_dir_all(&claims).expect("create claims dist");
        fs::write(map.join(&css_name), css).expect("write css");
        fs::write(map.join("tiles/main.webp"), tile).expect("write tile");
        fs::write(
            map.join("index.html"),
            format!(
                "<link rel=\"stylesheet\" href=\"/{css_name}\" integrity=\"{sri}\"/>\
                 <img src=\"/tiles/main.webp?v=__SEQUOIA_ASSET_VERSION__\">"
            ),
        )
        .expect("write map page");
        fs::write(claims.join("index.html"), "<p>claims</p>").expect("write claims page");
        let manifest = Manifest {
            id: String::new(),
            source_commit: "c0ffee".to_owned(),
            backend_key: KEY.to_owned(),
            asset_version: None,
            files: digest_tree(dir).expect("digest bundle"),
        };
        write_json(&dir.join(MANIFEST), &manifest).expect("write manifest");
    }

    pub(crate) fn installed(store: &ReleaseStore, css: &str, tile: &str) -> String {
        let staged = store.new_incoming().expect("incoming");
        write_bundle(&staged, css, tile);
        store.install_staged(&staged).expect("install").0
    }

    fn store() -> (tempfile::TempDir, ReleaseStore) {
        let root = tempfile::tempdir().expect("temp root");
        let store = ReleaseStore::open(root.path()).expect("open store");
        (root, store)
    }

    #[test]
    fn install_is_content_addressed_and_idempotent() {
        let (_root, store) = store();
        let first = installed(&store, "a{}", "tile");
        let again = store.new_incoming().expect("incoming");
        write_bundle(&again, "a{}", "tile");
        assert_eq!(
            store.install_staged(&again).expect("reinstall"),
            (first.clone(), false)
        );
        assert!(!again.exists(), "a duplicate bundle is not left behind");
        let manifest = store.verify(&first).expect("verify");
        assert!(manifest.asset_version.is_some());
        assert_ne!(installed(&store, "b{}", "tile"), first);
    }

    #[test]
    fn install_rejects_bundles_that_do_not_match() {
        type Damage = fn(&Path);
        let (_root, store) = store();
        let cases: [(&str, Damage); 5] = [
            ("altered file", |dir| {
                fs::write(dir.join("client/dist/tiles/main.webp"), "other").expect("alter")
            }),
            ("extra file", |dir| {
                fs::write(dir.join("client/dist/extra.js"), "x").expect("add")
            }),
            ("missing file", |dir| {
                fs::remove_file(dir.join("client/dist/tiles/main.webp")).expect("remove")
            }),
            ("link", |dir| {
                std::os::unix::fs::symlink("/etc/passwd", dir.join("client/dist/link"))
                    .expect("link")
            }),
            ("wrong integrity", |dir| {
                let page = dir.join("client/dist/index.html");
                let html = fs::read_to_string(&page).expect("read page");
                fs::write(&page, html.replace("sha384-", "sha384-AAAA")).expect("rewrite");
                let mut manifest = read_manifest(&dir.join(MANIFEST)).expect("manifest");
                manifest.files = digest_tree(dir).expect("digest");
                write_json(&dir.join(MANIFEST), &manifest).expect("write");
            }),
        ];
        for (case, damage) in cases {
            let staged = store.new_incoming().expect("incoming");
            write_bundle(&staged, "a{}", "tile");
            damage(&staged);
            let error = store.install_staged(&staged).expect_err(case);
            assert!(
                matches!(error, ReleaseError::Mismatch(_)),
                "{case}: {error}"
            );
            assert!(!staged.exists(), "{case}: the rejected bundle is removed");
        }
        assert!(store.installed().expect("list").is_empty());
    }

    #[test]
    fn activation_is_guarded_and_rollback_returns_to_the_previous_release() {
        let (_root, store) = store();
        let first = installed(&store, "a{}", "tile");
        let activation = store
            .activate(&first, &Expected::Nothing, KEY)
            .expect("activate");
        let second = installed(&store, "b{}", "tile");
        assert!(activation.changed);
        let repeat = store
            .activate(&first, &Expected::Nothing, KEY)
            .expect("a repeated activation converges");
        assert!(!repeat.changed);

        let stale = store.activate(&second, &Expected::Nothing, KEY);
        assert!(matches!(stale, Err(ReleaseError::Stale { .. })));
        let incompatible = store.activate(&second, &Expected::Release(first.clone()), "backend-b");
        assert!(
            matches!(incompatible, Err(ReleaseError::Incompatible { .. })),
            "{incompatible:?}"
        );
        let missing = store.activate("0123456789abcdef", &Expected::Release(first.clone()), KEY);
        assert!(matches!(missing, Err(ReleaseError::Missing(_))));
        assert_eq!(
            store.current().expect("current").as_deref(),
            Some(first.as_str())
        );

        store
            .activate(&second, &Expected::Release(first.clone()), KEY)
            .expect("activate second");
        let back = store
            .rollback(&Expected::Release(second.clone()), KEY)
            .expect("roll back");
        assert_eq!(
            (back.current.as_str(), back.previous.as_deref()),
            (first.as_str(), Some(second.as_str()))
        );
        let forward = store
            .rollback(&Expected::Release(first.clone()), KEY)
            .expect("roll back the rollback");
        assert_eq!(forward.current, second);
    }

    #[test]
    fn a_tampered_release_is_not_activated() {
        let (_root, store) = store();
        let id = installed(&store, "a{}", "tile");
        fs::write(
            store.release_dir(&id).join("client/dist/tiles/main.webp"),
            "other",
        )
        .expect("tamper");
        let result = store.activate(&id, &Expected::Nothing, KEY);
        assert!(matches!(result, Err(ReleaseError::Mismatch(_))));
        assert_eq!(store.current().expect("current"), None);
    }

    #[test]
    fn concurrent_promotions_have_one_winner() {
        let (_root, store) = store();
        let first = installed(&store, "first{}", "tile");
        store
            .activate(&first, &Expected::Nothing, KEY)
            .expect("first");
        let second = installed(&store, "second{}", "tile");
        store
            .activate(&second, &Expected::Release(first.clone()), KEY)
            .expect("second");
        let base = installed(&store, "base{}", "tile");
        store
            .activate(&base, &Expected::Release(second.clone()), KEY)
            .expect("base");
        for _ in 0..20 {
            let outcomes: Vec<Result<Activation>> = std::thread::scope(|scope| {
                let handles: Vec<_> = [&first, &second]
                    .into_iter()
                    .map(|id| {
                        let store = store.clone();
                        let base = base.clone();
                        scope.spawn(move || store.activate(id, &Expected::Release(base), KEY))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("join"))
                    .collect()
            });
            assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
            assert!(
                outcomes
                    .iter()
                    .any(|outcome| matches!(outcome, Err(ReleaseError::Stale { .. })))
            );
            let winner = store
                .current()
                .expect("current")
                .expect("a release is current");
            store
                .activate(&base, &Expected::Release(winner), KEY)
                .expect("reset");
        }
    }

    #[test]
    fn simultaneous_install_and_activate_has_one_winner_and_cleans_up() {
        let (_root, store) = store();
        let base = installed(&store, "base{}", "tile");
        store
            .activate(&base, &Expected::Nothing, KEY)
            .expect("base");
        let outcomes: Vec<Result<Activation>> = std::thread::scope(|scope| {
            let handles: Vec<_> = ["a{}", "b{}"]
                .into_iter()
                .map(|css| {
                    let store = store.clone();
                    let base = base.clone();
                    scope.spawn(move || {
                        let id = installed(&store, css, "tile");
                        store.activate(&id, &Expected::Release(base), KEY)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("join"))
                .collect()
        });
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        let winner = store.current().expect("current").expect("winner");
        let abandoned = installed(&store, "abandoned{}", "tile");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let next = installed(&store, "next{}", "tile");
        store
            .activate(&next, &Expected::Release(winner), KEY)
            .expect("next");
        assert!(!store.installed().expect("list").contains(&abandoned));
    }

    #[test]
    fn pruning_keeps_the_current_and_two_earlier_releases() {
        let (_root, store) = store();
        let mut current = Expected::Nothing;
        let mut ids = Vec::new();
        for index in 0..5 {
            let id = installed(&store, &format!("r{index}{{}}"), "tile");
            store.activate(&id, &current, KEY).expect("activate");
            current = Expected::Release(id.clone());
            ids.push(id);
        }
        let mut kept = store.installed().expect("list");
        kept.sort();
        let mut expected = ids[2..].to_vec();
        expected.sort();
        assert_eq!(kept, expected);
        assert_eq!(
            store.rollback_target().expect("target").as_deref(),
            Some(ids[3].as_str())
        );
    }

    /// An image directory as the server finds it: both clients, no manifest.
    fn image(css: &str) -> tempfile::TempDir {
        let image = tempfile::tempdir().expect("image");
        write_bundle(image.path(), css, "tile");
        fs::remove_file(image.path().join(MANIFEST)).expect("images carry no manifest");
        image
    }

    fn activate_static(store: &ReleaseStore, css: &str) -> String {
        let current = store
            .current()
            .expect("current")
            .expect("a release is current");
        let id = installed(store, css, "tile");
        store
            .activate(&id, &Expected::Release(current), KEY)
            .expect("static release");
        id
    }

    #[test]
    fn restarting_an_image_keeps_a_static_release_after_its_own_clients_are_pruned() {
        let (_root, store) = store();
        let running = image("image{}");
        let first = store
            .reconcile_embedded(running.path(), "c0ffee", KEY)
            .expect("first start")
            .expect("a new image activates its own clients");
        let statics: Vec<String> = (0..RETAINED_RELEASES)
            .map(|index| activate_static(&store, &format!("static-{index}{{}}")))
            .collect();
        assert!(
            !store.release_dir(&first.current).exists(),
            "retention pruned the image's own clients"
        );

        let restart = store
            .reconcile_embedded(running.path(), "c0ffee", KEY)
            .expect("restart");
        assert_eq!(restart, None, "the same image is not a new deployment");
        assert_eq!(store.current().expect("current").as_ref(), statics.last());

        let back = store
            .rollback(&Expected::Release(statics[2].clone()), KEY)
            .expect("roll back");
        let restart = store
            .reconcile_embedded(running.path(), "c0ffee", KEY)
            .expect("restart after a rollback");
        assert_eq!(restart, None, "a restart keeps the rollback");
        assert_eq!(store.current().expect("current"), Some(back.current));

        let next = image("next-image{}");
        let deployed = store
            .reconcile_embedded(next.path(), "d00d", KEY)
            .expect("new image")
            .expect("a new image activates its own clients");
        assert_ne!(deployed.current, first.current);
        let restart = store
            .reconcile_embedded(next.path(), "d00d", KEY)
            .expect("restart of the new image");
        assert_eq!(restart, None);

        let rebuilt = store
            .reconcile_embedded(next.path(), "d00d", "backend-b")
            .expect("new backend")
            .expect("clients built for another backend are replaced");
        assert_eq!(
            store
                .manifest(&rebuilt.current)
                .expect("manifest")
                .backend_key,
            "backend-b"
        );
    }

    #[test]
    fn a_new_revision_with_earlier_client_bytes_activates_its_own_clients() {
        let (_root, store) = store();
        let clients = image("image{}");
        let first = store
            .reconcile_embedded(clients.path(), "aaaa", KEY)
            .expect("first start")
            .expect("a new image activates its own clients");
        let static_release = activate_static(&store, "static{}");

        let reverted = store
            .reconcile_embedded(clients.path(), "cccc", KEY)
            .expect("a source revert deploys the same client bytes")
            .expect("a new revision activates its own clients");
        assert_eq!(reverted.current, first.current);
        assert_eq!(reverted.previous, Some(static_release.clone()));

        activate_static(&store, "static-again{}");
        let rebuilt = image("image{}");
        fs::write(
            rebuilt.path().join("client/dist/index.html.gz"),
            "rebuilt sidecar",
        )
        .expect("sidecar");
        let restart = store
            .reconcile_embedded(rebuilt.path(), "cccc", KEY)
            .expect("a rebuild of the same revision");
        assert_eq!(
            restart, None,
            "a same-revision rebuild keeps the static release"
        );

        let unknown = store
            .reconcile_embedded(rebuilt.path(), "", KEY)
            .expect("an image without a revision")
            .expect("its own clients, compared by content, are new");
        let restart = store
            .reconcile_embedded(rebuilt.path(), "", KEY)
            .expect("a restart without a revision");
        assert_eq!(restart, None);
        assert_eq!(store.current().expect("current"), Some(unknown.current));
    }

    #[test]
    fn image_clients_activate_once_and_after_a_backend_change() {
        let (_root, store) = store();
        let image = tempfile::tempdir().expect("image");
        write_bundle(image.path(), "image{}", "tile");
        fs::remove_file(image.path().join(MANIFEST)).expect("images carry no manifest");

        let started = store
            .reconcile_embedded(image.path(), "c0ffee", KEY)
            .expect("first start")
            .expect("the new image's clients are activated");
        let static_release = installed(&store, "static{}", "tile");
        store
            .activate(
                &static_release,
                &Expected::Release(started.current.clone()),
                KEY,
            )
            .expect("static release");

        let restart = store
            .reconcile_embedded(image.path(), "c0ffee", KEY)
            .expect("restart");
        assert_eq!(restart, None, "a restart keeps the later static release");
        assert_eq!(store.current().expect("current"), Some(static_release));

        let new_backend = store
            .reconcile_embedded(image.path(), "c0ffee", "backend-b")
            .expect("new backend")
            .expect("an incompatible current release is replaced");
        assert_ne!(new_backend.current, started.current);
        assert_eq!(
            store
                .manifest(&new_backend.current)
                .expect("manifest")
                .backend_key,
            "backend-b"
        );
    }

    #[test]
    fn a_tar_bundle_installs_like_a_directory() {
        let (_root, store) = store();
        let bundle = tempfile::tempdir().expect("bundle");
        write_bundle(bundle.path(), "a{}", "tile");
        let archive = Command::new("tar")
            .args(["-c", "-C"])
            .arg(bundle.path())
            .arg(".")
            .output()
            .expect("tar the bundle");
        let (id, installed) = store
            .install_tar(archive.stdout.as_slice())
            .expect("install tar");
        assert!(installed);
        store.verify(&id).expect("verify");
        assert!(store.install_tar(&b"not a tar"[..]).is_err());

        let mut exported = Vec::new();
        store.export_tar(&id, &mut exported).expect("export");
        let (again, installed) = store.install_tar(exported.as_slice()).expect("reinstall");
        assert_eq!(
            (again, installed),
            (id, false),
            "an exported release reinstalls unchanged"
        );
    }
}
