#!/usr/bin/env python3
"""Sign agx and its DMG, submit to Apple, and staple the accepted ticket.

Private credentials are read only from Actions environment variables, written
into a private temporary directory, and removed with the temporary keychain.
No unsigned fallback is supported.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import secrets
import shutil
import subprocess
import tempfile

REQUIRED = (
    "MACOS_CERTIFICATE_P12", "MACOS_CERTIFICATE_PASSWORD", "APPLE_TEAM_ID",
    "APPLE_NOTARY_KEY_P8", "APPLE_NOTARY_KEY_ID", "APPLE_NOTARY_ISSUER_ID",
)


def configuration():
    missing = [name for name in REQUIRED if not os.environ.get(name)]
    if missing:
        raise RuntimeError("Missing GitHub Actions secrets: " + ", ".join(missing))
    if not re.fullmatch(r"[A-Z0-9]{10}", os.environ["APPLE_TEAM_ID"]):
        raise RuntimeError("APPLE_TEAM_ID must be the 10-character Apple Team ID")
    return {name: os.environ[name] for name in REQUIRED}


def execute(arguments, operation, timeout=120):
    # Never print arguments: keychain/import commands contain private passwords.
    result = subprocess.run(arguments, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{operation} failed (exit {result.returncode}); command arguments were withheld")
    return result


def requirement(team, identifier="dev.agentgrep.agx"):
    return f'identifier "{identifier}" and anchor apple generic and certificate leaf[subject.OU] = "{team}" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists'


def sign(binary, version, output):
    config = configuration()
    if platform.system() != "Darwin":
        raise RuntimeError("Apple signing/notarization must run on macOS")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?", version):
        raise RuntimeError("Invalid release version")
    team = config["APPLE_TEAM_ID"]
    output.mkdir(parents=True, exist_ok=True)
    repository = Path(__file__).resolve().parent.parent
    architecture = {"arm64": "aarch64"}.get(platform.machine(), platform.machine())
    name = f"agx-{version}-{architecture}-apple-darwin"
    dmg = output / f"{name}.dmg"
    if dmg.exists():
        raise RuntimeError("DMG output already exists")
    with tempfile.TemporaryDirectory(prefix="agx-apple-signing-") as temporary:
        work = Path(temporary)
        certificate = work / "developer-id.p12"
        certificate.write_bytes(base64.b64decode("".join(config["MACOS_CERTIFICATE_P12"].split()), validate=True))
        certificate.chmod(0o600)
        api_key = work / "notary-key.p8"
        api_key.write_text(config["APPLE_NOTARY_KEY_P8"])
        api_key.chmod(0o600)
        keychain = work / "release.keychain-db"
        password = secrets.token_urlsafe(32)
        created = False
        try:
            execute(["security", "create-keychain", "-p", password, str(keychain)], "Create temporary keychain")
            created = True
            execute(["security", "set-keychain-settings", "-lut", "21600", str(keychain)], "Configure temporary keychain")
            execute(["security", "unlock-keychain", "-p", password, str(keychain)], "Unlock temporary keychain")
            execute(["security", "import", str(certificate), "-k", str(keychain), "-P", config["MACOS_CERTIFICATE_PASSWORD"], "-T", "/usr/bin/codesign"], "Import Developer ID identity")
            execute(["security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-s", "-k", password, str(keychain)], "Grant codesign key access")
            identities = execute(["security", "find-identity", "-v", "-p", "codesigning", str(keychain)], "Find Developer ID identity").stdout
            matches = re.findall(r'([A-Fa-f0-9]{40}) "Developer ID Application: [^\n"]+ \(' + re.escape(team) + r'\)"', identities)
            if len(matches) != 1:
                raise RuntimeError("Expected exactly one Developer ID Application identity matching APPLE_TEAM_ID")
            identity = matches[0]
            execute(["codesign", "--force", "--identifier", "dev.agentgrep.agx", "--options", "runtime", "--timestamp", "--sign", identity, "--keychain", str(keychain), str(binary)], "Sign agx with hardened runtime")
            execute(["codesign", "--verify", "--strict", "-R", requirement(team), str(binary)], "Verify agx Developer ID and Team ID")
            reported = execute([str(binary), "--version"], "Check signed binary version").stdout.strip()
            if reported != f"agx {version}":
                raise RuntimeError("Signed binary version does not match release tag")
            stage = work / "image"
            stage.mkdir()
            for source, destination in [(binary, "agx"), (repository / "README.md", "README.md"), (repository / "LICENSE", "LICENSE"), (repository / "skills/agentgrep/SKILL.md", "skills/agentgrep/SKILL.md")]:
                target = stage / destination
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
            (stage / "agx").chmod(0o755)
            commit = execute(["git", "-C", str(repository), "rev-parse", "HEAD"], "Read release commit").stdout.strip()
            metadata = {"name": "agentgrep", "version": version, "target": f"{architecture}-apple-darwin", "commit": commit, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "apple_team_id": team, "delivery": "Developer ID signed binary inside a signed DMG with a stapled Apple notarization ticket"}
            (stage / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")
            execute(["hdiutil", "create", "-fs", "HFS+", "-format", "UDZO", "-volname", f"agentgrep-{version}", "-srcfolder", str(stage), str(dmg)], "Create release DMG", timeout=300)
            execute(["codesign", "--force", "--identifier", "dev.agentgrep.agx.diskimage", "--timestamp", "--sign", identity, "--keychain", str(keychain), str(dmg)], "Sign release DMG")
            submission = execute(["xcrun", "notarytool", "submit", str(dmg), "--key", str(api_key), "--key-id", config["APPLE_NOTARY_KEY_ID"], "--issuer", config["APPLE_NOTARY_ISSUER_ID"], "--wait", "--timeout", "20m", "--output-format", "json"], "Apple notarization submission", timeout=1250)
            result = json.loads(submission.stdout)
            if result.get("status") != "Accepted":
                raise RuntimeError(f"Apple notarization was not accepted (status {result.get('status', 'unknown')})")
            execute(["xcrun", "stapler", "staple", str(dmg)], "Staple notarization ticket")
            execute(["xcrun", "stapler", "validate", str(dmg)], "Validate stapled notarization ticket")
            execute(["codesign", "--verify", "--strict", "-R", requirement(team, "dev.agentgrep.agx.diskimage"), str(dmg)], "Verify release DMG identity")
            execute(["spctl", "--assess", "--type", "open", "--context", "context:primary-signature", str(dmg)], "Gatekeeper assessment of notarized DMG")
            report = {**metadata, "status": "Accepted", "submission_id": result.get("id"), "stapled": True, "dmg": dmg.name, "dmg_sha256": hashlib.sha256(dmg.read_bytes()).hexdigest()}
            (output / f"{name}.notarization.json").write_text(json.dumps(report, indent=2) + "\n")
            (output / f"{name}.dmg.sha256").write_text(report["dmg_sha256"] + "  " + dmg.name + "\n")
            return report
        finally:
            if created:
                # Cleanup must not echo the random keychain password or mask the original error.
                subprocess.run(["security", "delete-keychain", str(keychain)], capture_output=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/release/agx")
    parser.add_argument("--version", required=True)
    parser.add_argument("--out", default="dist")
    parser.add_argument("--check-config", action="store_true")
    args = parser.parse_args()
    try:
        if args.check_config:
            configuration()
            print(json.dumps({"signing_configuration": "present"}))
        else:
            report = sign(Path(args.binary).resolve(strict=True), args.version, Path(args.out).resolve())
            print(json.dumps({"dmg": report["dmg"], "status": report["status"], "stapled": report["stapled"], "apple_team_id": report["apple_team_id"]}))
    except (RuntimeError, ValueError, OSError, subprocess.TimeoutExpired) as error:
        # TimeoutExpired includes command arguments; do not stringify it.
        message = "Signing command timed out; arguments withheld" if isinstance(error, subprocess.TimeoutExpired) else str(error)
        parser.exit(2, json.dumps({"error": message}) + "\n")


if __name__ == "__main__":
    main()
