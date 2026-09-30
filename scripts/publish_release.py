#!/usr/bin/env python3
"""Validate artifacts and publish unsigned development or explicitly signed releases."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu")


def signed_request_allowed(signed, event):
    if signed and event != "workflow_dispatch":
        raise RuntimeError("Signed publication is allowed only from a manual workflow_dispatch run")


def publication_plan(existing, signed, tag):
    prerelease = not signed or "-" in tag
    if existing:
        has_signed_images = any(a["name"].endswith(".dmg") for a in existing.get("assets", []))
        if has_signed_images and (not existing.get("draft") or not signed):
            raise RuntimeError("Refusing to overwrite signed release assets; publish a new version")
        if not existing.get("draft") and not existing.get("prerelease"):
            raise RuntimeError("Refusing to overwrite a published stable release")
    return {"prerelease": prerelease, "create_draft": not bool(existing), "promote_development": bool(existing and not existing.get("draft"))}


def artifact_files(output):
    return sorted(p for p in output.iterdir() if p.is_file() and p.name != "SHA256SUMS")


def prepare_checksums(output):
    (output / "SHA256SUMS").write_text("".join(hashlib.sha256(p.read_bytes()).hexdigest() + "  " + p.name + "\n" for p in artifact_files(output)))


def validate_artifacts(output, version, signed):
    for target in TARGETS:
        archive = output / f"agx-{version}-{target}.tar.gz"
        if not archive.is_file():
            raise RuntimeError(f"Missing native archive: {archive.name}")
    for extension in ("tar.gz", "zip"):
        if not (output / f"agentgrep-{version}-source.{extension}").is_file():
            raise RuntimeError("Both source archives are required")
    reports = list(output.glob("*.notarization.json"))
    images = list(output.glob("*.dmg"))
    if not signed:
        if reports or images:
            raise RuntimeError("Unsigned development publication cannot contain signed/notarized image assets")
        return
    if len(reports) != 2 or len(images) != 2:
        raise RuntimeError("Signed publication requires both Mac notarization reports and DMGs")
    teams = set()
    for target in TARGETS[:2]:
        name = f"agx-{version}-{target}"
        report = json.loads((output / f"{name}.notarization.json").read_text())
        image = output / f"{name}.dmg"
        if report.get("status") != "Accepted" or report.get("stapled") is not True or report.get("target") != target or report.get("version") != version:
            raise RuntimeError("Both Mac images must have accepted, stapled notarizations for this release")
        if report.get("dmg_sha256") != hashlib.sha256(image.read_bytes()).hexdigest():
            raise RuntimeError("Notarized DMG checksum mismatch")
        teams.add(report.get("apple_team_id"))
    if len(teams) != 1 or not re.fullmatch(r"[A-Z0-9]{10}", next(iter(teams)) or ""):
        raise RuntimeError("Both Mac images must use the same valid Apple Team ID")


def gh(arguments, allow_missing=False):
    result = subprocess.run(["gh"] + arguments, capture_output=True, text=True)
    if result.returncode:
        if allow_missing and ("HTTP 404" in result.stderr or "Not Found" in result.stdout):
            return None
        raise RuntimeError("GitHub release operation failed: " + result.stderr.strip())
    return result.stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True)
    parser.add_argument("--out", type=Path, default=Path("dist"))
    parser.add_argument("--signed", action="store_true")
    parser.add_argument("--prepare-only", action="store_true")
    args = parser.parse_args()
    try:
        if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?", args.tag):
            raise RuntimeError("Invalid release tag")
        if args.prepare_only:
            prepare_checksums(args.out)
            return
        signed_request_allowed(args.signed, os.environ.get("GITHUB_EVENT_NAME", ""))
        validate_artifacts(args.out, args.tag[1:], args.signed)
        repository = os.environ.get("GITHUB_REPOSITORY", "ar4ft/agentgrep")
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
        tag_commit = json.loads(gh(["api", f"repos/{repository}/commits/{args.tag}"]))["sha"]
        if tag_commit != head:
            raise RuntimeError("Release tag does not match the built source commit")
        response = gh(["api", f"repos/{repository}/releases/tags/{args.tag}"], allow_missing=True)
        existing = json.loads(response) if response else None
        plan = publication_plan(existing, args.signed, args.tag)
        notes = Path(f"docs/releases/{args.tag}.md").read_text()
        mode = "Developer ID signed and Apple notarized Mac release." if args.signed else "**Unsigned development prerelease.** Apple signing/notarization was not requested. Automatic updates reject these unsigned artifacts."
        with tempfile.NamedTemporaryFile(mode="w", suffix=".md") as body:
            body.write(mode + "\n\n" + notes)
            body.flush()
            if plan["create_draft"]:
                gh(["release", "create", args.tag, "--repo", repository, "--verify-tag", "--draft", "--title", f"agentgrep {args.tag}", "--notes-file", body.name])
            gh(["release", "upload", args.tag, "--repo", repository, "--clobber"] + [str(p) for p in sorted(args.out.iterdir()) if p.is_file()])
            gh(["release", "edit", args.tag, "--repo", repository, "--draft=false", "--prerelease=" + str(plan["prerelease"]).lower(), "--latest=" + str(not plan["prerelease"]).lower(), "--notes-file", body.name])
        print(json.dumps({"tag": args.tag, "signed": args.signed, "prerelease": plan["prerelease"], "url": f"https://github.com/{repository}/releases/tag/{args.tag}"}))
    except (RuntimeError, OSError, ValueError) as error:
        parser.exit(2, json.dumps({"error": str(error)}) + "\n")


if __name__ == "__main__":
    main()
