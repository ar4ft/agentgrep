# Signed releases and automatic updates

The v0.2 development branch includes Developer ID signing, Apple notarization, stapled disk images, verified updates, rollback, and an opt-in macOS LaunchAgent. The published v0.1.0 release remains unsigned. **Apple credentials are not set up yet, so a signed v0.2.1 release has not been published.**

## One-time Apple setup

1. Enroll in the [Apple Developer Program](https://developer.apple.com/programs/enroll/) and complete Apple's identity/account verification.
2. On a Mac, create a **Developer ID Application** certificate using the developer account/Xcode or Apple's Certificates portal. Export the certificate **and its private key** from Keychain Access as a password-protected `.p12`. A certificate-only `.cer` is insufficient. Developer ID Installer is not required for this DMG distribution.
3. In [App Store Connect](https://appstoreconnect.apple.com/access/integrations/api), create a team API key usable with notarization, using an appropriate role such as Developer. Retain its one-time `.p8` download, Key ID, and Issuer ID. This workflow uses a team key with an issuer, not an individual API key.
4. Find your 10-character Apple Team ID in the [developer membership account](https://developer.apple.com/account).
5. Add the following repository Actions secrets in [agentgrep's secret settings](https://github.com/ar4ft/agentgrep/settings/secrets/actions). Do not send private keys or passwords in chat.

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE_P12` | Base64-encoded exported Developer ID Application `.p12` including its private key |
| `MACOS_CERTIFICATE_PASSWORD` | Password protecting the `.p12` |
| `APPLE_TEAM_ID` | 10-character Apple Team ID |
| `APPLE_NOTARY_KEY_P8` | Complete PEM text of the App Store Connect API private key |
| `APPLE_NOTARY_KEY_ID` | API Key ID |
| `APPLE_NOTARY_ISSUER_ID` | Team API key Issuer ID |

For the certificate secret, macOS can copy the encoded value to the clipboard without printing it:

```sh
base64 -i developer-id.p12 | tr -d '\n' | pbcopy
```

Paste it directly into the GitHub secret form. Store the `.p12`, password, and `.p8` securely outside the repository. The connected GitHub credential currently lacks permission to inspect or configure Actions secrets, so an account administrator must enter them using GitHub's settings (or an appropriately authorized local CLI).

Tag pushes and normal CI never sign: they produce unsigned development binaries without Apple secrets. After the six secrets are configured, create a reviewed version tag, then explicitly run signing by hand:

```sh
git tag v0.2.1
git push origin v0.2.1  # unsigned development prerelease
gh workflow run release.yml --ref main \
  -f release_tag=v0.2.1 -f sign_and_notarize=true
```

Alternatively, open GitHub Actions → release → Run workflow, enter the existing version tag, and enable **sign_and_notarize**. The checkbox defaults to false; a manual unsigned build is also supported. Signing is allowed only for a manual `workflow_dispatch` with that option explicitly true. No tag push, scheduled job, or PR can enable it.

The tag must match `Cargo.toml` and the version printed by the built binary. Do not move a published release tag; fix failures in a new version/tag when necessary. Certificate renewal under the same Team ID preserves updater trust. Team transfers require a deliberate trust migration and new bootstrap installation.

## Release pipeline

For each Mac architecture, the **manual signing run**:

1. Requires the six credentials and compiles the publisher's Team ID into the binary as `AGX_APPLE_TEAM_ID`.
2. Runs Rust checks and adapter/signing contract tests.
3. Imports the Developer ID identity into a temporary keychain, signs `agx` with hardened runtime and a secure timestamp, and verifies Apple's certificate chain, Team ID, and identifier `dev.agentgrep.agx`.
4. Builds/signs a DMG with identifier `dev.agentgrep.agx.diskimage`, submits it to `notarytool`, and requires Apple status `Accepted`.
5. Staples the notarization ticket to the DMG, validates it, and runs Gatekeeper assessment.
6. Packages the signed binary with matching notarization metadata. Publishes both Mac DMGs, compatibility tar archives, Linux/source archives, reports, and checksums only after every platform succeeds.

Unsigned development runs always publish prereleases, even for a tag like `v0.2.1`. A successful manual signed run can promote that same immutable tag from unsigned development to signed production. Signed semantic prerelease tags remain prereleases. Published signed releases cannot be overwritten by either mode; later updates need new tags. Signed runs have no unsigned fallback. Credentials are not printed, imported keychains are deleted, and temporary key files are removed. Fork/PR validation does not receive signing secrets.

A standalone command-line executable cannot carry a stapled ticket, and tar archives cannot be stapled. **The DMG is the offline notarized delivery format.** Compatibility tar archives contain the signed executable but do not provide the stapled delivery ticket. Prefer the DMG for initial installation and updates.

## Install the first signed release

Download the architecture-specific DMG from [GitHub releases](https://github.com/ar4ft/agentgrep/releases), open it, and copy `agx` into a directory owned and writable by your user. For example, from the mounted image:

```sh
mkdir -p ~/.local/bin
install -m 755 /Volumes/agentgrep-0.2.1/agx ~/.local/bin/agx
~/.local/bin/agx doctor
```

Add `~/.local/bin` to your shell/harness PATH, or configure the absolute executable path. GUI installation from a downloaded DMG uses Gatekeeper; do not bypass its verification. Signed release builds pin the publisher's Team ID, so the updater needs no account credential or Apple private key.

The updater currently uses `xcrun stapler` as well as system `codesign`, `spctl`, and `hdiutil`. Install Apple's Command Line Tools if they are unavailable (`xcode-select --install`). A notarization-compatible macOS installation with these tools is required. Updates never invoke `sudo`; root-owned installations must be maintained by their administrator instead.

## Use the updater

```sh
agx update check                    # stable release channel, no installation
agx update install                  # verify, then install a newer signed release
agx update auto enable              # opt in to automatic checks/install every 24h
agx update auto status
agx update auto disable
agx update rollback                 # restore the exact previous binary
```

Add `--prerelease` explicitly to check/install or enable automatic updates on a prerelease channel. The default ignores draft releases, GitHub prereleases, and semantic prerelease tags. No downgrade is installed. v0.1.0 has no updater; upgrading from that version requires the first signed release's manual installation.

Source builds lack a publisher trust pin unless compiled with `AGX_APPLE_TEAM_ID`. To bootstrap one, pass `--team-id` with the publisher's **independently verified** Team ID to `install` or `auto enable`. A signed build refuses a Team ID override that differs from its embedded pin. Do not copy the trust identity from unverified update metadata.

```sh
agx update auto enable --interval-hours 24 --team-id YOURTEAMID --dry-run
```

Replace `YOURTEAMID` with the real 10-character Team ID. Dry-run renders the LaunchAgent without creating/loading it; it is available on Linux to review the Mac schedule. Linux supports release checks but currently requires manual source/package updates for installation.

## Verification and replacement

Only update commands contact GitHub. Search and MCP do not check for updates or change themselves.

The updater selects a matching architecture's exact repository DMG URL, requires GitHub's SHA-256 asset digest, limits downloads to 64 MiB over verified HTTPS, and checks the received size/hash. It verifies the DMG's Apple chain, Developer ID certificate, app identifier, and pinned Team ID, validates the stapled ticket, and asks Gatekeeper to assess it. It mounts read-only, accepts only the root regular `agx` file, then verifies the **staged copy** of that executable and its reported version before atomically replacing the installed path.

GitHub's digest detects corruption but is not the publisher trust root; Apple's verified certificate chain, app identifier, pinned Team ID, and notarization checks provide that trust. The updater has no unsigned or checksum-only fallback.

Per-executable locks prevent concurrent updates/rollback. The old executable is retained beside the installed path under a private generated filename, with hashes and versions recorded in the user-local update state directory. If the current executable or backup changes unexpectedly, rollback refuses to overwrite it. Only the most recent update is retained. Disable automatic updates before rolling back if you want to keep the older version.

The scheduler is a per-user `~/Library/LaunchAgents/dev.agentgrep.updater.plist`. It runs as a background job only while that user's GUI login session exists, checks at the chosen interval, skips prereleases by default, and does not run immediately on enable. Launchd may coalesce checks after sleep. Status reports the configured plist; it is not proof of a currently running job. Standard output/errors go into the app's private user-local `updates` directory. MCP processes already running continue to use their loaded executable until restarted.

## Validation limits

Tests cover channel selection, metadata rejection, staged verification failure, atomic replacement, rollback corruption, locks, schedule escaping, and mocked signing/notarization acceptance/rejection. Mac CI also exercises rejection of an unsigned test binary by the real `codesign` verifier. Successful real Apple signing, submission, stapling, and end-to-end signed update installation require the missing credentials and a signed release; fixtures cannot establish those results.
