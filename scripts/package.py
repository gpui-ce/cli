#!/usr/bin/env python3
"""Create a reproducible, self-contained CLI archive and its SHA-256 checksum."""

import argparse
import gzip
import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target", choices=sorted(TARGETS), required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--tag", help="Release tag, which must match Cargo's version")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if args.tag is not None and args.tag != f"v{version}":
        parser.error(f"tag {args.tag!r} does not match Cargo version v{version}")
    binary = args.binary.resolve(strict=True)
    result = subprocess.run([str(binary), "--version"], text=True, capture_output=True, check=True)
    if result.stdout.strip() != f"gpui {version}":
        parser.error(f"binary version does not match Cargo version {version}: {result.stdout.strip()}")
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH") or subprocess.check_output(
        ["git", "log", "-1", "--format=%ct"], cwd=ROOT, text=True).strip())
    args.output.mkdir(parents=True, exist_ok=True)
    name = f"gpui-v{version}-{args.target}"
    output = args.output / f"{name}.tar.gz"
    with output.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as archive:
            for filename, source in [("gpui", binary), ("LICENSE", ROOT / "LICENSE"),
                                     ("NOTICE", ROOT / "NOTICE"), ("README.md", ROOT / "README.md")]:
                info = tarfile.TarInfo(f"{name}/{filename}")
                info.size = source.stat().st_size
                info.mode = 0o755 if filename == "gpui" else 0o644
                info.mtime = epoch
                with source.open("rb") as contents:
                    archive.addfile(info, contents)
    with output.open("rb") as contents:
        digest = hashlib.file_digest(contents, "sha256").hexdigest()
    output.with_name(output.name + ".sha256").write_text(f"{digest}  {output.name}\n")
    print(output)


if __name__ == "__main__":
    main()
