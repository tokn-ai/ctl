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
template_directory=apps/desktop/src-tauri/macos/ctld

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

provisioning_profile=${CTLD_PROVISIONING_PROFILE:-}

staging_directory=$(mktemp -d "${TMPDIR:-/tmp}/ctld-app.XXXXXX")
trap 'rm -rf "$staging_directory"' EXIT HUP INT TERM
staged_app="$staging_directory/ctld.app"
mkdir -p "$staged_app/Contents/MacOS"
cp "$ctld_binary" "$staged_app/Contents/MacOS/ctld"
sed "s/@APP_VERSION@/$app_version/g" \
  "$template_directory/Info.plist" > "$staged_app/Contents/Info.plist"
plutil -lint "$staged_app/Contents/Info.plist" >/dev/null

if [ -n "$provisioning_profile" ]; then
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

  get_task_allow=$(
    /usr/libexec/PlistBuddy \
      -c 'Print :Entitlements:get-task-allow' \
      "$decoded_profile" 2>/dev/null || printf 'false\n'
  )
  signing_identity=
  valid_signing_identities=$(
    security find-identity -v -p codesigning |
      awk '/^[[:space:]]*[0-9]+\)/ { print $2 }'
  )
  certificate_index=0
  while certificate_base64=$(
    plutil -extract "DeveloperCertificates.$certificate_index" raw -o - \
      "$decoded_profile" 2>/dev/null
  ); do
    certificate="$staging_directory/profile-certificate-$certificate_index.der"
    printf '%s' "$certificate_base64" | base64 --decode -o "$certificate"
    certificate_hash=$(shasum -a 1 "$certificate" | awk '{ print toupper($1) }')
    if printf '%s\n' "$valid_signing_identities" | grep -Fqx "$certificate_hash"; then
      signing_identity=$certificate_hash
      break
    fi
    certificate_index=$((certificate_index + 1))
  done
  if [ -z "$signing_identity" ]; then
    echo "no valid signing identity matches ctld's provisioning profile" >&2
    exit 1
  fi

  cp "$provisioning_profile" "$staged_app/Contents/embedded.provisionprofile"
  entitlements="$staging_directory/Entitlements.plist"
  sed \
    -e "s/@APPLICATION_IDENTIFIER@/$application_identifier/g" \
    -e "s/@TEAM_IDENTIFIER@/$team_identifier/g" \
    "$template_directory/Entitlements.plist" > "$entitlements"
  plutil -lint "$entitlements" >/dev/null
  if [ "$get_task_allow" = true ]; then
    codesign \
      --force \
      --options runtime \
      --entitlements "$entitlements" \
      --sign "$signing_identity" \
      "$staged_app"
  else
    codesign \
      --force \
      --options runtime \
      --timestamp \
      --entitlements "$entitlements" \
      --sign "$signing_identity" \
      "$staged_app"
  fi
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
  if [ -n "${CTLD_SIGNING_IDENTITY_OUTPUT:-}" ]; then
    printf '%s\n' "$signing_identity" > "$CTLD_SIGNING_IDENTITY_OUTPUT"
  fi
fi

mkdir -p "$(dirname "$output_app")"
if [ -e "$output_app" ]; then
  rm -rf "$output_app"
fi
mv "$staged_app" "$output_app"
