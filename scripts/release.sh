#!/usr/bin/env bash
set -euo pipefail

# Simple release builder for qqqa
# - Bumps Cargo.toml version
# - Builds binaries for common targets
# - Packages tar.gz artifacts under target/releases/v<version>/
# - Writes checksums and a minimal manifest.json (also under target/releases)
# - Optionally tags the repo at a given SHA
#
# Usage:
#   scripts/release.sh v0.7.0 [<git_sha>] [TARGETS="..."]
#
# Defaults TARGETS cover macOS, Linux MUSL, and Windows:
#   - macOS/Linux hosts: macOS (x86_64/aarch64), Linux MUSL (x86_64/aarch64), Windows (x86_64/aarch64 via MinGW, zipped)
#   - Windows hosts: Windows MSVC (x86_64/aarch64, zipped) plus Linux MUSL (x86_64/aarch64 tarballs)
#
# You can override with TARGETS to include cross targets, but ensure
# cross C toolchains exist (e.g., aarch64-linux-musl-gcc or aarch64-linux-gnu-gcc).

root_dir=$(cd "$(dirname "$0")/.." && pwd)
cd "$root_dir"

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 v<version> [git_sha]" >&2
  exit 1
fi

ver_in=$1
ver_raw=${ver_in#v}
# Normalize to semver (major.minor.patch); if only major.minor provided, append .0
if [[ "$ver_raw" =~ ^[0-9]+\.[0-9]+$ ]]; then
  ver="${ver_raw}.0"
else
  ver="$ver_raw"
fi
git_sha=${2:-}

# Compute sensible OS-aware defaults to avoid accidental cross builds
host_os=$(uname -s)
case "$host_os" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    windows_host=1
    ;;
  *)
    windows_host=0
    ;;
esac

if [[ $windows_host -eq 1 ]]; then
  targets_default=(
    x86_64-pc-windows-msvc
    aarch64-pc-windows-msvc
    x86_64-unknown-linux-musl
    aarch64-unknown-linux-musl
  )
else
  targets_default=(
    x86_64-apple-darwin
    aarch64-apple-darwin
    x86_64-unknown-linux-musl
    aarch64-unknown-linux-musl
    x86_64-pc-windows-gnu
    aarch64-pc-windows-gnullvm
  )
fi

IFS=' ' read -r -a targets <<< "${TARGETS:-${targets_default[*]}}"

required_targets_default=(
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
)

IFS=' ' read -r -a required_targets <<< "${REQUIRED_TARGETS:-${required_targets_default[*]}}"

# Ensure required targets are always present in the build list
for rt in "${required_targets[@]}"; do
  [[ -n "$rt" ]] || continue
  found=0
  for t in "${targets[@]}"; do
    if [[ "$t" == "$rt" ]]; then
      found=1
      break
    fi
  done
  if [[ $found -eq 0 ]]; then
    targets+=("$rt")
  fi
done

is_required_target() {
  local needle="$1"
  for rt in "${required_targets[@]}"; do
    [[ -n "$rt" ]] || continue
    if [[ "$rt" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

echo "==> Releasing version ${ver}"

# Bump Cargo.toml version (in-place) in a portable way
if command -v gsed >/dev/null 2>&1; then
  gsed -i -E "s/^version = \"[0-9].*\"/version = \"${ver}\"/" Cargo.toml
else
  if [[ "$(uname -s)" == "Darwin" ]]; then
    sed -i '' -E "s/^version = \"[0-9].*\"/version = \"${ver}\"/" Cargo.toml
  else
    sed -i -E "s/^version = \"[0-9].*\"/version = \"${ver}\"/" Cargo.toml
  fi
fi

echo "==> Building targets: ${targets[*]}"

# Per-version directory for artifacts and manifest
artifact_root="${root_dir}/target/releases/v${ver}"
mkdir -p "$artifact_root"
manifest_dir="$artifact_root"

for t in "${targets[@]}"; do
  echo "--> Target: $t"
  export PKG_CONFIG_ALLOW_CROSS=1
  cc_tool=""
  linker_tool=""
  arch_prefix=""
  if [[ "$t" == *"-unknown-linux-gnu"* ]]; then
    # GNU libc toolchain
    arch_prefix=$(echo "$t" | sed -E 's/-unknown-linux-gnu$//')
    cc_tool="${arch_prefix}-linux-gnu-gcc"
    linker_tool="$cc_tool"
    if ! command -v "$cc_tool" >/dev/null 2>&1; then
      echo "    WARN: Missing cross C toolchain: $cc_tool (required by ring)." >&2
      if [[ "$host_os" == "Darwin" ]]; then
        echo "          Tip: prefer MUSL targets on macOS (install: brew install FiloSottile/musl-cross/musl-cross)" >&2
      fi
      if is_required_target "$t"; then
        echo "          ERROR: target $t is required; install the toolchain or override REQUIRED_TARGETS." >&2
        exit 1
      fi
      echo "          Skipping $t." >&2
      continue
    fi
  elif [[ "$t" == *"-unknown-linux-musl"* ]]; then
    # MUSL toolchain is best for portable Linux binaries
    arch_prefix=$(echo "$t" | sed -E 's/-unknown-linux-musl$//')
    cc_tool="${arch_prefix}-linux-musl-gcc"
    linker_tool="$cc_tool"
    if ! command -v "$cc_tool" >/dev/null 2>&1; then
      echo "    WARN: Missing MUSL cross toolchain: $cc_tool (required by ring)." >&2
      if [[ "$host_os" == "Darwin" ]]; then
        echo "          Install via Homebrew: brew install FiloSottile/musl-cross/musl-cross" >&2
      fi
      if is_required_target "$t"; then
        echo "          ERROR: target $t is required; install the toolchain or override REQUIRED_TARGETS." >&2
        exit 1
      fi
      echo "          Skipping $t." >&2
      continue
    fi
  elif [[ "$t" == *"-apple-darwin"* && "$host_os" != "Darwin" ]]; then
    # Cross-compiling to macOS from Linux typically requires osxcross
    # Try to find an osxcross clang for the architecture
    arch_prefix=$(echo "$t" | sed -E 's/-apple-darwin$//')
    # Probe PATH for any matching clang
    found=""
    IFS=: read -ra pths <<< "$PATH"
    for d in "${pths[@]}"; do
      for f in "$d/${arch_prefix}-apple-darwin"*-clang; do
        [[ -x "$f" ]] || continue
        found="$f"; break
      done
      [[ -n "$found" ]] && break
    done
    if [[ -z "$found" ]]; then
      echo "    WARN: No osxcross clang found for $t. Install osxcross and ensure <arch>-apple-darwin*-clang is on PATH. Skipping $t." >&2
      continue
    fi
    cc_tool="$found"
    linker_tool="$found"
  elif [[ "$t" == *"-pc-windows-gnullvm"* ]]; then
    arch_prefix=$(echo "$t" | sed -E 's/-pc-windows-gnullvm$//')
    cc_tool="${arch_prefix}-w64-mingw32-clang"
    linker_tool="$cc_tool"
    # Ring's build.rs expects to find the clang binary on PATH; mimic the manual
    # steps (PATH + CC_aarch64_pc_windows_gnullvm) we use when building locally.
    if [[ "$t" == "aarch64-pc-windows-gnullvm" && -d "/opt/llvm-mingw/bin" ]]; then
      export PATH="/opt/llvm-mingw/bin:$PATH"
    fi
    if ! command -v "$cc_tool" >/dev/null 2>&1; then
      echo "    WARN: Missing LLVM MinGW toolchain: $cc_tool." >&2
      if is_required_target "$t"; then
        echo "          ERROR: target $t is required; install llvm-mingw ($cc_tool) or override REQUIRED_TARGETS." >&2
        exit 1
      fi
      echo "          Skipping $t." >&2
      continue
    fi
  elif [[ "$t" == *"-pc-windows-gnu"* ]]; then
    arch_prefix=$(echo "$t" | sed -E 's/-pc-windows-gnu$//')
    cc_tool="${arch_prefix}-w64-mingw32-gcc"
    linker_tool="$cc_tool"
    if ! command -v "$cc_tool" >/dev/null 2>&1; then
      echo "    WARN: Missing MinGW cross toolchain: $cc_tool." >&2
      if is_required_target "$t"; then
        echo "          ERROR: target $t is required; install mingw-w64 ($cc_tool) or override REQUIRED_TARGETS." >&2
        exit 1
      fi
      echo "          Skipping $t." >&2
      continue
    fi
  elif [[ "$t" == *"-pc-windows-msvc"* && $windows_host -eq 0 ]]; then
    echo "    WARN: $t requires MSVC toolchains and can only be built on Windows hosts. Skipping." >&2
    continue
  fi

  # If we have a cc/linker tool for this target, set env vars cargo/cc understands
  if [[ -n "$cc_tool" ]]; then
    target_key=$(echo "${t//-/_}" | tr '[:lower:]' '[:upper:]')
    cc_var="CC_${target_key}"
    linker_var="CARGO_TARGET_${target_key}_LINKER"
    # shellcheck disable=SC2163
    export "$cc_var"="$cc_tool"
    # shellcheck disable=SC2163
    export "$linker_var"="$linker_tool"
    if [[ "$t" == "aarch64-pc-windows-gnullvm" ]]; then
      export CC_aarch64_pc_windows_gnullvm="$cc_tool"
    fi
    echo "    Using CC via $cc_var=$cc_tool and $linker_var=$linker_tool"
  fi
  if ! rustup target list | grep -q "^${t} (installed)"; then
    echo "    Installing target $t via rustup..."
    rustup target add "$t"
  fi
  if ! RUSTFLAGS="${RUSTFLAGS:-}" cargo build --release --locked --target "$t"; then
    echo "    ERROR: build failed for $t" >&2
    if is_required_target "$t"; then
      exit 1
    fi
    echo "    WARN: skipping packaging for non-required target $t" >&2
    continue
  fi

  outdir="${manifest_dir}/${t}"
  mkdir -p "$outdir"
  bin_dir="target/${t}/release"
  for bin in qq qa; do
    if [[ -f "${bin_dir}/${bin}.exe" ]]; then
      cp -f "${bin_dir}/${bin}.exe" "${outdir}/${bin}.exe" || true
    elif [[ -f "${bin_dir}/${bin}" ]]; then
      cp -f "${bin_dir}/${bin}" "${outdir}/${bin}" || true
    else
      echo "    WARN: ${bin} binary not found for $t (looked in ${bin_dir})." >&2
    fi
  done
  # Include basic docs
  cp -f README.md "$outdir/README.md" || true
  cp -f LICENSE "$outdir/" 2>/dev/null || true

  if [[ "$t" == *"windows"* ]]; then
    archive="${artifact_root}/qqqa-v${ver}-${t}.zip"
    if command -v zip >/dev/null 2>&1; then
      (cd "$outdir" && zip -qr "$archive" .)
    else
      echo "    WARN: zip not found; falling back to tar.gz for $t" >&2
      archive="${artifact_root}/qqqa-v${ver}-${t}.tar.gz"
      tar -C "$outdir" -czf "$archive" . 2>/dev/null || tar -C "$outdir" -czf "$archive" .
    fi
  else
    archive="${artifact_root}/qqqa-v${ver}-${t}.tar.gz"
    tar -C "$outdir" -czf "$archive" . 2>/dev/null || {
      echo "    Packaging whatever binaries are present for $t"
      tar -C "$outdir" -czf "$archive" .
    }
  fi

  # Cleanup per-target staging directory after packaging
  rm -rf "$outdir"
done

missing_required=0
for rt in "${required_targets[@]}"; do
  [[ -n "$rt" ]] || continue
  if [[ "$rt" == *"windows"* ]]; then
    artifact_path="${artifact_root}/qqqa-v${ver}-${rt}.zip"
  else
    artifact_path="${artifact_root}/qqqa-v${ver}-${rt}.tar.gz"
  fi
  if [[ ! -f "$artifact_path" ]]; then
    echo "==> ERROR: required artifact not produced: ${artifact_path}" >&2
    missing_required=1
  fi
done
if [[ $missing_required -ne 0 ]]; then
  echo "==> Aborting release because required targets failed to build." >&2
  exit 1
fi

# Pack source tarball for Homebrew / manual uploads
src_stage=$(mktemp -d)
src_dir="qqqa-${ver}"
rsync -a --exclude='.git' --exclude='target' --exclude='.DS_Store' ./ "${src_stage}/${src_dir}/"
src_tarball="${artifact_root}/qqqa-v${ver}-src.tar.gz"
tar -C "$src_stage" -czf "$src_tarball" "$src_dir"
rm -rf "$src_stage"
src_sha=$(shasum -a 256 "$src_tarball" | awk '{print $1}')

# Checksums and manifest
checksum_cmd=""
if command -v shasum >/dev/null 2>&1; then
  checksum_cmd="shasum -a 256"
elif command -v sha256sum >/dev/null 2>&1; then
  checksum_cmd="sha256sum"
fi

manifest_file="${artifact_root}/manifest.json"
date_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)

echo "==> Writing manifest ${manifest_file}"
{
  echo "{"
  echo "  \"version\": \"${ver}\"," 
  echo "  \"date\": \"${date_iso}\"," 
  if [[ -n "$git_sha" ]]; then
    echo "  \"git_sha\": \"${git_sha}\"," 
  fi
  echo "  \"artifacts\": ["
  first=1
  for f in "${manifest_dir}"/qqqa-v${ver}-*.tar.gz "${manifest_dir}"/qqqa-v${ver}-*.zip; do
    [[ -e "$f" ]] || continue
    name=$(basename "$f")
    csum=""
    if [[ -n "$checksum_cmd" ]]; then
      csum=$($checksum_cmd "$f" | awk '{print $1}')
    fi
    if [[ $first -eq 0 ]]; then echo ","; fi
    first=0
    echo -n "    { \"file\": \"${name}\""
    if [[ -n "$csum" ]]; then
      echo -n ", \"sha256\": \"${csum}\""
    fi
    echo -n " }"
  done
  echo
  echo "  ]"
  echo "}"
} > "$manifest_file"

echo "==> Done. Artifacts staged under ${artifact_root}/"
echo "    Upload the following files to your GitHub release/tag:"
for f in "${artifact_root}"/qqqa-v${ver}-*.tar.gz "$manifest_file"; do
  [[ -e "$f" ]] || continue
  echo "      - $f"
done
echo "    Attach them when running: gh release create v${ver} ..."
echo
echo "==> Next steps"
cat <<EOF
  1. Review README/docs for accuracy (update manually if needed).
  2. Commit the version bump (artifacts stay under target/ and remain ignored):
       git add Cargo.toml
       git commit -m 'release: v${ver}'
  3. Tag the release:
EOF
if [[ -n "$git_sha" ]]; then
  echo "       git tag -a v${ver} ${git_sha} -m 'qqqa v${ver}'"
else
  echo "       git tag -a v${ver} -m 'qqqa v${ver}'"
fi
cat <<EOF
  4. Push code and tag:
       git push && git push --tags
  5. Publish (or update) the GitHub release with the artifacts listed above if you haven't already.
  6. Announce the release / publish notes as needed.
EOF
