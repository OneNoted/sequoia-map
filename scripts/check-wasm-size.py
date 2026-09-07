# /// script
# requires-python = ">=3.13"
# dependencies = ["brotli==1.1.0"]
# ///
"""Measure optimized WASM without modifying Trunk's hashed release artifacts."""

import gzip
from pathlib import Path
import subprocess
import tempfile

import brotli


def main() -> None:
    artifacts = list(Path("client/dist").glob("*_bg.wasm"))
    if len(artifacts) != 1:
        raise SystemExit(f"Expected one client WASM artifact, found {len(artifacts)}")

    with tempfile.TemporaryDirectory() as directory:
        optimized = Path(directory) / "client.wasm"
        subprocess.run(["wasm-opt", "-Oz", str(artifacts[0]), "-o", str(optimized)], check=True)
        wasm = optimized.read_bytes()

    sizes = {"brotli": len(brotli.compress(wasm, quality=11)), "gzip": len(gzip.compress(wasm, compresslevel=9, mtime=0))}
    budgets = {"brotli": 1_900_000, "gzip": 2_800_000}
    print(f"Optimized WASM: {len(wasm):,} bytes")
    for encoding, size in sizes.items():
        print(f"{encoding}: {size:,} bytes (budget {budgets[encoding]:,})")
    if any(size > budgets[encoding] for encoding, size in sizes.items()):
        raise SystemExit("Client WASM exceeds the size budget")


if __name__ == "__main__":
    main()
