#!/usr/bin/env bash
# The container's startup script (nix/packages.nix packages it as the image's entrypoint).
#
# Purrnel needs an Anthropic API key and, for the weather in the window, a place. Each one is taken
# from the first of these that exists:
#
#   API key    ANTHROPIC_API_KEY, the file named by ANTHROPIC_API_KEY_FILE,
#              /run/secrets/anthropic_api_key (a Docker secret), or $PURRNEL_CONFIG/api_key
#   location   PURRNEL_LOCATION, the file named by PURRNEL_LOCATION_FILE,
#              /run/secrets/purrnel_location, or $PURRNEL_CONFIG/location
#
# If something is still missing and the container has a terminal (docker run -it), it asks once and
# saves the answer in $PURRNEL_CONFIG (the /data volume), mode 600. Without a terminal it carries on,
# and the page's first-run dialog asks instead.

cfg="${PURRNEL_CONFIG:-${HOME:-/data}/.config/purrnel}"
mkdir -p "$cfg"
chmod 700 "$cfg" 2>/dev/null || true

say() { printf 'purrnel: %s\n' "$*" >&2; }

# Contents of a file, or fail with a clear message if it was named but cannot be read.
from_file() { # $1 = what it is, $2 = path
  if [ ! -r "$2" ]; then
    say "$1 file '$2' is not readable"
    exit 1
  fi
  cat "$2"
}

save() { # $1 = name, $2 = value; the file is private to whoever runs the container
  ( umask 077; printf '%s\n' "$2" > "$cfg/$1" )
}

# --- values that were handed to us ---------------------------------------------------------------

if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
  ANTHROPIC_API_KEY=
  if [ -n "${ANTHROPIC_API_KEY_FILE:-}" ]; then
    ANTHROPIC_API_KEY=$(from_file "API key" "$ANTHROPIC_API_KEY_FILE")
  elif [ -r /run/secrets/anthropic_api_key ]; then
    ANTHROPIC_API_KEY=$(cat /run/secrets/anthropic_api_key)
  fi
  ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY//[[:space:]]/}
  if [ -n "$ANTHROPIC_API_KEY" ]; then export ANTHROPIC_API_KEY; else unset ANTHROPIC_API_KEY; fi
fi

if [ -z "${PURRNEL_LOCATION:-}" ]; then
  PURRNEL_LOCATION=
  if [ -n "${PURRNEL_LOCATION_FILE:-}" ]; then
    PURRNEL_LOCATION=$(from_file "location" "$PURRNEL_LOCATION_FILE")
  elif [ -r /run/secrets/purrnel_location ]; then
    PURRNEL_LOCATION=$(cat /run/secrets/purrnel_location)
  fi
  PURRNEL_LOCATION=${PURRNEL_LOCATION//[[:cntrl:]]/}
  if [ -n "$PURRNEL_LOCATION" ]; then export PURRNEL_LOCATION; else unset PURRNEL_LOCATION; fi
fi

# --- what is still missing -----------------------------------------------------------------------

need_key=
[ -z "${ANTHROPIC_API_KEY:-}" ] && [ ! -s "$cfg/api_key" ] && need_key=1

need_place=
if [ "${PURRNEL_WEATHER:-1}" != 0 ] && [ -z "${PURRNEL_LOCATION:-}" ] && [ -z "${PURRNEL_LAT:-}" ] \
  && [ ! -s "$cfg/location" ]; then
  need_place=1
fi

# Ask only the first time (a skipped question is not asked again on every start).
if [ -t 0 ] && [ -z "$need_key$need_place" ]; then
  :
elif [ -t 0 ] && [ ! -e "$cfg/.asked" ]; then
  echo
  echo "  Hello! A couple of things before the cat wakes up (Enter skips; you can also"
  echo "  set them later from the settings button on the page)."
  echo

  if [ -n "$need_key" ]; then
    echo "  Anthropic API key (console.anthropic.com). Typing is hidden."
    while :; do
      read -r -s -p "  API key: " key || key=
      echo
      key=${key//[[:space:]]/}
      if [ -z "$key" ]; then break; fi
      if [ "${#key}" -ge 8 ] && [ "${#key}" -le 300 ] && [[ "$key" =~ ^[[:graph:]]+$ ]]; then
        save api_key "$key"
        break
      fi
      echo "  That does not look like a key; try again, or press Enter to skip."
    done
  fi

  if [ -n "$need_place" ]; then
    read -r -p "  Where do you live, for the weather in the window (e.g. Auckland)? " place || place=
    place=${place//[[:cntrl:]]/}
    place=${place:0:80}
    place=${place#"${place%%[![:space:]]*}"}
    [ -n "$place" ] && save location "$place"
  fi

  echo
  ( umask 077; : > "$cfg/.asked" )
else
  [ -n "$need_key" ] && say "no API key: set ANTHROPIC_API_KEY (or ANTHROPIC_API_KEY_FILE), or enter it on the page"
  [ -n "$need_place" ] && say "no location, so no weather: set PURRNEL_LOCATION, or enter it on the page"
fi

exec purrnel "$@"
