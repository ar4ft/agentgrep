"""Signing workflow contract tests. Apple commands are mocked; no credentials needed."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/notarize_macos.py"
SPEC = importlib.util.spec_from_file_location("notarize_macos", SCRIPT)
SIGNING = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIGNING)


class SigningTests(unittest.TestCase):
    def credentials(self):
        return {
            "MACOS_CERTIFICATE_P12": base64.b64encode(b"fixture-p12").decode(),
            "MACOS_CERTIFICATE_PASSWORD": "fixture-secret-do-not-print",
            "APPLE_TEAM_ID": "ABCDE12345", "APPLE_NOTARY_KEY_P8": "fixture-p8",
            "APPLE_NOTARY_KEY_ID": "FIXTUREKEY1", "APPLE_NOTARY_ISSUER_ID": "fixture-issuer",
        }

    def test_missing_configuration_is_explicit_without_secret_values(self):
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaisesRegex(RuntimeError, "Missing GitHub Actions secrets"):
                SIGNING.configuration()

    def test_untrusted_team_identifier_is_rejected(self):
        config = self.credentials()
        config["APPLE_TEAM_ID"] = 'x"; arbitrary'
        with patch.dict(os.environ, config, clear=True):
            with self.assertRaisesRegex(RuntimeError, "10-character"):
                SIGNING.configuration()

    def test_signing_failures_do_not_expose_command_passwords(self):
        failed = subprocess.CompletedProcess([], 1, "", "")
        with patch.object(SIGNING.subprocess, "run", return_value=failed):
            with self.assertRaises(RuntimeError) as error:
                SIGNING.execute(["security", "-p", "fixture-secret-do-not-print"], "Import identity")
        self.assertNotIn("fixture-secret", str(error.exception))

    def pipeline(self, accepted):
        commands = []
        cleanup = []
        def execute(arguments, operation, timeout=120):
            commands.append(arguments)
            stdout = ""
            if arguments[:2] == ["security", "find-identity"]:
                stdout = '1) ' + 'A' * 40 + ' "Developer ID Application: Fixture (ABCDE12345)"'
            elif arguments[-1:] == ["--version"]:
                stdout = "agx 0.2.0\n"
            elif arguments[:2] == ["hdiutil", "create"]:
                Path(arguments[-1]).write_bytes(b"fixture-dmg")
            elif arguments[:3] == ["xcrun", "notarytool", "submit"]:
                stdout = json.dumps({"status": "Accepted" if accepted else "Invalid", "id": "fixture-submission"})
            elif arguments[-2:] == ["rev-parse", "HEAD"]:
                stdout = "fixture-commit"
            return subprocess.CompletedProcess(arguments, 0, stdout, "")
        def remove(arguments, **_kwargs):
            cleanup.append(arguments)
            return subprocess.CompletedProcess(arguments, 0)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "agx"
            binary.write_bytes(b"fixture-binary")
            with patch.dict(os.environ, self.credentials(), clear=True), patch.object(SIGNING.platform, "system", return_value="Darwin"), patch.object(SIGNING.platform, "machine", return_value="arm64"), patch.object(SIGNING, "execute", side_effect=execute), patch.object(SIGNING.subprocess, "run", side_effect=remove):
                if accepted:
                    result = SIGNING.sign(binary, "0.2.0", root / "dist")
                    self.assertEqual(result["status"], "Accepted")
                    self.assertTrue(result["stapled"])
                    self.assertEqual(result["target"], "aarch64-apple-darwin")
                    self.assertEqual(result["apple_team_id"], "ABCDE12345")
                    self.assertEqual(len(list((root / "dist").glob("*.notarization.json"))), 1)
                else:
                    with self.assertRaisesRegex(RuntimeError, "not accepted"):
                        SIGNING.sign(binary, "0.2.0", root / "dist")
                    self.assertEqual(list((root / "dist").glob("*.notarization.json")), [])
            self.assertTrue(any(c[:2] == ["security", "delete-keychain"] for c in cleanup))
        return commands

    def test_accepted_notarization_staples_and_checks_gatekeeper(self):
        commands = self.pipeline(True)
        self.assertTrue(any(c[:3] == ["xcrun", "stapler", "staple"] for c in commands))
        self.assertTrue(any(c[:3] == ["xcrun", "stapler", "validate"] for c in commands))
        self.assertTrue(any(c[0] == "spctl" for c in commands))
        signs = [c for c in commands if c[0] == "codesign" and "--sign" in c]
        self.assertEqual(len(signs), 2)
        self.assertIn("runtime", signs[0])
        self.assertIn("dev.agentgrep.agx", signs[0])
        self.assertIn("dev.agentgrep.agx.diskimage", signs[1])

    def test_rejected_notarization_does_not_mark_the_release_accepted(self):
        commands = self.pipeline(False)
        self.assertFalse(any(c[:2] == ["xcrun", "stapler"] for c in commands))


if __name__ == "__main__":
    unittest.main()
