# /// script
# requires-python = ">=3.13"
# dependencies = ["brotli==1.2.0"]
# ///
"""Tests for scripts/release.py: the backend key, change selection and the CSS-only path."""

import argparse
import base64
import contextlib
import gzip
import hashlib
import http.server
import importlib.util
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from pathlib import Path

import brotli

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts" / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class BackendKey(unittest.TestCase):
    def test_matches_the_image_build_for_the_same_commit(self):
        dockerfile = (ROOT / "Dockerfile").read_text()
        command = re.search(r"RUN (find Cargo\.toml.*?)> /app/backend-key", dockerfile, re.DOTALL)
        self.assertIsNotNone(command, "the Dockerfile computes the backend key")
        command = command.group(1).replace("\\\n", " ")
        head = release.resolve("HEAD")
        with tempfile.TemporaryDirectory() as scratch:
            archive = subprocess.run(["git", "-C", str(ROOT), "archive", head, *release.BACKEND_PATHS],
                                     capture_output=True, check=True).stdout
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                tar.extractall(scratch, filter="data")
            image_key = subprocess.run(["sh", "-c", command], cwd=scratch, capture_output=True,
                                       text=True, check=True).stdout.strip()
        self.assertEqual(release.backend_key(head), image_key)

    def test_the_key_command_prints_the_backend_key(self):
        printed = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/release.py"), "key", "--commit", "ee8e674"],
                                 capture_output=True, text=True, check=True).stdout.strip()
        self.assertEqual(printed, release.backend_key(release.resolve("ee8e674")))

    def test_changes_only_with_the_backend(self):
        # ee8e674 changed only client/input.css over 4bc3421; fecedbc changed only client files.
        self.assertEqual(release.backend_key("4bc3421"), release.backend_key("ee8e674"))
        self.assertEqual(release.backend_key("4255ce9"), release.backend_key("fecedbc"))
        self.assertNotEqual(release.backend_key("ee8e674"), release.backend_key("HEAD"))


class Selection(unittest.TestCase):
    def check(self, paths, client, images, static, config=False, unknown=()):
        needed = release.classify(paths)
        self.assertEqual((needed["client_release"], needed["images"], needed["static_release_suffices"],
                          needed["config"], needed["unknown"]),
                         (client, list(images), static, config, list(unknown)), paths)

    def test_realistic_changes(self):
        self.check([], None, [], False)
        self.check(["docs/deployment.md", "mise.toml"], None, [], False)
        self.check(["client/input.css"], "css", ["server"], True)
        self.check(["client/input.css", "docs/deployment.md", "scripts/release.py"], "css", ["server"], True)
        self.check(["client/input.css", "client/src/sidebar.rs"], "client", ["server"], True)
        self.check(["browser-map/src/gpu/mod.rs", "wasm/src/camera.rs"], "client", ["server"], True)
        self.check(["server/src/app.rs"], None, ["server"], False)
        self.check(["server/src/app.rs", "client/input.css"], "css", ["server"], False)
        self.check(["shared/src/territory.rs", "client/input.css"], "css", ["server", "ingest"], False)
        self.check(["Cargo.lock", "client/src/main.rs"], "client", ["server"], False)
        self.check(["Dockerfile"], None, ["server"], False)
        self.check(["services/sequoia-ingest/src/main.rs"], None, ["ingest"], False)
        self.check(["client/input.css", "ops/caddy/Caddyfile"], "css", ["server", "edge"], False)
        self.check(["docker-compose.coolify.yml"], None, ["server", "ingest", "edge"], False, config=True)
        self.check(["client/input.css", "docker-compose.yml"], "css", ["server"], False, config=True)
        self.check(["client/input.css", "tools/new.sh"], "css", ["server", "ingest", "edge"], False,
                   config=True, unknown=["tools/new.sh"])

    def test_history(self):
        brackets = release.plan(release.resolve("4bc3421"), release.resolve("ee8e674"))
        self.assertEqual((brackets["client_release"], brackets["static_release_suffices"]), ("css", True))
        controls = release.plan(release.resolve("4255ce9"), release.resolve("fecedbc"))
        self.assertEqual((controls["client_release"], controls["static_release_suffices"]), ("client", True))
        server = release.plan(release.resolve("ee8e674"), release.resolve("HEAD"))
        self.assertIn("server", server["images"])
        self.assertFalse(server["static_release_suffices"])

    def test_css_reuse_needs_a_stylesheet_only_difference(self):
        self.assertEqual(release.css_only("4bc3421", release.resolve("ee8e674")), (True, None))
        self.assertFalse(release.css_only("4255ce9", release.resolve("fecedbc"))[0])
        self.assertFalse(release.css_only("ee8e674", release.resolve("ee8e674"))[0])
        # Tooling and docs between a release and its stylesheet change do not change its bytes.
        self.assertTrue(release.classify(["client/input.css", "scripts/release.py", "docs/x.md"])
                        ["client_release"] == "css")
        self.assertFalse(release.css_only("ee8e674", release.resolve("HEAD"))[0], "server changes rebuild")

    def test_an_unusable_base_commit_falls_back_to_the_full_build(self):
        head = release.resolve("HEAD")
        for source in ("", None, "0" * 40):
            reusable, reason = release.css_only(source, head)
            self.assertFalse(reusable, source)
            self.assertTrue(reason, source)

    def test_the_client_command_builds_in_full_on_an_empty_base_commit(self):
        with tempfile.TemporaryDirectory() as scratch:
            base, out = Path(scratch) / "base", Path(scratch) / "bundle"
            base.mkdir()
            (base / "release.json").write_text('{"source_commit": ""}')
            built = []

            def full_build(bundle):
                built.append(bundle)
                (bundle / "client" / "dist").mkdir(parents=True)
                (bundle / "client" / "dist" / "index.html").write_text("<p>map</p>")
                return {"built": ["stub"]}

            git = release.git
            originals = release.build_full_release, release.git, release.DATA
            release.build_full_release, release.DATA = full_build, Path(scratch) / "receipts"
            release.git = lambda *args, **kwargs: "" if args[0] == "status" else git(*args, **kwargs)
            output = io.StringIO()
            try:
                with contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
                    release.command_client(argparse.Namespace(base_release=base, from_target=False, out=out))
            finally:
                release.build_full_release, release.git, release.DATA = originals
            result = json.loads(output.getvalue())
            self.assertEqual(built, [out])
            self.assertEqual((result["kind"], result["full_build_reason"]),
                             ("full", "the base release records no source commit"))


class ServedCheck(unittest.TestCase):
    def test_requests_name_the_release_client(self):
        seen = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append((self.path, self.headers.get("User-Agent"), self.headers.get("Accept-Encoding")))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"ok")

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            status, body = release.fetch(f"http://127.0.0.1:{server.server_port}/api/health")
        finally:
            server.shutdown()
        self.assertEqual((status, body), (200, b"ok"))
        self.assertEqual(seen, [("/api/health", release.USER_AGENT, "identity")])


class Services(unittest.TestCase):
    """`services` through a stand-in compose prefix that records its calls and answers `ps` and
    `images` from the project state before and after `up`."""

    def project(self, scratch, after):
        state = Path(scratch)
        rows = {"postgres": "p1", "server": "s1", "ingest": "i1", "edge": "e1", "postgres-backup": "b1"}
        for name, containers in (("before", rows), ("after", {**rows, **after})):
            (state / f"ps-{name}.json").write_text("\n".join(json.dumps(
                {"Service": service, "Name": f"map-{service}-1", "ID": container, "Image": f"img/{service}",
                 "State": "running", "Health": "healthy"}) for service, container in containers.items()))
            (state / f"images-{name}.json").write_text(json.dumps([
                {"ContainerName": f"map-{service}-1", "ID": f"sha256:{container}"} for service, container in containers.items()]))
        fake = state / "compose"
        fake.write_text(f"""#!{sys.executable}
import pathlib, sys
state = pathlib.Path({str(state)!r})
with (state / "calls").open("a") as calls:
    calls.write(" ".join(sys.argv[1:]) + "\\n")
args = sys.argv[3:]
phase = "after" if (state / "up").exists() else "before"
if args[0] == "up":
    (state / "up").write_text("")
elif args[0] in ("ps", "images"):
    print((state / f"{{args[0]}}-{{phase}}.json").read_text())
""")
        fake.chmod(0o755)
        return release.Compose(f"{fake} -p map"), state

    def receipts(self, scratch):
        data, release.DATA = release.DATA, Path(scratch) / "receipts"
        self.addCleanup(setattr, release, "DATA", data)
        return release.Receipts("services")

    def test_updates_only_the_named_services(self):
        with tempfile.TemporaryDirectory() as scratch:
            compose, state = self.project(scratch, {"server": "s2"})
            result = release.apply_services(compose, ["server"], False, self.receipts(scratch))
            calls = (state / "calls").read_text().splitlines()
        self.assertIn("-p map pull server", calls)
        self.assertIn("-p map up --detach --no-deps --wait --pull never server", calls)
        self.assertEqual(result["replaced"], ["server"])
        self.assertEqual(sorted(result["unchanged"]), ["edge", "ingest", "postgres", "postgres-backup"])

    def test_a_dry_run_changes_nothing(self):
        with tempfile.TemporaryDirectory() as scratch:
            compose, state = self.project(scratch, {"server": "s2"})
            result = release.apply_services(compose, ["server", "edge"], True, self.receipts(scratch))
            calls = (state / "calls").read_text().splitlines()
        self.assertTrue(result["dry_run"])
        self.assertEqual([call for call in calls if " ps " not in f" {call} " and "images" not in call],
                         ["-p map --dry-run pull server edge",
                          "-p map --dry-run up --detach --no-deps --wait --pull never server edge"])

    def test_refuses_unrelated_replacements_and_absent_services(self):
        with tempfile.TemporaryDirectory() as scratch:
            compose, _ = self.project(scratch, {"server": "s2", "postgres": "p2"})
            with self.assertRaisesRegex(release.ReleaseFailure, "outside the selection.*postgres"):
                release.apply_services(compose, ["server"], False, self.receipts(scratch))
        with tempfile.TemporaryDirectory() as scratch:
            compose, state = self.project(scratch, {})
            (state / "ps-before.json").write_text(json.dumps({"Service": "postgres", "ID": "p1", "State": "running"}))
            with self.assertRaisesRegex(release.ReleaseFailure, "no server service"):
                release.apply_services(compose, ["server"], False, self.receipts(scratch))

    def test_rejects_services_it_does_not_manage(self):
        refused = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/release.py"), "services", "postgres"],
                                 capture_output=True, text=True, check=False)
        self.assertEqual(refused.returncode, 2)
        self.assertIn("invalid choice: 'postgres'", refused.stderr)


class CssRelease(unittest.TestCase):
    def test_replaces_only_the_stylesheet(self):
        with tempfile.TemporaryDirectory() as scratch:
            base, bundle = Path(scratch) / "base", Path(scratch) / "bundle"
            dist = base / "client" / "dist"
            (dist / "tiles").mkdir(parents=True)
            (base / "claims-client" / "dist").mkdir(parents=True)
            old_css = b".old{}"
            (dist / "input-0123456789abcdef.css").write_bytes(old_css)
            (dist / "input-0123456789abcdef.css.gz").write_bytes(gzip.compress(old_css))
            (dist / "app-89abcdef01234567_bg.wasm").write_bytes(b"\0asm-bytes")
            (dist / "tiles" / "main.webp").write_bytes(b"tile")
            (dist / "index.html").write_text(
                '<link rel="stylesheet" href="/input-0123456789abcdef.css" integrity="'
                + release.sri(old_css) + '"/><link rel="preload" href="/app-89abcdef01234567_bg.wasm">')
            (base / "claims-client" / "dist" / "index.html").write_text("<p>claims</p>")
            (base / "release.json").write_text("{}")

            detail = release.build_css_release(base, bundle, release.resolve("HEAD"))

            new = bundle / "client" / "dist" / detail["stylesheet"]
            css = new.read_bytes()
            self.assertEqual(detail["stylesheet"], f"input-{hashlib.sha256(css).hexdigest()[:16]}.css")
            page = (bundle / "client" / "dist" / "index.html").read_text()
            self.assertIn(f'href="/{detail["stylesheet"]}" integrity="{release.sri(css)}"', page)
            self.assertNotIn("0123456789abcdef", page)
            self.assertFalse((bundle / "client/dist/input-0123456789abcdef.css").exists())
            self.assertEqual(brotli.decompress(new.with_name(new.name + ".br").read_bytes()), css)
            self.assertEqual(gzip.decompress(new.with_name(new.name + ".gz").read_bytes()), css)
            for kept in ("client/dist/app-89abcdef01234567_bg.wasm", "client/dist/tiles/main.webp",
                         "claims-client/dist/index.html"):
                self.assertEqual((bundle / kept).read_bytes(), (base / kept).read_bytes(), kept)
            self.assertFalse((bundle / "release.json").exists())
            self.assertEqual(detail["reused_files"], 3)
            self.assertIn(base64.b64encode(hashlib.sha384(css).digest()).decode(), page)


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
