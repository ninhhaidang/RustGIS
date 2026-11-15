#!/usr/bin/env bash
# Builds the downloads.rustgis.app site for a RustGIS release: its assets up to 100 MB (checked
# against GitHub's SHA-256 digests), a manifest.json the page reads, SHA256SUMS.txt and the page.
#
#   packaging/downloads/build.sh <output-dir> [release-tag]
#
# Needs `gh` (authenticated) and `jq`. With DOWNLOAD_MIRROR_METADATA_ONLY=true it writes only
# manifest.json, so a caller can tell whether anything changed before downloading.
set -euo pipefail

readonly SOURCE_REPOSITORY="opengeos/RustGIS"
readonly MIRROR_ORIGIN="https://downloads.rustgis.app"
readonly MAX_MIRRORED_BYTES=$((100 * 1024 * 1024))
readonly METADATA_ONLY="${DOWNLOAD_MIRROR_METADATA_ONLY:-false}"
# The Store-only packages (the MSIX, the Android App Bundle for Google Play, the iOS app signed for
# the App Store: none installs on its own), and the release's own checksum files (SHA256SUMS and
# the phone packages' .sha256; the mirror writes SHA256SUMS.txt).
readonly OMITTED_ASSET_PATTERNS_JSON='[
  "\\.msix$",
  "\\.aab$",
  "\\.ipa$",
  "\\.(apk|aab|ipa)\\.sha256$",
  "^SHA256SUMS$"
]'

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "Usage: $0 OUTPUT_DIRECTORY [RELEASE_TAG]" >&2
  exit 2
fi

output_directory="$1"
release_tag="${2:-}"
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
template_directory="$script_directory"

if [[ -e "$output_directory" ]]; then
  echo "Output path already exists: $output_directory" >&2
  exit 1
fi

mkdir -p "$output_directory/artifacts"

release_json="$output_directory/.release.json"
if [[ -n "$release_tag" ]]; then
  gh api "repos/$SOURCE_REPOSITORY/releases/tags/$release_tag" > "$release_json"
else
  gh api "repos/$SOURCE_REPOSITORY/releases/latest" > "$release_json"
  release_tag="$(jq -r '.tag_name' "$release_json")"
fi

if [[ "$(jq -r '.draft' "$release_json")" == "true" ]]; then
  echo "Refusing to mirror draft release $release_tag." >&2
  exit 1
fi

generated_at="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"

jq \
  --arg generated_at "$generated_at" \
  --arg mirror_origin "$MIRROR_ORIGIN" \
  --argjson max_mirrored_bytes "$MAX_MIRRORED_BYTES" \
  --argjson omitted_asset_patterns "$OMITTED_ASSET_PATTERNS_JSON" \
  '{
    schema_version: 1,
    product: "RustGIS",
    generated_at: $generated_at,
    max_mirrored_bytes: $max_mirrored_bytes,
    release: {
      tag: .tag_name,
      name: .name,
      published_at: .published_at,
      github_url: .html_url
    },
    artifacts: [
      .assets[]
      | . as $asset
      | select(([$omitted_asset_patterns[] as $pattern | $asset.name | test($pattern)] | any) | not)
      | ($asset.size <= $max_mirrored_bytes) as $mirrored
      | {
          name: $asset.name,
          size: $asset.size,
          content_type: $asset.content_type,
          digest: $asset.digest,
          mirrored: $mirrored,
          download_url: (
            if $mirrored then
              $mirror_origin + "/artifacts/" + ($asset.name | @uri)
            else
              $asset.browser_download_url
            end
          ),
          github_url: $asset.browser_download_url
        }
    ] | sort_by(.name | ascii_downcase)
  }
  | .mirrored_bytes = ([.artifacts[] | select(.mirrored) | .size] | add // 0)
  | .mirrored_count = ([.artifacts[] | select(.mirrored)] | length)
  | .github_only_count = ([.artifacts[] | select(.mirrored | not)] | length)' \
  "$release_json" > "$output_directory/manifest.json"

if [[ "$METADATA_ONLY" == "true" ]]; then
  rm "$release_json"
  echo "Built candidate manifest for $release_tag."
  exit 0
fi

while IFS= read -r encoded_asset; do
  asset="$(base64 --decode <<< "$encoded_asset")"
  asset_id="$(jq -r '.id' <<< "$asset")"
  asset_name="$(jq -r '.name' <<< "$asset")"
  expected_size="$(jq -r '.size' <<< "$asset")"
  expected_digest="$(jq -r '.digest // empty' <<< "$asset")"
  destination="$output_directory/artifacts/$asset_name"

  echo "Downloading $asset_name"
  gh api \
    -H 'Accept: application/octet-stream' \
    "repos/$SOURCE_REPOSITORY/releases/assets/$asset_id" > "$destination"

  actual_size="$(stat -c '%s' "$destination")"
  if [[ "$actual_size" != "$expected_size" ]]; then
    echo "Size mismatch for $asset_name: expected $expected_size, got $actual_size" >&2
    exit 1
  fi

  if [[ "$expected_digest" == sha256:* ]]; then
    expected_sha256="${expected_digest#sha256:}"
    actual_sha256="$(sha256sum "$destination" | cut -d' ' -f1)"
    if [[ "$actual_sha256" != "$expected_sha256" ]]; then
      echo "SHA-256 mismatch for $asset_name" >&2
      exit 1
    fi
  fi
done < <(
  jq -r \
    --argjson max_mirrored_bytes "$MAX_MIRRORED_BYTES" \
    --argjson omitted_asset_patterns "$OMITTED_ASSET_PATTERNS_JSON" \
    '.assets[]
     | . as $asset
     | select(([$omitted_asset_patterns[] as $pattern | $asset.name | test($pattern)] | any) | not)
     | select(.size <= $max_mirrored_bytes)
     | @base64' \
    "$release_json"
)

jq -r '
  .artifacts[]
  | select((.digest // "") | startswith("sha256:"))
  | "\(.digest | sub("^sha256:"; ""))  \(.name)"
' "$output_directory/manifest.json" > "$output_directory/SHA256SUMS.txt"

cp "$template_directory/index.html" "$output_directory/index.html"
cp "$template_directory/styles.css" "$output_directory/styles.css"
cp "$template_directory/app.js" "$output_directory/app.js"
cp "$template_directory/404.html" "$output_directory/404.html"
cp "$template_directory/README.md" "$output_directory/README.md"
touch "$output_directory/.nojekyll"
printf '%s\n' 'downloads.rustgis.app' > "$output_directory/CNAME"
rm "$release_json"

echo "Built mirror for $release_tag: $(jq -r '.mirrored_count' "$output_directory/manifest.json") mirrored, $(jq -r '.github_only_count' "$output_directory/manifest.json") linked to GitHub."
