#!/usr/bin/env python3
"""Package a native agx binary with its documentation, skill, and build metadata."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import re
import subprocess
import tarfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/release/agx")
    parser.add_argument("--out", default="dist")
    parser.add_argument("--version", default="0.3.0")
    parser.add_argument("--macos-signing-report", type=Path)
    parser.add_argument("--require-apple-signing", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?", args.version):
        parser.error("version must be a semantic version")
    binary = Path(args.binary).resolve(strict=True)
    repository = Path(__file__).resolve().parent.parent
    output = Path(args.out).resolve()
    output.mkdir(parents=True, exist_ok=True)
    architecture = {"arm64": "aarch64", "AMD64": "x86_64"}.get(platform.machine(), platform.machine())
    suffix = {"Darwin": "apple-darwin", "Linux": "unknown-linux-gnu"}.get(platform.system())
    if suffix is None:
        parser.error("native packaging supports Mac and Linux")
    target = f"{architecture}-{suffix}"
    name = f"agx-{args.version}-{target}"
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip()
    reported = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if reported != f"agx {args.version}":
        parser.error(f"binary version mismatch: {reported}")
    metadata = {"name": "agentgrep", "version": args.version, "target": target, "commit": commit, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "mac_signing": "unsigned and not notarized" if platform.system() == "Darwin" else "not applicable"}
    if args.require_apple_signing and platform.system() != "Darwin":
        parser.error("--require-apple-signing is only valid on Mac")
    if args.require_apple_signing and not args.macos_signing_report:
        parser.error("Apple notarization report is required; refusing unsigned release packaging")
    if args.macos_signing_report:
        report = json.loads(args.macos_signing_report.read_text())
        if platform.system() != "Darwin" or report.get("status") != "Accepted" or report.get("stapled") is not True:
            parser.error("Apple notarization must be accepted with a stapled ticket")
        if report.get("binary_sha256") != metadata["binary_sha256"] or report.get("version") != args.version or report.get("commit") != commit or report.get("target") != target:
            parser.error("Notarization report does not match this binary, version, architecture, and commit")
        dmg = args.macos_signing_report.parent / report.get("dmg", "")
        if not dmg.is_file() or dmg.parent != args.macos_signing_report.parent or hashlib.sha256(dmg.read_bytes()).hexdigest() != report.get("dmg_sha256"):
            parser.error("Stapled DMG checksum does not match notarization report")
        metadata["mac_signing"] = "Developer ID signed and Apple notarized; offline ticket stapled to the DMG, not the tar archive"
        metadata["apple_team_id"] = report["apple_team_id"]
        metadata["notarization_submission_id"] = report["submission_id"]
    archive = output / f"{name}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        for source, destination in [
            (binary, "agx"), (repository / "README.md", "README.md"),
            (repository / "LICENSE", "LICENSE"),
            (repository / "skills/agentgrep/SKILL.md", "skills/agentgrep/SKILL.md"),
            (repository / "docs/validation.md", "docs/validation.md"),
            (repository / "docs/signing-and-updates.md", "docs/signing-and-updates.md"),
            (repository / "docs/cli-json.md", "docs/cli-json.md"),
            (repository / "docs/editor-protocol.md", "docs/editor-protocol.md"),
            (repository / "docs/nain-integration.md", "docs/nain-integration.md"),
            (repository / "examples/nain_adapter.rs", "examples/nain_adapter.rs"),
        ]:
            info = tar.gettarinfo(str(source), arcname=f"{name}/{destination}")
            info.mode = 0o755 if destination == "agx" else 0o644
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            with source.open("rb") as file:
                tar.addfile(info, file)
        data = (json.dumps(metadata, indent=2) + "\n").encode()
        info = tarfile.TarInfo(f"{name}/build.json")
        info.size = len(data)
        info.mode = 0o644
        tar.addfile(info, io.BytesIO(data))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / f"{name}.sha256").write_text(f"{digest}  {archive.name}\n")
    print(json.dumps({"archive": str(archive), "sha256": digest, "build": metadata}))


if __name__ == "__main__":
    main()
