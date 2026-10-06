#!/bin/sh
# GitHub-hosted, dependency-free agx installer. See docs/installation.md.
# Keep the whole program inside main so a truncated pipe cannot start installing.
main() {
    set -eu
    umask 022
    agx_prefix=${AGX_INSTALL_DIR:-"$HOME/.agx"}
    agx_version=${AGX_VERSION:-latest}
    agx_channel=published
    agx_modify_path=true
    agx_tmp=
    agx_locked=false

    fail() { printf 'agx installer: %s\n' "$*" >&2; exit 1; }
    download() {
        curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
            --tlsv1.2 --connect-timeout 15 --max-time 300 --retry 2 "$1" -o "$2"
    }
    cleanup() {
        if [ -n "$agx_tmp" ]; then rm -rf "$agx_tmp"; fi
        if [ "$agx_locked" = true ]; then rmdir "$agx_prefix/.install-lock"; fi
    }
    quote() { printf "'"; printf '%s' "$1" | sed "s/'/'\\\\''/g"; printf "'"; }
    sha256() {
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$1" | awk '{print $1}'
        else
            shasum -a 256 "$1" | awk '{print $1}'
        fi
    }
    # Parse only top-level release fields, ignoring nested assets and release-body
    # strings. Works with a release object or array and compact/pretty API JSON;
    # no jq, Python, eval, or downloaded code is needed for metadata parsing.
    releases() {
        LC_ALL=C fold -b -w 4096 "$1" | LC_ALL=C awk '
          function scalar() {
            if (depth == 1 && key == "prerelease") pre = token
            if (depth == 1 && key == "draft") draft = token
            token = ""
          }
          {
            for (i=1; i<=length($0); i++) {
              c=substr($0,i,1)
              if (str) {
                if (esc) { token=token "\\" c; esc=0 }
                else if (c == "\\") esc=1
                else if (c == "\"") {
                  str=0
                  if (value) { if (depth == 1 && key == "tag_name") tag=token }
                  else pending=token
                  token=""
                } else token=token c
                continue
              }
              if (c == "\"") { str=1; token=""; esc=0 }
              else if (c == ":") { key=pending; pending=""; value=1; token="" }
              else if (c == "{") { depth++; value=0; key=""; if (depth==1) {tag="";pre="";draft=""} }
              else if (c == "}") {
                scalar()
                if (depth==1 && tag!="") print tag "|" pre "|" draft
                depth--; value=0; key=""
              }
              else if (c == "," || c == "[" || c == "]") { scalar(); value=0; key="" }
              else if (value && c !~ /[[:space:]]/) token=token c
            }
          }
          END { if (str || depth != 0) exit 1 }
        '
    }

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version|--prefix)
                [ "$#" -ge 2 ] || fail "$1 requires a value"
                case "$1" in --version) agx_version=$2;; --prefix) agx_prefix=$2;; esac
                shift 2;;
            --stable) agx_channel=stable; shift;;
            --no-modify-path) agx_modify_path=false; shift;;
            --help|-h)
                cat <<'HELP'
Usage: install.sh [--version VERSION] [--stable] [--prefix ABSOLUTE_PATH] [--no-modify-path]

Install agx under ~/.agx (or AGX_INSTALL_DIR). AGX_VERSION selects a version.
Default: newest published GitHub release, INCLUDING development prereleases.
--stable: require a production release; fail if none exists.
--version: pin an existing version such as 0.3.1 or v0.3.1.
No sudo, Rust, model downloads, automatic updates, or agent configuration changes.
HELP
                return 0;;
            *) fail "unknown option: $1";;
        esac
    done
    case "$agx_prefix" in /*) ;; *) fail 'installation prefix must be absolute';; esac
    case "$agx_prefix" in /|*:*|*'
'*|*"$(printf '\r')"*) fail 'invalid installation prefix';; esac
    for agx_command in curl tar awk fold sed mktemp; do
        command -v "$agx_command" >/dev/null 2>&1 || fail "$agx_command is required"
    done
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || fail 'sha256sum or shasum is required'
    agx_os=$(uname -s)
    agx_arch=$(uname -m)
    case "$agx_os:$agx_arch" in
        Darwin:arm64|Darwin:aarch64) agx_target=aarch64-apple-darwin;;
        Darwin:x86_64)
            # Prefer native Apple Silicon even when this shell runs in Rosetta.
            if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
                agx_target=aarch64-apple-darwin
            else agx_target=x86_64-apple-darwin; fi;;
        Linux:x86_64) agx_target=x86_64-unknown-linux-gnu;;
        *) fail "unsupported platform: $agx_os/$agx_arch (Mac ARM/Intel and Linux x86_64 glibc are available)";;
    esac
    if [ "$agx_version" != latest ]; then
        agx_version=${agx_version#v}
        printf '%s\n' "$agx_version" | LC_ALL=C grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$' || fail 'invalid version'
    fi
    mkdir -p "$agx_prefix"
    [ ! -L "$agx_prefix/.install-lock" ] || fail 'unexpected lock symlink'
    mkdir "$agx_prefix/.install-lock" 2>/dev/null || fail "another installer is running; see $agx_prefix/.install-lock"
    agx_locked=true
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM HUP
    agx_tmp=$(mktemp -d "$agx_prefix/.install.XXXXXXXX")
    agx_api=https://api.github.com/repos/ar4ft/agentgrep/releases
    if [ "$agx_version" = latest ]; then
        if [ "$agx_channel" = stable ]; then agx_metadata_url=$agx_api/latest
        else agx_metadata_url=$agx_api'?per_page=20'; fi
    else agx_metadata_url=$agx_api/tags/v$agx_version; fi
    download "$agx_metadata_url" "$agx_tmp/release.json" || fail 'could not fetch release metadata (missing release, API rate limit, or network error)'
    releases "$agx_tmp/release.json" > "$agx_tmp/releases" || fail 'invalid GitHub release metadata'
    agx_release=$(awk -F '|' '$3 == "false" && ($2 == "true" || $2 == "false") {print; exit}' "$agx_tmp/releases")
    [ -n "$agx_release" ] || fail 'no published release found'
    agx_tag=${agx_release%%|*}
    printf '%s\n' "$agx_tag" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$' || fail 'invalid release tag in metadata'
    [ "$agx_version" = latest ] || [ "$agx_tag" = "v$agx_version" ] || fail 'release metadata version mismatch'
    case "$agx_release" in
        *'|true|false')
            [ "$agx_channel" != stable ] || fail 'requested release is a development prerelease'
            printf 'Installing development prerelease %s. Apple signing/notarization is not guaranteed.\n' "$agx_tag" >&2;;
    esac
    agx_version=${agx_tag#v}
    agx_bundle=agx-$agx_version-$agx_target
    agx_url=https://github.com/ar4ft/agentgrep/releases/download/$agx_tag/$agx_bundle
    printf 'Downloading agx %s for %s\n' "$agx_version" "$agx_target"
    download "$agx_url.tar.gz" "$agx_tmp/archive.tar.gz" || fail 'archive download failed'
    download "$agx_url.sha256" "$agx_tmp/checksum" || fail 'checksum download failed'
    agx_expected=$(awk -v name="$agx_bundle.tar.gz" 'NF==2 && $2==name && length($1)==64 && $1 !~ /[^0-9a-f]/ {print $1; count++} END {if(count!=1) exit 1}' "$agx_tmp/checksum") || fail 'invalid checksum file'
    [ "$(sha256 "$agx_tmp/archive.tar.gz")" = "$agx_expected" ] || fail 'archive SHA-256 checksum mismatch; installation unchanged'
    # Reject traversal, links, and special files BEFORE extraction. Native agx
    # packages contain only regular files (and may contain directory entries).
    tar -tzf "$agx_tmp/archive.tar.gz" > "$agx_tmp/members" || fail 'invalid archive'
    awk -v root="$agx_bundle" '
      { n=split($0,p,"/"); if(p[1]!=root) exit 1; for(i=2;i<=n;i++) if(p[i]==".." || p[i]=="." || (p[i]=="" && i<n)) exit 1; count++ }
      END {if(count==0) exit 1}
    ' "$agx_tmp/members" || fail 'unsafe archive paths'
    tar -tvzf "$agx_tmp/archive.tar.gz" > "$agx_tmp/types" || fail 'invalid archive'
    LC_ALL=C awk 'substr($0,1,1)!="-" && substr($0,1,1)!="d" {exit 1}' "$agx_tmp/types" || fail 'archive contains links or special files'
    mkdir "$agx_tmp/unpack"
    tar -xzf "$agx_tmp/archive.tar.gz" -C "$agx_tmp/unpack"
    agx_payload=$agx_tmp/unpack/$agx_bundle
    [ -f "$agx_payload/agx" ] && [ -x "$agx_payload/agx" ] && [ ! -L "$agx_payload/agx" ] || fail 'archive lacks an executable agx'
    [ -f "$agx_payload/build.json" ] || fail 'archive lacks build metadata'
    [ "$("$agx_payload/agx" --version)" = "agx $agx_version" ] || fail 'binary version/platform check failed'
    for agx_dir in bin share; do
        [ ! -L "$agx_prefix/$agx_dir" ] || fail "unexpected $agx_dir symlink"
        mkdir -p "$agx_prefix/$agx_dir"
    done
    [ ! -L "$agx_prefix/bin/agx" ] || fail 'refusing to replace an agx symlink'
    if [ -e "$agx_prefix/bin/agx" ]; then
        [ -f "$agx_prefix/bin/agx" ] && [ -f "$agx_prefix/install-receipt" ] || fail 'refusing to overwrite an unmanaged agx installation'
    fi
    for agx_output in bin/agx.previous install-receipt env; do
        [ ! -L "$agx_prefix/$agx_output" ] || fail "unexpected $agx_output symlink"
        [ ! -e "$agx_prefix/$agx_output" ] || [ -f "$agx_prefix/$agx_output" ] || fail "unexpected $agx_output directory"
    done
    [ ! -L "$agx_prefix/share/$agx_bundle" ] || fail 'unexpected package symlink'
    cp "$agx_payload/agx" "$agx_tmp/agx-next"
    chmod 755 "$agx_tmp/agx-next"
    rm "$agx_payload/agx"
    # Only documentation in an installer-owned, exact version/target directory.
    if [ ! -e "$agx_prefix/share/$agx_bundle" ]; then
        mv "$agx_payload" "$agx_prefix/share/$agx_bundle"
    fi
    if [ -f "$agx_prefix/bin/agx" ]; then
        cp -p "$agx_prefix/bin/agx" "$agx_tmp/agx-previous"
        mv -f "$agx_tmp/agx-previous" "$agx_prefix/bin/agx.previous"
    fi
    printf 'version=%s\ntarget=%s\narchive_sha256=%s\n' "$agx_version" "$agx_target" "$agx_expected" > "$agx_tmp/receipt"
    agx_path_line="export PATH=$(quote "$agx_prefix/bin"):\"\$PATH\""
    printf '%s\n' "$agx_path_line" > "$agx_tmp/env"
    # Staging is on the same filesystem; readers see a complete old/new binary.
    mv -f "$agx_tmp/agx-next" "$agx_prefix/bin/agx"
    mv -f "$agx_tmp/receipt" "$agx_prefix/install-receipt"
    mv -f "$agx_tmp/env" "$agx_prefix/env"
    if [ "$agx_modify_path" = true ]; then
        case "$agx_os:${SHELL:-}" in
            Darwin:*/zsh) agx_profile=$HOME/.zprofile;;
            Darwin:*/bash) agx_profile=$HOME/.bash_profile;;
            *:*/zsh) agx_profile=$HOME/.zshrc;;
            *:*/bash) agx_profile=$HOME/.bashrc;;
            *) agx_profile=$HOME/.profile;;
        esac
        agx_source_line=". $(quote "$agx_prefix/env") # agx installer"
        if ! grep -Fqx "$agx_source_line" "$agx_profile" 2>/dev/null; then
            printf '\n%s\n' "$agx_source_line" >> "$agx_profile" || fail "agx installed, but cannot update $agx_profile; source $agx_prefix/env manually"
        fi
        printf 'PATH configured in %s. Open a new terminal, or run:\n  . ' "$agx_profile"
        quote "$agx_prefix/env"; printf '\n'
    fi
    printf 'Installed %s/bin/agx (%s)\nDocumentation and skill: %s/share/%s\n' "$agx_prefix" "$agx_version" "$agx_prefix" "$agx_bundle"
    printf 'For GUI editors, set the explicit executable path to %s/bin/agx.\n' "$agx_prefix"
}
main "$@"
