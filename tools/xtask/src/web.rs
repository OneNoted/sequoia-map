//! Browser-client builds: Cargo for wasm32, wasm-bindgen, wasm-opt in release, the stylesheet,
//! public assets and the page that loads them.
//!
//! A client's `index.html` is an ordinary page whose `<link rel="stylesheet" href="input.css">`
//! names its stylesheet. The build writes `<client>/dist` with content-hashed JS, WASM and CSS,
//! points the page at them and adds the module preloads and the script that starts the app.
//! Release builds add integrity to each of those references, optimise the WASM, copy the
//! public assets and write Brotli and gzip copies; dev builds hard-link the assets and add
//! the watcher's reload script.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use sha2::{Digest, Sha256, Sha384};

use crate::{Args, run_diagnosed, run_status};

/// A browser client: its crate, where its page is served and how its stylesheet is built.
#[derive(Debug, PartialEq, Eq)]
pub struct Client {
    pub name: &'static str,
    pub dir: &'static str,
    package: &'static str,
    mount: &'static str,
    tailwind: bool,
}

pub const MAP: Client = Client {
    name: "map",
    dir: "client",
    package: "sequoia-client",
    mount: "/",
    tailwind: true,
};

pub const CLAIMS: Client = Client {
    name: "claims",
    dir: "claims-client",
    package: "sequoia-claims-client",
    mount: "/claims-app/",
    tailwind: false,
};

pub const CLIENTS: [&Client; 2] = [&MAP, &CLAIMS];

/// The public asset directories served beside each client.
const PUBLIC_DIRS: [&str; 3] = ["tiles", "icons", "fonts"];
/// The page's stylesheet reference in the source page.
const STYLESHEET_HREF: &str = "href=\"input.css\"";
/// Extensions the server can send precompressed, as the image always has.
const COMPRESSED: [&str; 6] = ["wasm", "js", "css", "html", "json", "svg"];
/// The map client's shipped WASM budget, compressed.
const BROTLI_BUDGET: usize = 1_900_000;
const GZIP_BUDGET: usize = 2_800_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Dev,
    Release,
}

impl Profile {
    fn dir(self) -> &'static str {
        match self {
            Profile::Dev => "debug",
            Profile::Release => "release",
        }
    }
}

/// The generated files a client's page references, by name within its `dist`.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub js: String,
    pub wasm: String,
    pub css: String,
}

/// `xtask web [--release] [map|claims]...`
pub fn command(root: &Path, args: &[String]) -> Result<()> {
    let args = Args::parse(args, &[], &["release"])?;
    let profile = if args.has("release") {
        Profile::Release
    } else {
        Profile::Dev
    };
    let clients = select(&args.positional)?;
    build(root, &clients, profile, None)?;
    Ok(())
}

/// The clients `names` selects; every client when there are none.
pub fn select(names: &[String]) -> Result<Vec<&'static Client>> {
    if names.is_empty() {
        return Ok(CLIENTS.to_vec());
    }
    names
        .iter()
        .map(|name| {
            CLIENTS
                .into_iter()
                .find(|client| client.name == name)
                .with_context(|| format!("unknown client {name}: expected map or claims"))
        })
        .collect()
}

/// Builds `clients` from scratch into their `dist` directories and returns their pages.
/// `reload_port` adds the dev watcher's reload script.
pub fn build(
    root: &Path,
    clients: &[&'static Client],
    profile: Profile,
    reload_port: Option<u16>,
) -> Result<Vec<Page>> {
    compile(root, clients, profile)?;
    clients
        .iter()
        .map(|client| {
            let dist = dist_dir(root, client, profile);
            if dist.exists() {
                fs::remove_dir_all(&dist).with_context(|| format!("clear {}", dist.display()))?;
            }
            fs::create_dir_all(&dist)?;
            link_public(root, client, profile)?;
            let (js, wasm) = bindgen(root, client, profile)?;
            let css = stylesheet(root, client, profile)?;
            let page = Page { js, wasm, css };
            write_page(root, client, profile, &page, reload_port)?;
            if profile == Profile::Release {
                compress(&dist)?;
                if client.tailwind {
                    check_size(&dist.join(&page.wasm))?;
                }
            }
            Ok(page)
        })
        .collect()
}

/// Where dev builds go, laid out like the release (`client/dist`, `claims-client/dist`); the dev
/// `api` daemon serves it through `SEQUOIA_CLIENT_ROOT`. Release builds keep `<client>/dist`, so
/// one never replaces the files the other is serving.
pub const DEV_ROOT: &str = ".data/dev-web";

pub fn dist_dir(root: &Path, client: &Client, profile: Profile) -> PathBuf {
    match profile {
        Profile::Dev => root.join(DEV_ROOT).join(client.dir).join("dist"),
        Profile::Release => root.join(client.dir).join("dist"),
    }
}

/// The tool's own working files for one profile, so a release build and the watcher never
/// share them.
fn scratch_dir(root: &Path, profile: Profile) -> PathBuf {
    target_dir(root).join("xtask").join(profile.dir())
}

/// Compiles the clients' WASM in one Cargo invocation, so shared crates build once.
pub fn compile(root: &Path, clients: &[&Client], profile: Profile) -> Result<()> {
    check_bindgen_version(root)?;
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo.current_dir(root).args([
        "build",
        "--locked",
        "--color",
        "never",
        "--target",
        "wasm32-unknown-unknown",
    ]);
    match profile {
        Profile::Release => {
            cargo.arg("--release");
        }
        // Leptos's type-erased views: a map edit recompiles in seconds instead of a minute.
        // Release builds keep the fully typed views.
        Profile::Dev => {
            cargo.args([
                "--config",
                r#"target.wasm32-unknown-unknown.rustflags=["--cfg","erase_components"]"#,
            ]);
        }
    }
    for client in clients {
        cargo.args(["-p", client.package]);
    }
    run_diagnosed(&mut cargo)
}

/// Generates the client's JS bindings from its compiled WASM and writes both under hashed
/// names, replacing the previous ones. Release builds run `wasm-opt -Oz` first.
pub fn bindgen(root: &Path, client: &Client, profile: Profile) -> Result<(String, String)> {
    let input = target_dir(root)
        .join("wasm32-unknown-unknown")
        .join(profile.dir())
        .join(format!("{}.wasm", client.package));
    let staging = scratch_dir(root, profile).join(client.name);
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&input)
        .out_name(client.package)
        .web(true)?
        .typescript(false)
        .generate(&staging)
        .with_context(|| format!("wasm-bindgen {}", input.display()))?;
    let mut wasm_path = staging.join(format!("{}_bg.wasm", client.package));
    if profile == Profile::Release {
        let optimised = staging.join("optimised.wasm");
        run_status(
            Command::new("wasm-opt")
                .arg(format!("--output={}", optimised.display()))
                .arg("-Oz")
                .arg(&wasm_path),
        )?;
        wasm_path = optimised;
    }
    let dist = dist_dir(root, client, profile);
    remove_generated(&dist, |name| {
        name.starts_with(&format!("{}-", client.package))
            && (name.ends_with(".js") || name.ends_with("_bg.wasm"))
    })?;
    let js = fs::read(staging.join(format!("{}.js", client.package)))?;
    let wasm = fs::read(&wasm_path)?;
    let js_name = format!("{}-{}.js", client.package, short_hash(&js));
    let wasm_name = format!("{}-{}_bg.wasm", client.package, short_hash(&wasm));
    fs::write(dist.join(&js_name), js)?;
    fs::write(dist.join(&wasm_name), wasm)?;
    let snippets = staging.join("snippets");
    if snippets.exists() {
        copy_tree(&snippets, &dist.join("snippets"), false)?;
    }
    Ok((js_name, wasm_name))
}

/// Builds the client's stylesheet (Tailwind for the map, plain CSS for claims) under a hashed
/// name, replacing the previous one.
pub fn stylesheet(root: &Path, client: &Client, profile: Profile) -> Result<String> {
    let source = root.join(client.dir);
    let css = if client.tailwind {
        compile_tailwind(&source, &scratch_dir(root, profile).join("tailwind.css"))?
    } else {
        fs::read(source.join("input.css"))?
    };
    let dist = dist_dir(root, client, profile);
    remove_generated(&dist, |name| {
        name.starts_with("input-") && name.ends_with(".css")
    })?;
    let name = stylesheet_name(&css);
    fs::write(dist.join(&name), css)?;
    Ok(name)
}

/// Compiles `<source>/input.css` with Tailwind through the scratch file `output`, resolving
/// its `@source` globs from `source`.
pub fn compile_tailwind(source: &Path, output: &Path) -> Result<Vec<u8>> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    run_diagnosed(
        Command::new("tailwindcss")
            .current_dir(source)
            .args(["-i", "input.css", "-o"])
            .arg(output),
    )?;
    let css = fs::read(output)?;
    fs::remove_file(output)?;
    Ok(css)
}

pub fn stylesheet_name(css: &[u8]) -> String {
    format!("input-{}.css", short_hash(css))
}

/// Puts the public asset directories into `dist`: hard links in dev, so nothing is copied
/// on each start, and independent copies in release.
pub fn link_public(root: &Path, client: &Client, profile: Profile) -> Result<()> {
    let public = root.join(client.dir).join("public");
    let dist = dist_dir(root, client, profile);
    for dir in PUBLIC_DIRS {
        let target = dist.join(dir);
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        copy_tree(&public.join(dir), &target, profile == Profile::Dev)?;
    }
    Ok(())
}

/// Writes the client's page: the source page with its stylesheet pointed at `page.css`, the
/// module preloads and the start script. Release references carry integrity.
pub fn write_page(
    root: &Path,
    client: &Client,
    profile: Profile,
    page: &Page,
    reload_port: Option<u16>,
) -> Result<()> {
    let source = root.join(client.dir).join("index.html");
    let template = fs::read_to_string(&source)?;
    let dist = dist_dir(root, client, profile);
    let integrity = |name: &str| -> Result<String> {
        Ok(match profile {
            Profile::Release => format!(" integrity=\"{}\"", sri(&fs::read(dist.join(name))?)),
            Profile::Dev => String::new(),
        })
    };
    let html = render_page(&template, client, page, &integrity, reload_port)
        .with_context(|| format!("render {}", source.display()))?;
    fs::write(dist.join("index.html"), html)?;
    Ok(())
}

fn render_page(
    template: &str,
    client: &Client,
    page: &Page,
    integrity: &dyn Fn(&str) -> Result<String>,
    reload_port: Option<u16>,
) -> Result<String> {
    ensure!(
        template.matches(STYLESHEET_HREF).count() == 1,
        "the page must link input.css exactly once"
    );
    let mount = client.mount;
    let stylesheet = format!(
        "href=\"{mount}{css}\"{integrity}",
        css = page.css,
        integrity = integrity(&page.css)?
    );
    let mut head = format!(
        "<link rel=\"modulepreload\" href=\"{mount}{js}\" crossorigin=\"anonymous\"{js_integrity}>\
         <link rel=\"preload\" href=\"{mount}{wasm}\" crossorigin=\"anonymous\"{wasm_integrity} \
         as=\"fetch\" type=\"application/wasm\">\n\
         <script type=\"module\">\n\
         import init from '{mount}{js}';\n\
         await init({{ module_or_path: '{mount}{wasm}' }});\n\
         </script>\n",
        js = page.js,
        wasm = page.wasm,
        js_integrity = integrity(&page.js)?,
        wasm_integrity = integrity(&page.wasm)?,
    );
    if let Some(port) = reload_port {
        head.push_str(&reload_script(client, port));
    }
    head.push_str("</head>");
    ensure!(
        template.matches("</head>").count() == 1,
        "the page must have one </head>"
    );
    Ok(template
        .replacen(STYLESHEET_HREF, &stylesheet, 1)
        .replacen("</head>", &head, 1))
}

/// The dev page's connection to the watcher: a stylesheet swaps in place, keeping the app's
/// state; a new build of this client or the server reloads; a failed build shows its error.
fn reload_script(client: &Client, port: u16) -> String {
    format!(
        r#"<script type="module">
const client = "{name}";
const events = new EventSource("http://127.0.0.1:{port}/events");
let banner;
events.addEventListener("css", (event) => {{
  const update = JSON.parse(event.data);
  if (update.client !== client) return;
  const old = document.querySelector('link[rel="stylesheet"]');
  const next = old.cloneNode();
  next.href = update.href;
  next.onload = () => old.remove();
  old.after(next);
  banner?.remove();
}});
events.addEventListener("reload", (event) => {{
  if (event.data === client || event.data === "all") location.reload();
}});
events.addEventListener("failed", (event) => {{
  banner ??= Object.assign(document.createElement("pre"), {{
    style: "position:fixed;inset:auto 0 0 0;max-height:40vh;overflow:auto;margin:0;padding:8px;z-index:2147483647;background:#2a0f12;color:#ffd7d7;font:12px monospace;white-space:pre-wrap",
  }});
  banner.textContent = event.data;
  document.body.append(banner);
}});
</script>
"#,
        name = client.name
    )
}

/// Writes Brotli (quality 11) and gzip (level 9, no timestamp) copies of every compressible
/// file, so repeated builds of the same bytes produce the same release.
fn compress(dist: &Path) -> Result<()> {
    let mut files = Vec::new();
    collect_files(dist, &mut files)?;
    files.retain(|path| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| COMPRESSED.contains(&ext))
    });
    std::thread::scope(|scope| {
        let tasks: Vec<_> = files
            .iter()
            .map(|path| {
                scope.spawn(move || -> Result<()> {
                    let data = fs::read(path)?;
                    fs::write(sidecar(path, "br"), brotli(&data)?)?;
                    fs::write(sidecar(path, "gz"), gzip(&data)?)?;
                    Ok(())
                })
            })
            .collect();
        tasks
            .into_iter()
            .try_for_each(|task| task.join().expect("compression thread panicked"))
    })
}

pub fn brotli(data: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 11, 22);
        writer.write_all(data)?;
    }
    Ok(output)
}

pub fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn sidecar(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

/// Fails when the map's shipped WASM is over its compressed budget.
fn check_size(wasm: &Path) -> Result<()> {
    let brotli = fs::metadata(sidecar(wasm, "br"))?.len() as usize;
    let gzip = fs::metadata(sidecar(wasm, "gz"))?.len() as usize;
    let raw = fs::metadata(wasm)?.len();
    eprintln!(
        "map WASM: {raw} bytes, brotli {brotli} (budget {BROTLI_BUDGET}), gzip {gzip} (budget {GZIP_BUDGET})"
    );
    ensure!(
        brotli <= BROTLI_BUDGET && gzip <= GZIP_BUDGET,
        "the map client's WASM is over its size budget"
    );
    Ok(())
}

/// The application and this tool must use the same wasm-bindgen release: the bindings
/// format is only guaranteed between identical versions. Both lockfiles say which.
fn check_bindgen_version(root: &Path) -> Result<()> {
    let app = locked_versions(
        &fs::read_to_string(root.join("Cargo.lock"))?,
        "wasm-bindgen",
    );
    let tool = locked_versions(
        &fs::read_to_string(root.join("tools/xtask/Cargo.lock"))?,
        "wasm-bindgen-cli-support",
    );
    if app.len() != 1 || app != tool {
        bail!(
            "the application locks wasm-bindgen {app:?} but xtask builds with wasm-bindgen-cli-support \
             {tool:?}; pin tools/xtask's wasm-bindgen-cli-support to the application's version"
        );
    }
    Ok(())
}

fn locked_versions(lock: &str, package: &str) -> Vec<String> {
    let name = format!("name = \"{package}\"");
    let mut lines = lock.lines();
    let mut versions = Vec::new();
    while let Some(line) = lines.next() {
        if line == name
            && let Some(version) = lines
                .next()
                .and_then(|line| line.strip_prefix("version = \""))
                .and_then(|rest| rest.strip_suffix('"'))
        {
            versions.push(version.to_owned());
        }
    }
    versions
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

fn short_hash(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn sri(data: &[u8]) -> String {
    format!(
        "sha384-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha384::digest(data))
    )
}

fn remove_generated(dist: &Path, generated: impl Fn(&str) -> bool) -> Result<()> {
    for entry in fs::read_dir(dist)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(&generated) {
            fs::remove_file(entry.path())?;
            for extension in ["br", "gz"] {
                let _ = fs::remove_file(sidecar(&entry.path(), extension));
            }
        }
    }
    Ok(())
}

/// Copies `from` into `to` recursively, or hard-links each file when `link` is set.
fn copy_tree(from: &Path, to: &Path, link: bool) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if fs::metadata(&source)?.is_dir() {
            copy_tree(&source, &target, link)?;
        } else if !link || fs::hard_link(&source, &target).is_err() {
            fs::copy(&source, &target).with_context(|| format!("copy {}", source.display()))?;
        }
    }
    Ok(())
}

pub fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Page {
        Page {
            js: "app-1.js".into(),
            wasm: "app-1_bg.wasm".into(),
            css: "input-2.css".into(),
        }
    }

    #[test]
    fn release_page_references_carry_integrity_under_the_client_mount() {
        let template = "<head><link rel=\"stylesheet\" href=\"input.css\" /><style></style></head>";
        let html = render_page(
            template,
            &CLAIMS,
            &page(),
            &|name| Ok(format!(" integrity=\"{name}\"")),
            None,
        )
        .expect("render");
        assert!(html.contains(
            "<link rel=\"stylesheet\" href=\"/claims-app/input-2.css\" integrity=\"input-2.css\" /><style>"
        ));
        assert!(html.contains(
            "href=\"/claims-app/app-1.js\" crossorigin=\"anonymous\" integrity=\"app-1.js\""
        ));
        assert!(html.contains("module_or_path: '/claims-app/app-1_bg.wasm'"));
        assert!(!html.contains("EventSource"));
    }

    #[test]
    fn a_page_without_exactly_one_stylesheet_reference_is_refused() {
        let none = render_page("<head></head>", &MAP, &page(), &|_| Ok(String::new()), None);
        assert!(none.is_err());
    }

    #[test]
    fn a_lockfile_names_each_locked_version() {
        let lock = "[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.113\"\n\n\
                    [[package]]\nname = \"wasm-bindgen-shared\"\nversion = \"0.2.113\"\n";
        assert_eq!(locked_versions(lock, "wasm-bindgen"), ["0.2.113"]);
    }
}
