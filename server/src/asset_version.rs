//! Version identities for deployed content: the source revision and static asset contents.

use std::path::Path;

/// Served directories whose files are requested with the `?v=` asset version.
const VERSIONED_ASSET_DIRS: [&str; 3] = ["tiles", "fonts", "icons"];

/// The file in which the image build records the commit it was built from.
const SOURCE_COMMIT_FILE: &str = "source-commit";

/// The deployed source revision: the commit the image was built from, then `SOURCE_COMMIT`.
/// The image's own record comes first because a platform can set a container's
/// `SOURCE_COMMIT` to something other than the commit the image's clients were built from.
pub(crate) fn source_commit() -> Option<String> {
    std::fs::read_to_string(SOURCE_COMMIT_FILE)
        .ok()
        .or_else(|| std::env::var("SOURCE_COMMIT").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// FNV-1a over source assets under the versioned directories of `dist_dirs`, in path
/// order: each file's path relative to `root`, length and bytes. Generated compression
/// sidecars are ignored; their gzip headers can change with build timestamps without
/// changing the asset. Hashing paths relative to `root` gives the same version to the same
/// assets wherever they are installed. `None` when no assets exist or traversal fails.
pub(crate) fn asset_content_version(root: &Path, dist_dirs: &[&Path]) -> Option<String> {
    let mut files = Vec::new();
    for dist in dist_dirs {
        for dir in VERSIONED_ASSET_DIRS {
            let path = root.join(dist).join(dir);
            if let Err(error) = collect_files(&path, &mut files) {
                // A distribution may omit an optional asset directory. Never hash a
                // partially enumerated directory as if it were a complete asset set.
                if error.kind() == std::io::ErrorKind::NotFound && !path.exists() {
                    continue;
                }
                tracing::warn!(path = %path.display(), %error, "cannot enumerate versioned assets");
                return None;
            }
        }
    }
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for path in &files {
        let Ok(contents) = std::fs::read(path) else {
            tracing::warn!(path = %path.display(), "unreadable versioned asset");
            return None;
        };
        let relative = path.strip_prefix(root).unwrap_or(path);
        feed(relative.to_string_lossy().as_bytes());
        feed(&(contents.len() as u64).to_le_bytes());
        feed(&contents);
    }
    Some(format!("{hash:016x}"))
}

fn collect_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        match entry.file_type()? {
            kind if kind.is_dir() => collect_files(&path, files)?,
            kind if kind.is_file()
                && !matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("br" | "gz")
                ) =>
            {
                files.push(path);
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::asset_content_version;

    #[test]
    fn asset_version_follows_the_versioned_files_only() {
        let root =
            std::env::temp_dir().join(format!("sequoia-asset-version-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("tiles")).expect("create tiles");
        std::fs::create_dir_all(dist.join("fonts")).expect("create fonts");
        std::fs::write(dist.join("tiles/main-1-1.webp"), b"tile").expect("write tile");
        std::fs::write(dist.join("fonts/font.woff2"), b"font").expect("write font");
        let version = || asset_content_version(Path::new(""), &[dist.as_path()]);

        let first = version().expect("assets present");
        // A new bundle or page leaves the asset version alone.
        std::fs::write(dist.join("index.html"), b"<html>").expect("write html");
        std::fs::write(dist.join("sequoia-client-0123456789abcdef.js"), b"js").expect("write js");
        assert_eq!(version().as_deref(), Some(first.as_str()));
        // Encoded variants are generated at build time; gzip timestamps must not bust
        // the canonical asset URL when its decoded contents have not changed.
        let encoded = dist.join("fonts/font.woff2.gz");
        std::fs::write(&encoded, b"encoded-at-build-one").expect("write encoding");
        assert_eq!(version().as_deref(), Some(first.as_str()));
        std::fs::write(&encoded, b"encoded-at-build-two").expect("rewrite encoding");
        assert_eq!(version().as_deref(), Some(first.as_str()));
        // Any changed or added asset changes it.
        std::fs::write(dist.join("tiles/main-1-1.webp"), b"tile2").expect("rewrite tile");
        let changed = version().expect("assets present");
        assert_ne!(changed, first);
        std::fs::write(dist.join("fonts/other.woff2"), b"font").expect("write font");
        assert_ne!(version().expect("assets present"), changed);

        assert_eq!(
            asset_content_version(Path::new(""), &[root.join("missing").as_path()]),
            None
        );
        // An existing but invalid asset directory must not produce a partial digest.
        std::fs::write(dist.join("icons"), b"not a directory").expect("write invalid icons");
        assert_eq!(version(), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
