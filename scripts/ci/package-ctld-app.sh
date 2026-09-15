#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: package-ctld-app.sh CTLD_BINARY OUTPUT_APP APP_VERSION" >&2
  exit 2
fi

ctld_binary=$1
output_app=$2
app_version=$3
bundle_identifier=io.rmux.desktop.ctld
template_directory=apps/rmux/src-tauri/macos/ctld

if [ "$(uname -s)" != Darwin ]; then
  echo "ctld.app can only be packaged on macOS" >&2
  exit 2
fi

if [ ! -x "$ctld_binary" ]; then
  echo "missing ctld executable: $ctld_binary" >&2
  exit 1
fi

case "$output_app" in
  *.app) ;;
  *)
    echo "ctld output must be an app bundle: $output_app" >&2
    exit 2
    ;;
esac

case "$app_version" in
  *[!a-zA-Z0-9._+-]*|'')
    echo "invalid app version: $app_version" >&2
    exit 2
    ;;
esac

signing_identity=${APPLE_SIGNING_IDENTITY:-}
provisioning_profile=${CTLD_PROVISIONING_PROFILE:-}
if [ -n "$signing_identity" ] && [ -z "$provisioning_profile" ]; then
  echo "CTLD_PROVISIONING_PROFILE is required when APPLE_SIGNING_IDENTITY is set" >&2
  exit 2
fi
if [ -z "$signing_identity" ] && [ -n "$provisioning_profile" ]; then
  echo "APPLE_SIGNING_IDENTITY is required when CTLD_PROVISIONING_PROFILE is set" >&2
  exit 2
fi

staging_directory=$(mktemp -d "${TMPDIR:-/tmp}/ctld-app.XXXXXX")
trap 'rm -rf "$staging_directory"' EXIT HUP INT TERM
staged_app="$staging_directory/ctld.app"
mkdir -p "$staged_app/Contents/MacOS"
cp "$ctld_binary" "$staged_app/Contents/MacOS/ctld"
sed "s/@APP_VERSION@/$app_version/g" \
  "$template_directory/Info.plist" > "$staged_app/Contents/Info.plist"
plutil -lint "$staged_app/Contents/Info.plist" >/dev/null

if [ -n "$signing_identity" ]; then
  if [ ! -f "$provisioning_profile" ]; then
    echo "missing ctld provisioning profile: $provisioning_profile" >&2
    exit 1
  fi

  decoded_profile="$staging_directory/profile.plist"
  security cms -D -i "$provisioning_profile" -o "$decoded_profile"
  application_identifier=$(
    /usr/libexec/PlistBuddy \
      -c 'Print :Entitlements:com.apple.application-identifier' \
      "$decoded_profile"
  )
  team_identifier=$(
    /usr/libexec/PlistBuddy \
      -c 'Print :Entitlements:com.apple.developer.team-identifier' \
      "$decoded_profile"
  )
  expected_application_identifier="$team_identifier.$bundle_identifier"
  if [ "$application_identifier" != "$expected_application_identifier" ]; then
    echo "ctld profile authorizes $application_identifier, expected $expected_application_identifier" >&2
    exit 1
  fi
  case "$team_identifier" in
    *[!a-zA-Z0-9]*|'')
      echo "ctld profile contains an invalid team identifier" >&2
      exit 1
      ;;
  esac

  cp "$provisioning_profile" "$staged_app/Contents/embedded.provisionprofile"
  entitlements="$staging_directory/Entitlements.plist"
  sed \
    -e "s/@APPLICATION_IDENTIFIER@/$application_identifier/g" \
    -e "s/@TEAM_IDENTIFIER@/$team_identifier/g" \
    "$template_directory/Entitlements.plist" > "$entitlements"
  plutil -lint "$entitlements" >/dev/null
  codesign \
    --force \
    --options runtime \
    --timestamp \
    --entitlements "$entitlements" \
    --sign "$signing_identity" \
    "$staged_app"
  codesign --verify --strict --verbose=2 "$staged_app"

  signed_entitlements="$staging_directory/signed-entitlements.plist"
  codesign -d --entitlements :- "$staged_app" \
    > "$signed_entitlements" 2>/dev/null
  signed_application_identifier=$(
    /usr/libexec/PlistBuddy \
      -c 'Print :com.apple.application-identifier' \
      "$signed_entitlements"
  )
  signed_team_identifier=$(
    /usr/libexec/PlistBuddy \
      -c 'Print :com.apple.developer.team-identifier' \
      "$signed_entitlements"
  )
  certificate_team_identifier=$(
    codesign -d --verbose=2 "$staged_app" 2>&1 |
      sed -n 's/^TeamIdentifier=//p'
  )
  if [ "$signed_application_identifier" != "$application_identifier" ] ||
    [ "$signed_team_identifier" != "$team_identifier" ] ||
    [ "$certificate_team_identifier" != "$team_identifier" ]; then
    echo "ctld signature, certificate, and provisioning profile identities do not agree" >&2
    exit 1
  fi
fi

mkdir -p "$(dirname "$output_app")"
if [ -e "$output_app" ]; then
  rm -rf "$output_app"
fi
mv "$staged_app" "$output_app"
