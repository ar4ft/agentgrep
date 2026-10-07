# Installing agx from GitHub

Apple Silicon and Intel Macs can install the prebuilt executable without Rust,
Homebrew, sudo, jq, or Python:

```sh
curl -fsSL https://raw.githubusercontent.com/ar4ft/agentgrep/main/scripts/install.sh | sh
```

This installs the **newest published release, including development prereleases**.
Current releases are unsigned development builds; the installer prints that
status. Apple credentials are not configured yet. For production releases only:

```sh
curl -fsSL https://raw.githubusercontent.com/ar4ft/agentgrep/main/scripts/install.sh | sh -s -- --stable
```

`--stable` fails if there is no production release. It also rejects an explicitly
pinned development prerelease. The installer does not enable automatic updates.
Rerunning it explicitly upgrades/reinstalls a development build. Starting with
0.3.4, `agx update-pre --check` discovers development updates and `agx update-pre`
installs a newer unsigned archive at the current executable path, retaining
rollback for `agx update rollback`. This updates only the binary, not documentation,
installer receipts or installed skills. Bootstrap older versions with the installer
once to acquire the command. The existing
`agx update install` and opt-in launchd updater retain their stronger Developer ID,
Team ID, notarization and DMG checks; unsigned development builds cannot use them
to install updates. See [signing and updates](signing-and-updates.md).

## Pinning, reviewing, and custom paths

To select a specific release, use its versioned script and
pin the binary version too (a release's script otherwise still selects latest):

```sh
curl -fsSL https://github.com/ar4ft/agentgrep/releases/download/v0.3.4/install.sh | sh -s -- --version 0.3.4
```

Alternatively, download and inspect the script before executing it:

```sh
curl -fsSL https://raw.githubusercontent.com/ar4ft/agentgrep/main/scripts/install.sh -o install-agx.sh
less install-agx.sh
sh install-agx.sh --version 0.3.4 --prefix "$HOME/.agx" --no-modify-path
"$HOME/.agx/bin/agx" --version
```

Options: `--version VERSION` (with or without `v`), `--stable`, `--prefix ABSOLUTE_PATH`,
`--no-modify-path`, and `--help`. `AGX_VERSION` and `AGX_INSTALL_DIR` supply defaults;
arguments take precedence. Paths may contain spaces/quotes but not colons or
newlines. Linux x86_64 with glibc is also supported; Windows, musl Linux, and Linux
ARM do not have release binaries and fail explicitly. On Apple Silicon, a shell
running under Rosetta still selects the native ARM binary.

The only download sources are GitHub's release metadata, public Atom release feed,
and release assets over HTTPS. If the API is blocked/unavailable, default discovery
uses the public feed and skips tags without native checksum assets (up to 20 candidates); an explicit version can directly select its release assets.
In both cases the installer warns that prerelease/signing classification is unknown
and retains all archive/hash/binary checks. `--stable` requires API classification
and fails if it is unavailable; the feed cannot prove a release is production.
`curl`, `tar`, `awk`, `fold`, `sed`, `mktemp`, and either `shasum` or `sha256sum`
are required (available on a standard Mac). Network failures, unavailable metadata
and feed, unavailable assets, hash mismatches, unsafe archive entries,
unmanaged destination binaries, and unsupported systems fail with a nonzero exit
code and a diagnostic. No token or GitHub CLI is required.

## Files and PATH

- `~/.agx/bin/agx`: regular executable; replacement is staged on the same filesystem
  and renamed atomically. Running editor workers continue using their old process;
  restart them explicitly to use the new version.
- `~/.agx/bin/agx.previous`: binary present before the most recent script install.
  This is separate from the verified updater's own rollback state.
- `~/.agx/share/agx-VERSION-TARGET/`: bundled README, license, build metadata,
  protocol/integration documentation, and `skills/agentgrep/SKILL.md`.
  Documentation from previous versions is retained; it may be removed manually.
- `~/.agx/env`: safely quoted PATH statement; `install-receipt`: installed version,
  target and archive SHA-256. Neither is a cryptographic trust anchor.

By default the installer adds one idempotent source line to the selected shell
profile: macOS zsh `.zprofile`, macOS bash `.bash_profile`, other zsh `.zshrc`,
other bash `.bashrc`, or `.profile`. It preserves existing contents. Custom shell
startup arrangements may require sourcing `~/.agx/env` yourself. Open a new terminal
or run `. "$HOME/.agx/env"`; a piped installer cannot change its parent shell's PATH.
`--no-modify-path` leaves shell profiles alone. Existing unrelated installations
on PATH are not removed; check `command -v agx` when multiple copies exist.

GUI applications may have a different PATH. Configure the future nain adapter with
the absolute executable path `/Users/YOU/.agx/bin/agx`. The example adapter includes
this location in discovery; installation alone does **not** add a panel to current
nain. See [the nain guide](nain-integration.md).

Only the binary and bundle are installed. No model downloads, telemetry setup,
background update jobs, MCP registrations, or Codex/Claude configuration changes
are performed. Installing the bundled skill into a harness remains an explicit
`agx skill` action. The script uses a fail-fast per-prefix lock; after an uncatchable
termination (SIGKILL/power loss), check that no installer is running before removing
the empty `~/.agx/.install-lock` directory. Temporary `.install.*` directories from
such an interruption can also be removed once no installer is running.

## Verification and trust

The installer checks the archive's SHA-256 against its release checksum, rejects
links/special files/path traversal before extraction, and runs only `agx --version`
to check the selected binary before replacement. Failed downloads/verification
leave the existing executable intact. Shell profile errors can occur after a
successful binary install and are reported with manual PATH instructions.

Checksums fetched from the same GitHub release establish download integrity,
**not independent publisher authentication**. The bootstrap installer trusts the
GitHub repository, HTTPS, and the selected release. It does not perform the
verified updater's Apple identity/notarization checks, bypass Gatekeeper, remove
quarantine attributes, or claim unsigned builds are notarized. If macOS blocks a
development binary, use a trusted source build or wait for a signed release.
Production Mac distributions also provide notarized DMGs with stapled tickets;
use the documented signing/update process when those trust guarantees are needed.

Every release automatically uploads `install.sh` alongside native/source archives.
`SHA256SUMS` includes the script. Tag builds stay unsigned; signing is still possible
only by manually running the release workflow with `sign_and_notarize=true`.

Source installation remains supported with the normal executable name:

```sh
cargo install --path . --locked
```
