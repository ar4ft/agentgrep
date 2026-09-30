"""Release policy tests: manual signing, unsigned development, immutable production."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/publish_release.py"
SPEC = importlib.util.spec_from_file_location("publish_release", SCRIPT)
PUBLISH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLISH)


class PublicationTests(unittest.TestCase):
    def test_signing_requires_manual_event_and_unsigned_does_not(self):
        for event in ("push", "pull_request", "schedule", ""):
            with self.assertRaisesRegex(RuntimeError, "manual"):
                PUBLISH.signed_request_allowed(True, event)
            PUBLISH.signed_request_allowed(False, event)
        PUBLISH.signed_request_allowed(True, "workflow_dispatch")

    def test_unsigned_stable_tags_are_still_development_prereleases(self):
        self.assertTrue(PUBLISH.publication_plan(None, False, "v0.2.0")["prerelease"])
        self.assertFalse(PUBLISH.publication_plan(None, True, "v0.2.0")["prerelease"])
        self.assertTrue(PUBLISH.publication_plan(None, True, "v0.2.0-rc.1")["prerelease"])

    def test_manual_signing_promotes_development_but_never_overwrites_signed_release(self):
        development = {"draft": False, "prerelease": True, "assets": [{"name": "agx.tar.gz"}]}
        plan = PUBLISH.publication_plan(development, True, "v0.2.0")
        self.assertTrue(plan["promote_development"])
        self.assertFalse(plan["prerelease"])
        signed = {**development, "assets": [{"name": "agx.dmg"}]}
        for sign in (False, True):
            with self.assertRaisesRegex(RuntimeError, "signed release"):
                PUBLISH.publication_plan(signed, sign, "v0.2.0")
        with self.assertRaisesRegex(RuntimeError, "stable release"):
            PUBLISH.publication_plan({**development, "prerelease": False}, False, "v0.2.0")

    def test_signed_draft_can_resume_only_with_signed_manual_policy(self):
        draft = {"draft": True, "prerelease": True, "assets": [{"name": "agx.dmg"}]}
        PUBLISH.publication_plan(draft, True, "v0.2.0")
        with self.assertRaises(RuntimeError):
            PUBLISH.publication_plan(draft, False, "v0.2.0")

    def artifacts(self, root):
        for target in PUBLISH.TARGETS:
            (root / f"agx-0.2.0-{target}.tar.gz").write_bytes(b"fixture-archive")
        for extension in ("tar.gz", "zip"):
            (root / f"agentgrep-0.2.0-source.{extension}").write_bytes(b"fixture-source")

    def test_signed_publication_requires_both_matching_notarized_images(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.artifacts(root)
            PUBLISH.validate_artifacts(root, "0.2.0", False)
            with self.assertRaisesRegex(RuntimeError, "both Mac"):
                PUBLISH.validate_artifacts(root, "0.2.0", True)
            for target in PUBLISH.TARGETS[:2]:
                name = f"agx-0.2.0-{target}"
                image = root / f"{name}.dmg"
                image.write_bytes(b"fixture-image")
                report = {"version": "0.2.0", "target": target, "status": "Accepted", "stapled": True, "apple_team_id": "ABCDE12345", "dmg_sha256": hashlib.sha256(image.read_bytes()).hexdigest()}
                (root / f"{name}.notarization.json").write_text(json.dumps(report))
            PUBLISH.validate_artifacts(root, "0.2.0", True)
            with self.assertRaisesRegex(RuntimeError, "Unsigned"):
                PUBLISH.validate_artifacts(root, "0.2.0", False)
            image.write_bytes(b"corrupted")
            with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                PUBLISH.validate_artifacts(root, "0.2.0", True)


if __name__ == "__main__":
    unittest.main()
