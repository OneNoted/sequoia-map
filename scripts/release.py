# /// script
# requires-python = ">=3.13"
# dependencies = ["brotli==1.2.0"]
# ///
"""Plan, build, activate and roll back releases.

    release.py plan [--base REV] [--head REV]
    release.py key [--commit REV]
    release.py client [--base-release DIR | --from-target] [--out DIR]
    release.py activate BUNDLE [--dry-run]
    release.py status
    release.py rollback [--dry-run]
    release.py services SERVICE... [--dry-run]

`plan` says which artifacts a change needs: a CSS-only client release, a rebuilt client
release, or new service images. `client` builds a client release bundle. When the only
change since its base release is client/input.css, it reuses every other byte of the base
release and swaps the compiled stylesheet, its integrity and its compressed copies;
otherwise it runs the real Trunk builds. `activate` installs a bundle on the target, makes
it current and checks the served page, rolling back if that check fails.

The target is the server container: SEQUOIA_RELEASE_EXEC is the command prefix that runs a
command in it (for example `docker compose -p map exec -T server`), and SEQUOIA_RELEASE_URL
is the public base URL. `services` applies already-published server, ingest or edge images
through SEQUOIA_COMPOSE_EXEC, the `docker compose` command prefix for the deployed project
and its existing configuration; it pulls and recreates only the services named. Every stage
appends a timed receipt to .data/releases/receipts.jsonl.
"""

import argparse
import base64
import fnmatch
import gzip
import hashlib
import io
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / ".data" / "releases"
SERVER = "/app/sequoia-server"
CLIENTS = (("client/dist", "/"), ("claims-client/dist", "/claims-app/"))
BACKEND_PATHS = ("Cargo.toml", "Cargo.lock", "shared", "server")
COMPRESSED = (".wasm", ".js", ".css", ".html", ".json", ".svg")
ALL_SERVICES = ("server", "ingest", "edge")
# The activation check names itself: the public edge refuses Python's default User-Agent.
USER_AGENT = "SequoiaMapRelease/1.0 (+https://github.com/OneNoted/sequoia-map)"

# What each changed path needs, first match wins: the static client release that can deliver
# it on its own ("css" reuses everything but the stylesheet, "client" rebuilds both clients;
# None when only an image can), the images a normal deploy rebuilds, and whether the platform's
# compose configuration changes. Server images embed both clients, so client paths also name
# the server image: a static release delivers them sooner, and the next server image carries
# them. A path no rule names needs everything.
RULES = (
    ("client/input.css", "css", ("server",), False),
    ("client/*", "client", ("server",), False),
    ("claims-client/*", "client", ("server",), False),
    ("browser-map/*", "client", ("server",), False),
    ("wasm/*", "client", ("server",), False),
    ("Cargo.toml", None, ("server", "ingest"), False),
    ("shared/*", None, ("server", "ingest"), False),
    ("Cargo.lock", None, ("server",), False),
    ("Dockerfile", None, ("server",), False),
    (".ci/*", None, ("server",), False),
    ("server/*", None, ("server",), False),
    ("services/sequoia-ingest/*", None, ("ingest",), False),
    ("ops/caddy/*", None, ("edge",), False),
    (".dockerignore", None, ALL_SERVICES, False),
    ("docker-compose.coolify.yml", None, ALL_SERVICES, True),
    (".github/workflows/deploy-images.yml", None, ALL_SERVICES, False),
    ("docker-compose.yml", None, (), True),
    *((pattern, None, (), False) for pattern in (
        "README.md", "AGENTS.md", "LICENSE", "docs/*", ".gitignore", ".cargo/audit.toml",
        "mise.toml", ".github/workflows/ci.yml", ".github/actionlint.yaml",
        "docker-compose.coolify.dev.yml", "scripts/*", "ops/postgres/*", "ops/backup/*",
        "ops/prometheus/*")),
)
UNKNOWN = (None, ALL_SERVICES, True)


class ReleaseFailure(Exception):
    """A refused or failed release step; the message says which and why."""


def git(*args, binary=False):
    result = subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, check=False)
    if result.returncode:
        raise ReleaseFailure(f"git {' '.join(args)}: {result.stderr.decode().strip()}")
    return result.stdout if binary else result.stdout.decode().strip()


def resolve(rev):
    return git("rev-parse", "--verify", f"{rev}^{{commit}}")


def changed_paths(base, head):
    return [path for path in git("diff", "--name-only", "--no-renames", base, head).splitlines() if path]


def needs(path):
    """The rule for `path`, or None when no rule names it."""
    return next(((static, images, config) for pattern, static, images, config in RULES
                 if fnmatch.fnmatchcase(path, pattern)), None)


def classify(paths):
    """What delivering a change to `paths` needs. A static release suffices when every path
    either needs nothing or is a client path a static release delivers."""
    rules = {path: needs(path) for path in paths}
    unknown = [path for path, rule in rules.items() if rule is None]
    effects = [rule or UNKNOWN for rule in rules.values()]
    statics = {static for static, _, _ in effects if static}
    client = "client" if "client" in statics else "css" if statics else None
    images = [service for service in ALL_SERVICES if any(service in rule[1] for rule in effects)]
    static_suffices = client is not None and all(
        static or (not rule_images and not rule_config) for static, rule_images, rule_config in effects)
    return {"paths": paths, "unknown": unknown, "client_release": client, "images": images,
            "config": any(rule[2] for rule in effects), "static_release_suffices": static_suffices}


def plan(base, head):
    """What delivering `head` over `base` needs. A static release suffices when only clients
    change and the server's backend is the same."""
    needed = classify(changed_paths(base, head))
    needed["static_release_suffices"] &= backend_key(base) == backend_key(head)
    return {"base": base, "head": head, **needed}


def css_only(base_source, head):
    """Whether delivering `head` over the commit a release was built from needs only a new
    stylesheet, so every other byte of that release can be reused, and if not, why. A base
    release whose commit is empty or not in this repository cannot be compared, so the full
    build, which does not need it, runs instead."""
    if not base_source:
        return False, "the base release records no source commit"
    try:
        base = resolve(base_source)
    except ReleaseFailure:
        return False, f"the base release's commit {base_source} is not in this repository"
    needed = classify(changed_paths(base, head))
    if needed["client_release"] == "css" and needed["static_release_suffices"]:
        return True, None
    return False, "the change since the base release needs more than a stylesheet"


def backend_key(commit):
    """The server's backend key for `commit`, as the image build computes it: the digest of
    `sha256sum` lines for every file of the backend closure in byte order."""
    listing = git("ls-tree", "-r", "-z", commit, "--", *BACKEND_PATHS, binary=True)
    entries = sorted(
        (path, oid)
        for line in listing.split(b"\0") if line
        for meta, path in [line.split(b"\t", 1)]
        for mode, kind, oid in [meta.split(b" ")]
        if kind == b"blob" and mode != b"120000"
    )
    batch = subprocess.run(
        ["git", "-C", str(ROOT), "cat-file", "--batch"],
        input=b"".join(oid + b"\n" for _, oid in entries),
        capture_output=True, check=True,
    ).stdout
    lines, offset = [], 0
    for path, _ in entries:
        header_end = batch.index(b"\n", offset)
        size = int(batch[offset:header_end].split(b" ")[2])
        content = batch[header_end + 1:header_end + 1 + size]
        offset = header_end + 1 + size + 1
        lines.append(hashlib.sha256(content).hexdigest().encode() + b"  " + path + b"\n")
    return hashlib.sha256(b"".join(lines)).hexdigest()[:16]


def dockerfile_version(name):
    match = re.search(rf"^ARG {name}=(\S+)$", (ROOT / "Dockerfile").read_text(), re.MULTILINE)
    if not match:
        raise ReleaseFailure(f"Dockerfile has no {name}")
    return match.group(1)


def require_tool_version(command, expected, label):
    output = subprocess.run(command, capture_output=True, text=True, check=False)
    text = output.stdout + output.stderr
    if expected not in text:
        raise ReleaseFailure(f"{label} must be {expected}, the image's version, not: {text.strip()[:80]}")


def digest(path):
    data = path.read_bytes()
    return {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data)}


def tree(root):
    return {
        path.relative_to(root).as_posix(): digest(path)
        for path in sorted(root.rglob("*"))
        if path.is_file() and path.relative_to(root).as_posix() != "release.json"
    }


def compress(path):
    import brotli  # Only bundle builds need it; `plan` runs on a bare Python.

    data = path.read_bytes()
    path.with_name(path.name + ".br").write_bytes(brotli.compress(data, quality=11))
    path.with_name(path.name + ".gz").write_bytes(gzip.compress(data, compresslevel=9, mtime=0))


def sri(data):
    return "sha384-" + base64.b64encode(hashlib.sha384(data).digest()).decode()


def write_manifest(bundle, head):
    manifest = {"source_commit": head, "backend_key": backend_key(head), "files": tree(bundle)}
    (bundle / "release.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def build_css_release(base_release, bundle, head):
    """Reuse every file of `base_release` and replace only the compiled stylesheet."""
    require_tool_version(["tailwindcss", "--help"], "v" + dockerfile_version("TAILWINDCSS_VERSION"),
                         "tailwindcss")
    shutil.copytree(base_release, bundle, ignore=shutil.ignore_patterns("release.json"))
    dist = bundle / "client" / "dist"
    page = (dist / "index.html").read_text()
    tags = [tag for tag in re.findall(r"<link\b[^>]*>", page) if 'rel="stylesheet"' in tag]
    if len(tags) != 1:
        raise ReleaseFailure("the base release's map page must link exactly one stylesheet")
    old_name = re.search(r'href="/([^"]+\.css)"', tags[0]).group(1)
    with tempfile.TemporaryDirectory() as scratch:
        compiled = Path(scratch) / "input.css"
        subprocess.run(["tailwindcss", "-i", "input.css", "-o", str(compiled)], cwd=ROOT / "client",
                       check=True, capture_output=True)
        css = compiled.read_bytes()
    new_name = f"input-{hashlib.sha256(css).hexdigest()[:16]}.css"
    old_integrity = re.search(r'integrity="([^"]+)"', tags[0]).group(1)
    tag = tags[0].replace(old_name, new_name).replace(old_integrity, sri(css))
    for suffix in ("", ".br", ".gz"):
        (dist / (old_name + suffix)).unlink(missing_ok=True)
    (dist / new_name).write_bytes(css)
    (dist / "index.html").write_text(page.replace(tags[0], tag))
    for name in (new_name, "index.html"):
        compress(dist / name)
    before, after = tree(base_release), tree(bundle)
    touched = {f"client/dist/{name}{suffix}" for name in (old_name, new_name, "index.html")
               for suffix in ("", ".br", ".gz")}
    reused = [path for path in after if path not in touched]
    if any(before.get(path) != after[path] for path in reused):
        raise ReleaseFailure("a CSS-only release must reuse every other file byte for byte")
    return {"stylesheet": new_name, "replaced": old_name, "reused_files": len(reused)}


def build_full_release(bundle):
    """Build both clients once with the pinned Trunk, then compress them like the image."""
    require_tool_version(["trunk", "--version"], dockerfile_version("TRUNK_VERSION"), "trunk")
    require_tool_version(["tailwindcss", "--help"], "v" + dockerfile_version("TAILWINDCSS_VERSION"),
                         "tailwindcss")
    for client, _ in CLIENTS:
        project = ROOT / client.split("/")[0]
        # Build output goes to stderr: stdout carries only the command's JSON result.
        subprocess.run(["trunk", "build", "--release", "--locked"], cwd=project, check=True,
                       stdout=sys.stderr)
        shutil.copytree(project / "dist", bundle / client)
        for path in sorted((bundle / client).rglob("*")):
            if path.is_file() and path.suffix in COMPRESSED:
                compress(path)
    return {"built": [client for client, _ in CLIENTS]}


class Target:
    """The server container to release into, and its public URL."""

    def __init__(self, exec_prefix, url):
        if not exec_prefix:
            raise ReleaseFailure("set SEQUOIA_RELEASE_EXEC or pass --exec")
        self.exec_prefix = shlex.split(exec_prefix)
        self.url = (url or "").rstrip("/")

    def run(self, *args, stdin=None):
        result = subprocess.run([*self.exec_prefix, SERVER, "client-release", *args],
                                input=stdin, capture_output=True, check=False)
        if result.returncode:
            raise ReleaseFailure(f"client-release {args[0]}: {result.stderr.decode().strip()}")
        return result.stdout

    def json(self, *args, stdin=None):
        return json.loads(self.run(*args, stdin=stdin))

    def status(self):
        return self.json("status")


class Compose:
    """The deployed Compose project: a `docker compose` command prefix that already names its
    project, files and environment, which this tool never reads."""

    def __init__(self, prefix):
        if not prefix:
            raise ReleaseFailure("set SEQUOIA_COMPOSE_EXEC or pass --compose")
        self.prefix = shlex.split(prefix)

    def run(self, *args):
        result = subprocess.run([*self.prefix, *args], capture_output=True, text=True, check=False)
        if result.returncode:
            raise ReleaseFailure(f"compose {args[0]}: {result.stderr.strip()[-600:]}")
        return result.stdout + result.stderr

    def rows(self, *args):
        output = self.run(*args, "--format", "json").strip()
        if output.startswith("["):
            return json.loads(output)
        return [json.loads(line) for line in output.splitlines() if line.startswith("{")]

    def identities(self):
        """Each service's container, image and state, for every service in the project."""
        images = {row.get("ContainerName"): row.get("ID") for row in self.rows("images")}
        return {
            row["Service"]: {"container": row.get("ID", "")[:12], "image": row.get("Image"),
                             "image_id": (images.get(row.get("Name")) or "")[:19],
                             "state": row.get("State"), "health": row.get("Health", "")}
            for row in self.rows("ps", "--all")
        }


def apply_services(compose, services, dry_run, receipts):
    """Pull the named services' images and let Compose recreate only those whose image or
    configuration changed, without touching their dependencies."""
    before = receipts.stage("before", compose.identities)
    missing = [service for service in services if service not in before]
    if missing:
        raise ReleaseFailure(f"the project has no {', '.join(missing)} service")
    up = ["up", "--detach", "--no-deps", "--wait", "--pull", "never", *services]
    if dry_run:
        plan = receipts.stage("dry-run", lambda: compose.run("--dry-run", "pull", *services)
                              + compose.run("--dry-run", *up))
        return {"dry_run": True, "services": services, "compose": plan.strip().splitlines(),
                "before": before}
    receipts.stage("pull", compose.run, "pull", *services)
    receipts.stage("up", compose.run, *up)
    after = receipts.stage("after", compose.identities)
    replaced = [service for service in after if before.get(service, {}).get("container")
                != after[service]["container"]]
    unrelated = [service for service in replaced if service not in services]
    if unrelated:
        raise ReleaseFailure(f"services outside the selection were replaced: {', '.join(unrelated)}")
    unhealthy = [service for service in services if after[service]["state"] != "running"
                 or after[service]["health"] not in ("", "healthy")]
    if unhealthy:
        raise ReleaseFailure(f"not running and healthy after the update: {', '.join(unhealthy)}")
    return {"services": services, "replaced": replaced,
            "unchanged": [service for service in after if service not in replaced],
            "before": before, "after": after}


def bundle_tar(bundle):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(bundle.rglob("*")):
            info = archive.gettarinfo(path, path.relative_to(bundle).as_posix())
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            if path.is_file():
                with path.open("rb") as handle:
                    archive.addfile(info, handle)
            else:
                archive.addfile(info)
    return buffer.getvalue()


def fetch(url):
    request = urllib.request.Request(url, headers={"Accept-Encoding": "identity", "User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.status, response.read()


def verify_served(target, manifest, asset_version):
    """The served pages are the activated release's and every integrity-checked asset loads."""
    if not target.url:
        raise ReleaseFailure("set SEQUOIA_RELEASE_URL or pass --url to check the served release")
    checked = []
    for (client, mount), page in zip(CLIENTS, ("/", "/claims/new"), strict=True):
        status, body = fetch(target.url + page)
        html = body.decode()
        if status != 200:
            raise ReleaseFailure(f"{page} answered {status}")
        references = re.findall(r'<(?:link|script)\b[^>]*?(?:href|src)="([^"]+)"[^>]*?integrity="([^"]+)"', html)
        if client == "client/dist" and not references:
            raise ReleaseFailure("the map page has no integrity-checked assets")
        for href, integrity in references:
            name = client + "/" + href.removeprefix(mount)
            if name not in manifest["files"]:
                raise ReleaseFailure(f"{page} references {href}, which is not in the release")
            _, data = fetch(target.url + href)
            if sri(data) != integrity:
                raise ReleaseFailure(f"{href} does not match its integrity")
            checked.append(href)
        if client == "client/dist" and asset_version and f"?v={asset_version}" not in html:
            raise ReleaseFailure("the map page does not carry the release's asset version")
    status, _ = fetch(target.url + "/api/health")
    if status != 200:
        raise ReleaseFailure(f"/api/health answered {status}")
    return checked


class Receipts:
    """Timed stage receipts, appended as JSON lines."""

    def __init__(self, action):
        self.action = action
        DATA.mkdir(parents=True, exist_ok=True)

    def stage(self, name, function, *args, **kwargs):
        started = time.time()
        try:
            detail = function(*args, **kwargs)
            self.write(name, started, "ok", detail)
            return detail
        except Exception as error:
            self.write(name, started, "failed", str(error))
            raise

    def write(self, name, started, outcome, detail):
        record = {"action": self.action, "stage": name, "outcome": outcome,
                  "started": time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(started)) + "Z",
                  "seconds": round(time.time() - started, 3), "detail": detail}
        with (DATA / "receipts.jsonl").open("a") as receipts:
            receipts.write(json.dumps(record, default=str) + "\n")
        print(json.dumps(record, default=str), file=sys.stderr)


def command_client(options):
    if git("status", "--porcelain", "--untracked-files=no"):
        raise ReleaseFailure("commit or stash changes first: a release is built from a commit")
    head = resolve("HEAD")
    receipts = Receipts("client")
    with tempfile.TemporaryDirectory() as scratch:
        base = options.base_release
        if options.from_target:
            target = Target(options.exec, options.url)
            current = target.status()["current"]
            if not current:
                raise ReleaseFailure("the target has no current release to build on")
            base = Path(scratch) / "base"
            base.mkdir()

            def fetch_base():
                with tarfile.open(fileobj=io.BytesIO(target.run("export", current))) as archive:
                    archive.extractall(base, filter="data")
                return {"release": current}

            receipts.stage("fetch-base", fetch_base)
        bundle = Path(options.out or DATA / f"bundle-{head[:12]}")
        if bundle.exists():
            shutil.rmtree(bundle)
        base_source = json.loads((Path(base) / "release.json").read_text())["source_commit"] if base else None
        reuse, full_build_reason = css_only(base_source, head) if base else (False, "no base release")
        if reuse:
            detail = receipts.stage("build-css", build_css_release, Path(base), bundle, head)
        else:
            detail = receipts.stage("build-clients", build_full_release, bundle)
        manifest = write_manifest(bundle, head)
        result = {"bundle": str(bundle), "source_commit": head, "backend_key": manifest["backend_key"],
                  "kind": "css" if reuse else "full", "base_source": base_source,
                  "full_build_reason": full_build_reason, **detail}
        print(json.dumps(result, indent=2))


def command_activate(options):
    target = Target(options.exec, options.url)
    bundle = Path(options.bundle)
    manifest = json.loads((bundle / "release.json").read_text())
    receipts = Receipts("activate")
    before = receipts.stage("status", target.status)
    expected = before["current"] or "none"
    if before["server_backend_key"] != manifest["backend_key"]:
        raise ReleaseFailure(
            f"the bundle was built for backend {manifest['backend_key']}; the target runs "
            f"{before['server_backend_key']}. Deliver this change with a server release.")
    if options.dry_run:
        print(json.dumps({"would_install": str(bundle), "replacing": before["current"],
                          "files": len(manifest["files"])}, indent=2))
        return
    installed = receipts.stage("install", target.json, "install", stdin=bundle_tar(bundle))
    release = installed["id"]
    activation = receipts.stage("activate", target.json, "activate", release, "--expect", expected)
    status = target.status()
    version = next(entry["asset_version"] for entry in status["releases"] if entry["id"] == release)
    try:
        checked = receipts.stage("verify", verify_served, target, manifest, version)
    except Exception as error:
        if activation["changed"]:
            receipts.stage("rollback", target.json, "rollback", "--expect", release)
        raise ReleaseFailure(f"the served release failed its check and was rolled back: {error}") from error
    print(json.dumps({"current": release, "previous": activation["previous"],
                      "changed": activation["changed"], "checked_assets": checked,
                      "pruned": activation["pruned"]}, indent=2))


def command_rollback(options):
    target = Target(options.exec, options.url)
    receipts = Receipts("rollback")
    before = receipts.stage("status", target.status)
    if options.dry_run:
        print(json.dumps({"current": before["current"], "would_activate": before["rollback_target"]}, indent=2))
        return
    activation = receipts.stage("rollback", target.json, "rollback", "--expect", before["current"] or "none")
    print(json.dumps(activation, indent=2))


def command_services(options):
    services = sorted(set(options.service), key=ALL_SERVICES.index)
    result = apply_services(Compose(options.compose), services, options.dry_run, Receipts("services"))
    print(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    plan_parser = commands.add_parser("plan", help="say what delivering a change needs")
    plan_parser.add_argument("--base", default="origin/main")
    plan_parser.add_argument("--head", default="HEAD")
    key_parser = commands.add_parser("key", help="print a commit's backend key, as its image records it")
    key_parser.add_argument("--commit", default="HEAD")
    client_parser = commands.add_parser("client", help="build a client release bundle")
    source = client_parser.add_mutually_exclusive_group()
    source.add_argument("--base-release", type=Path, help="an exported release to build on")
    source.add_argument("--from-target", action="store_true", help="build on the target's current release")
    client_parser.add_argument("--out", type=Path)
    activate_parser = commands.add_parser("activate", help="install, activate and check a bundle")
    activate_parser.add_argument("bundle", type=Path)
    activate_parser.add_argument("--dry-run", action="store_true")
    services_parser = commands.add_parser(
        "services", help="pull and update only the named service images, leaving the rest running")
    services_parser.add_argument("service", nargs="+", choices=ALL_SERVICES)
    services_parser.add_argument("--dry-run", action="store_true")
    services_parser.add_argument("--compose", default=os.environ.get("SEQUOIA_COMPOSE_EXEC"))
    commands.add_parser("status", help="show the target's releases")
    rollback_parser = commands.add_parser("rollback", help="activate the previous release")
    rollback_parser.add_argument("--dry-run", action="store_true")
    for sub in (client_parser, activate_parser, rollback_parser, commands.choices["status"]):
        sub.add_argument("--exec", default=os.environ.get("SEQUOIA_RELEASE_EXEC"))
        sub.add_argument("--url", default=os.environ.get("SEQUOIA_RELEASE_URL"))
    options = parser.parse_args()
    try:
        match options.command:
            case "plan":
                print(json.dumps(plan(resolve(options.base), resolve(options.head)), indent=2))
            case "key":
                print(backend_key(resolve(options.commit)))
            case "client":
                command_client(options)
            case "activate":
                command_activate(options)
            case "status":
                print(json.dumps(Target(options.exec, options.url).status(), indent=2))
            case "rollback":
                command_rollback(options)
            case "services":
                command_services(options)
    except ReleaseFailure as error:
        print(f"release: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
