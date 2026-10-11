//! Public territory-map images rendered from the live snapshot.
//!
//! [`MapScene`] captures what an image shows; [`MapImages`] turns a scene into PNG or WebP
//! bytes. Rendering runs on Tokio's blocking pool one image at a time behind a short bounded
//! queue, and the latest encoded image per format and width is cached.
//!
//! Terrain is resampled from the source tiles for every render instead of being cached: a
//! near-source-resolution canvas is about 100 MB, and the server runs in a 768 MB container.

mod assets;
mod encode;
mod frame;
mod glyphs;
mod paint;
mod palette;
mod scene;
mod terrain;

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::sync::Semaphore;

pub use assets::AssetError;
pub use scene::MapScene;

use crate::asset_version::{asset_content_version, source_commit};
use assets::Assets;
use frame::Frame;

/// Width when a request does not ask for one, and the most an export may be: about one pixel
/// per block, the source terrain's resolution. A WebP at this width is about 7 MB, inside
/// Discord's 10 MiB attachment limit, and a render peaks about 270 MB above the server's
/// baseline, inside its 768 MB container.
pub const DEFAULT_WIDTH: u32 = MAX_WIDTH;
pub const MIN_WIDTH: u32 = 800;
pub const MAX_WIDTH: u32 = 4096;

/// Renders at once; each holds a full-size canvas plus the codec's working copy.
const RENDER_CONCURRENCY: usize = 1;
/// Requests that may hold or wait for a render before new ones are turned away.
const RENDER_QUEUE: usize = 4;
const ENCODED_CACHE_ENTRIES: usize = 4;
const ENCODED_CACHE_BYTES: usize = 48 * 1024 * 1024;

/// An output codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// Lossless and large; for archiving or further editing.
    Png,
    /// Lossy and compact; what the Discord command attaches.
    Webp,
}

impl ImageFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }
}

/// A validated image request: codec and exact pixel width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageSpec {
    format: ImageFormat,
    width: u32,
}

/// A requested width outside [`MIN_WIDTH`]..=[`MAX_WIDTH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("width must be a whole number of pixels from {MIN_WIDTH} to {MAX_WIDTH}")]
pub struct InvalidWidth;

impl ImageSpec {
    /// Validates a request; `None` selects [`DEFAULT_WIDTH`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidWidth`] for widths outside [`MIN_WIDTH`]..=[`MAX_WIDTH`].
    pub fn new(format: ImageFormat, width: Option<u32>) -> Result<Self, InvalidWidth> {
        let width = width.unwrap_or(DEFAULT_WIDTH);
        if !(MIN_WIDTH..=MAX_WIDTH).contains(&width) {
            return Err(InvalidWidth);
        }
        Ok(Self { format, width })
    }
}

/// Why an image could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum MapImageError {
    /// The render queue is full; retrying shortly is expected to succeed.
    #[error("map renderer is busy")]
    Busy,
    #[error(transparent)]
    Assets(#[from] AssetError),
    #[error("cannot resample terrain tile {file}: {message}")]
    Resample { file: &'static str, message: String },
    #[error("map canvas cannot be allocated")]
    Canvas,
    #[error("map image encoding failed: {0}")]
    Encode(String),
    #[error("map render task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

type ImageKey = (ImageSpec, u64);

/// Renders and caches map images. Cheap to clone; clones share caches and limits.
#[derive(Clone)]
pub struct MapImages {
    shared: Arc<Shared>,
}

struct Shared {
    root: PathBuf,
    /// Deployed source revision and asset contents: with the scene, they decide the bytes.
    revision: String,
    assets: Mutex<Option<Arc<Assets>>>,
    encoded: Mutex<VecDeque<(ImageKey, Bytes)>>,
    render: Arc<Semaphore>,
    queue: Arc<Semaphore>,
}

impl MapImages {
    /// Renderer reading fonts, icons and tiles from a website asset directory.
    ///
    /// Hashes the directory's versioned assets for validators, like the site's `?v=` asset
    /// version; nothing is decoded until the first render, so a missing directory surfaces as
    /// [`MapImageError::Assets`] on requests rather than at startup.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        // Without a known source revision, tags cannot be compared across processes.
        let code = source_commit().unwrap_or_else(|| {
            let started = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("process-{}-{started}", std::process::id())
        });
        let assets = asset_content_version(&[root.as_path()]).unwrap_or_default();
        Self {
            shared: Arc::new(Shared {
                revision: format!("{code}/{assets}"),
                root,
                assets: Mutex::new(None),
                encoded: Mutex::new(VecDeque::with_capacity(ENCODED_CACHE_ENTRIES + 1)),
                render: Arc::new(Semaphore::new(RENDER_CONCURRENCY)),
                queue: Arc::new(Semaphore::new(RENDER_QUEUE)),
            }),
        }
    }

    /// Strong validator for the image of `scene` in `spec`: equal tags mean byte-identical
    /// bodies, also across deployments, because the digest covers the deployed revision and
    /// asset contents as well as the scene.
    pub fn etag(&self, scene: &MapScene, spec: ImageSpec) -> String {
        let mut hasher = DefaultHasher::new();
        (&self.shared.revision, spec, scene.fingerprint()).hash(&mut hasher);
        format!(
            "\"map-{}-{}-{:016x}\"",
            spec.width,
            spec.format.extension(),
            hasher.finish()
        )
    }

    /// The encoded image of `scene`, rendered unless an identical one is cached.
    ///
    /// # Errors
    ///
    /// [`MapImageError::Busy`] when the render queue is full; otherwise an asset, drawing
    /// or codec failure.
    pub async fn image(&self, scene: MapScene, spec: ImageSpec) -> Result<Bytes, MapImageError> {
        let key = (spec, scene.fingerprint());
        if let Some(bytes) = self.shared.cached(key) {
            return Ok(bytes);
        }
        let _queued = Arc::clone(&self.shared.queue)
            .try_acquire_owned()
            .map_err(|_| MapImageError::Busy)?;
        let permit = Arc::clone(&self.shared.render)
            .acquire_owned()
            .await
            .map_err(|_| MapImageError::Busy)?;
        // Concurrent requests for the same image wait here and reuse the first render.
        if let Some(bytes) = self.shared.cached(key) {
            return Ok(bytes);
        }
        let shared = Arc::clone(&self.shared);
        tokio::task::spawn_blocking(move || {
            // The permit lives as long as the work: a disconnected client must not let a
            // second render start while this one is still running.
            let _permit = permit;
            shared.render(&scene, key)
        })
        .await?
    }
}

impl Shared {
    fn render(&self, scene: &MapScene, key: ImageKey) -> Result<Bytes, MapImageError> {
        let (spec, _) = key;
        let assets = self.assets()?;
        let frame = Frame::for_width(spec.width);
        let started = Instant::now();
        let mut canvas = terrain::compose(&assets.tiles, frame)?;
        let terrain_done = Instant::now();
        paint::paint(&mut canvas, scene, frame, &assets);
        let paint_done = Instant::now();
        let bytes = Bytes::from(encode::encode(&canvas, spec.format)?);
        drop(canvas);
        tracing::info!(
            width = frame.width(),
            height = frame.height(),
            format = spec.format.extension(),
            bytes = bytes.len(),
            terrain_ms = (terrain_done - started).as_millis(),
            paint_ms = (paint_done - terrain_done).as_millis(),
            encode_ms = paint_done.elapsed().as_millis(),
            "rendered map image"
        );
        self.remember(key, bytes.clone());
        Ok(bytes)
    }

    fn assets(&self) -> Result<Arc<Assets>, MapImageError> {
        let mut slot = lock(&self.assets);
        if let Some(assets) = slot.as_ref() {
            return Ok(Arc::clone(assets));
        }
        let assets = Arc::new(Assets::load(&self.root)?);
        *slot = Some(Arc::clone(&assets));
        Ok(assets)
    }

    fn cached(&self, key: ImageKey) -> Option<Bytes> {
        lock(&self.encoded)
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, bytes)| bytes.clone())
    }

    /// Keeps only the newest image per format and width; older scenes are never served again.
    fn remember(&self, key: ImageKey, bytes: Bytes) {
        let mut cache = lock(&self.encoded);
        cache.retain(|((spec, _), _)| *spec != key.0);
        cache.push_front((key, bytes));
        while cache.len() > ENCODED_CACHE_ENTRIES
            || (cache.len() > 1
                && cache.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > ENCODED_CACHE_BYTES)
        {
            cache.pop_back();
        }
    }
}

/// Cache state stays consistent even if a render panicked while another thread held a lock.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use sequoia_shared::LiveState;

    use super::*;

    #[tokio::test]
    async fn requests_beyond_the_queue_are_turned_away_while_rendering() {
        let map = sequoia_shared::TerritoryMap::from([(
            "Ragni".to_string(),
            scene::tests::territory("Sequoia", "SEQ", chrono::Utc::now()),
        )]);
        let scene = MapScene::new(&map, chrono::Utc::now());
        let spec = ImageSpec::new(ImageFormat::Webp, None).expect("default width");
        let images = MapImages::new("/nonexistent-map-assets");
        let rendering = Arc::clone(&images.shared.render)
            .try_acquire_owned()
            .expect("idle renderer");

        let mut queued: Vec<_> = (0..RENDER_QUEUE)
            .map(|_| Box::pin(images.image(scene.clone(), spec)))
            .collect();
        for request in &mut queued {
            assert!(futures::poll!(request).is_pending());
        }
        let overflow = images.image(scene.clone(), spec).await;

        assert!(matches!(overflow, Err(MapImageError::Busy)));
        drop(queued);
        drop(rendering);
        let after = images.image(scene, spec).await;
        assert!(matches!(after, Err(MapImageError::Assets(_))), "{after:?}");
    }

    #[test]
    fn validators_follow_the_deployed_revision_and_asset_contents() {
        let root =
            std::env::temp_dir().join(format!("sequoia-map-image-revision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fonts")).expect("fonts dir");
        std::fs::write(root.join("fonts/minecraft-bold.otf"), b"font one").expect("font");
        let map = sequoia_shared::TerritoryMap::from([(
            "Ragni".to_string(),
            scene::tests::territory("Sequoia", "SEQ", chrono::Utc::now()),
        )]);
        let scene = MapScene::new(&map, chrono::Utc::now());
        let spec = ImageSpec::new(ImageFormat::Webp, None).expect("default width");
        let tag = || MapImages::new(&root).etag(&scene, spec);

        temp_env::with_var("SOURCE_COMMIT", Some("abc123"), || {
            let deployed = tag();
            // Every process of one deployment agrees, so revalidation survives restarts.
            assert_eq!(tag(), deployed);
            std::fs::write(root.join("fonts/minecraft-bold.otf"), b"font two").expect("font");
            let new_assets = tag();
            assert_ne!(new_assets, deployed);
            let new_code = temp_env::with_var("SOURCE_COMMIT", Some("def456"), tag);
            assert_ne!(new_code, new_assets);
        });
        // An unknown revision never lets two processes share a validator.
        temp_env::with_var_unset("SOURCE_COMMIT", || assert_ne!(tag(), tag()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Writes previews of a saved live state, e.g.
    /// `MAP_IMAGE_PREVIEW_FIXTURE=live-state.json MAP_IMAGE_PREVIEW_DIR=out \
    /// cargo test -p sequoia-server previews -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "writes preview files; set MAP_IMAGE_PREVIEW_FIXTURE and MAP_IMAGE_PREVIEW_DIR"]
    async fn previews() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let fixture = std::env::var("MAP_IMAGE_PREVIEW_FIXTURE").expect("fixture path");
        let out = PathBuf::from(std::env::var("MAP_IMAGE_PREVIEW_DIR").expect("output dir"));
        let live: LiveState =
            serde_json::from_slice(&std::fs::read(fixture).expect("read fixture")).expect("parse");
        let as_of = chrono::DateTime::parse_from_rfc3339(&live.timestamp)
            .expect("timestamp")
            .to_utc();
        let images = MapImages::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../client/public"));
        let widths: Vec<u32> = std::env::var("MAP_IMAGE_PREVIEW_WIDTHS")
            .map(|widths| widths.split(',').filter_map(|w| w.parse().ok()).collect())
            .unwrap_or_else(|_| vec![DEFAULT_WIDTH]);
        for width in widths {
            let formats = match std::env::var("MAP_IMAGE_PREVIEW_FORMATS").as_deref() {
                Ok("png") => vec![ImageFormat::Png],
                Ok("webp") => vec![ImageFormat::Webp],
                _ => vec![ImageFormat::Webp, ImageFormat::Png],
            };
            for format in formats {
                let spec = ImageSpec::new(format, Some(width)).expect("width");
                let started = Instant::now();
                let bytes = images
                    .image(MapScene::new(&live.territories, as_of), spec)
                    .await
                    .expect("render");
                let elapsed = started.elapsed();
                let path = out.join(format!("map-{width}.{}", format.extension()));
                std::fs::write(&path, &bytes).expect("write preview");
                println!("{} {} bytes in {elapsed:?}", path.display(), bytes.len());
            }
        }
    }
}
