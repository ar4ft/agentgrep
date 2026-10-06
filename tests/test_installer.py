"""Exercise the actual POSIX installer offline on each native CI architecture."""
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import tempfile
import unittest

REPOSITORY = Path(__file__).resolve().parents[1]
INSTALLER = REPOSITORY / "scripts/install.sh"
BINARY = REPOSITORY / "target/debug/agx"


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.fixture = self.root / "fixture"
        self.fixture.mkdir()
        tools = self.root / "tools"
        tools.mkdir()
        # Replace only transport, not tar/checksum/shell/native binary execution.
        curl = tools / "curl"
        curl.write_text(f"#!{sys.executable}\n" + '''import os, sys
from pathlib import Path
from urllib.parse import urlparse
args = sys.argv[1:]
assert args[args.index('--proto')+1] == '=https'
assert args[args.index('--proto-redir')+1] == '=https'
url = next(a for a in args if a.startswith('https://'))
path = urlparse(url).path
fixture = Path(os.environ['INSTALL_FIXTURE'])
if urlparse(url).hostname == 'api.github.com' and (fixture / 'block-api').exists():
    print('fixture HTTP 403', file=sys.stderr)
    raise SystemExit(22)
if '/releases/download/' in path:
    source = fixture / path.rsplit('/', 1)[1]
elif path.endswith('/releases.atom'):
    source = fixture / 'releases.atom'
else:
    source = fixture / 'release.json'
Path(args[args.index('-o')+1]).write_bytes(source.read_bytes())
''')
        curl.chmod(0o755)
        self.env = {**os.environ, "HOME": str(self.home), "SHELL": "/bin/zsh",
                    "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                    "INSTALL_FIXTURE": str(self.fixture)}
        for name in ("AGX_INSTALL_DIR", "AGX_VERSION"):
            self.env.pop(name, None)
        arch = {"arm64": "aarch64"}.get(platform.machine(), platform.machine())
        suffix = "apple-darwin" if platform.system() == "Darwin" else "unknown-linux-gnu"
        self.target = f"{arch}-{suffix}"
        self.version = subprocess.check_output([str(BINARY), "--version"], text=True).strip().split()[1]
        self.bundle = f"agx-{self.version}-{self.target}"
        self.archive = self.fixture / f"{self.bundle}.tar.gz"
        self.metadata()
        self.package()

    def metadata(self, *, prerelease=True, compact=False, tag=None, array=True):
        data = {"body": 'ignore \\"tag_name\\": \\"v999.0.0\\"',
                "tag_name": tag or f"v{self.version}", "prerelease": prerelease,
                "draft": False, "assets": [{"name": "tag_name", "tag_name": "v999.0.0"}]}
        (self.fixture / "release.json").write_text(json.dumps([data] if array else data, indent=None if compact else 2))

    def package(self, unsafe=None):
        with tarfile.open(self.archive, "w:gz", compresslevel=1) as tar:
            tar.add(BINARY, arcname=f"{self.bundle}/agx")
            metadata = json.dumps({"version": self.version, "target": self.target}).encode()
            info = tarfile.TarInfo(f"{self.bundle}/build.json")
            info.size = len(metadata)
            tar.addfile(info, io.BytesIO(metadata))
            if unsafe:
                info = tarfile.TarInfo(unsafe)
                if unsafe.endswith("link"):
                    info.type = tarfile.SYMTYPE
                    info.linkname = "../../escaped"
                tar.addfile(info)
        checksum = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        (self.fixture / f"{self.bundle}.sha256").write_text(f"{checksum}  {self.archive.name}\n")

    def run_installer(self, *args, success=True):
        # Supply the script on stdin exactly like curl | sh, including arguments.
        result = subprocess.run(["sh", "-s", "--", *args], input=INSTALLER.read_text(),
                                env=self.env, text=True, capture_output=True, timeout=30)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_native_install_and_repeat_upgrade_are_atomic_and_path_is_idempotent(self):
        original_profile = "# user settings\nexport EDITOR=nain\n"
        profile = self.home / (".zprofile" if platform.system() == "Darwin" else ".zshrc")
        profile.write_text(original_profile)
        first = self.run_installer()
        self.assertIn("development prerelease", first.stderr)
        installed = self.home / ".agx/bin/agx"
        self.assertEqual(subprocess.check_output([str(installed), "--version"], text=True), f"agx {self.version}\n")
        self.run_installer("--version", f"v{self.version}")
        self.assertEqual(installed.read_bytes(), BINARY.read_bytes())
        self.assertEqual(installed.with_name("agx.previous").read_bytes(), BINARY.read_bytes())
        self.assertTrue(profile.read_text().startswith(original_profile))
        self.assertEqual(profile.read_text().count("# agx installer"), 1)
        self.assertFalse((self.home / ".agx/.install-lock").exists())
        self.assertEqual(list((self.home / ".agx").glob(".install.*")), [])

    def test_custom_path_with_shell_metacharacters_is_literal_and_no_profile_changes(self):
        prefix = self.home / "space ' dollar $ and `touch SHOULD_NOT_EXIST`"
        self.run_installer("--prefix", str(prefix), "--no-modify-path")
        result = subprocess.run(["sh", "-c", '. "$1/env"; command -v agx', "sh", str(prefix)],
                                env=self.env, text=True, capture_output=True, check=True)
        self.assertEqual(result.stdout.strip(), str(prefix / "bin/agx"))
        self.assertFalse((REPOSITORY / "SHOULD_NOT_EXIST").exists())
        self.assertFalse((self.home / ".zprofile").exists())
        self.assertFalse((self.home / ".zshrc").exists())

    def test_compact_metadata_and_stable_filter(self):
        self.metadata(compact=True)
        self.run_installer("--stable", success=False)
        self.metadata(prerelease=False, compact=True, array=False)
        result = self.run_installer("--stable", "--no-modify-path")
        self.assertNotIn("development prerelease", result.stderr)

    def test_failed_checksum_preserves_existing_binary_and_releases_lock(self):
        self.run_installer("--no-modify-path")
        installed = self.home / ".agx/bin/agx"
        old = installed.read_bytes()
        self.archive.write_bytes(b"corrupted archive")
        result = self.run_installer(success=False)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertEqual(installed.read_bytes(), old)
        self.assertFalse((self.home / ".agx/.install-lock").exists())

    def test_archive_traversal_and_links_are_rejected_before_extraction(self):
        for unsafe in (f"{self.bundle}/../../escaped", f"{self.bundle}/link", "/absolute"):
            with self.subTest(unsafe=unsafe):
                self.package(unsafe)
                self.run_installer(success=False)
                self.assertFalse((self.home / ".agx/bin/agx").exists())
                self.assertFalse((self.home / ".agx/escaped").exists())

    def test_wrong_tag_and_invalid_version_fail(self):
        self.run_installer("--version", "../bad", success=False)
        self.metadata(tag="v999.0.0")
        self.assertIn("version mismatch", self.run_installer("--version", self.version, success=False).stderr)

    def test_unmanaged_binary_and_symlink_are_preserved(self):
        binary = self.home / ".agx/bin/agx"
        binary.parent.mkdir(parents=True)
        binary.write_text("user file")
        self.run_installer(success=False)
        self.assertEqual(binary.read_text(), "user file")
        binary.unlink()
        binary.symlink_to(self.root / "external")
        self.run_installer(success=False)
        self.assertTrue(binary.is_symlink())

    def test_concurrent_installer_fails_without_removing_other_lock(self):
        lock = self.home / ".agx/.install-lock"
        lock.mkdir(parents=True)
        result = self.run_installer(success=False)
        self.assertIn("another installer", result.stderr)
        self.assertTrue(lock.exists())

    def test_blocked_api_uses_feed_for_latest_and_explicit_pin_but_stable_fails(self):
        (self.fixture / "block-api").touch()
        (self.fixture / "releases.atom").write_text(
            '<feed><entry>\n'
            f'  <link rel="alternate" type="text/html" href="https://github.com/ar4ft/agentgrep/releases/tag/v{self.version}"/>\n'
            '<content>&lt;link href="evil"/&gt;</content>\n</entry></feed>\n')
        result = self.run_installer("--no-modify-path")
        self.assertIn("public release feed", result.stderr)
        self.assertIn("without API release classification", result.stderr)
        self.run_installer("--version", self.version, "--no-modify-path")
        (self.fixture / "releases.atom").unlink()
        self.assertIn("refusing to weaken --stable", self.run_installer("--stable", success=False).stderr)
        self.assertIn("GitHub API and release feed", self.run_installer(success=False).stderr)

    def test_blocked_api_feed_rejects_escaped_body_links_and_unsafe_tags(self):
        (self.fixture / "block-api").touch()
        for feed in (
            '<feed><entry><content>&lt;link rel="alternate" type="text/html" href="https://github.com/ar4ft/agentgrep/releases/tag/v999.0.0"/&gt;</content></entry></feed>',
            '<feed>\n<link rel="alternate" type="text/html" href="https://github.com/ar4ft/agentgrep/releases/tag/../../escape"/>\n</feed>',
        ):
            (self.fixture / "releases.atom").write_text(feed)
            self.run_installer(success=False)
            self.assertFalse((self.home / ".agx/bin/agx").exists())


if __name__ == "__main__":
    unittest.main()
