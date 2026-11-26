#!/usr/bin/env bash
# Build and run Purrnel as a container. The image is built by Nix (nix/packages.nix), so it holds
# only Purrnel and what it needs: no distro, no shell, no package manager.
#
#   scripts/docker.sh build   build the image and load it into Docker
#   scripts/docker.sh run     run it; the UI is on http://localhost:7979 (this machine only; PURRNEL_PORT changes it)
#   scripts/docker.sh size    what is in the image and how big it is
#
# Extra arguments to `run` go to `docker run`, e.g. `-e PURRNEL_LOCATION=Auckland`.
set -euo pipefail
cd "$(dirname "$0")/.."
nix_cmd=(nix --extra-experimental-features 'nix-command flakes')
name=purrnel:latest

case "${1:-run}" in
  build)
    # A flake only sees files git knows about, so make sure new files are at least staged.
    out=$("${nix_cmd[@]}" build .#docker --no-link --print-out-paths)
    docker load < "$out"
    ;;
  size)
    docker image ls "$name"
    "${nix_cmd[@]}" path-info -rsSh .#purrnel | sort -k3 -h -r | sed -n 1,25p
    ;;
  run)
    shift || true
    # The page checks that its websocket comes from its own address, so the port must be the same
    # inside and out. Set PURRNEL_PORT to use another one.
    port="${PURRNEL_PORT:-7979}"
    args=(--rm --name purrnel
      # The harness listens on all addresses inside the container; only this machine can reach it.
      -p "127.0.0.1:$port:$port" -e "PURRNEL_PORT=$port"
      # Run as you, so the sound server accepts us and the saved key and artefacts are yours.
      --user "$(id -u):$(id -g)"
      -v purrnel-data:/data
      -e TZ="${TZ:-$(cat /etc/timezone 2>/dev/null || true)}")
    # Sound goes through the host's PulseAudio / PipeWire socket.
    sock="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pulse/native"
    if [ -S "$sock" ]; then
      args+=(-v "$sock:/run/pulse/native" -e PULSE_SERVER=unix:/run/pulse/native)
    else
      echo "purrnel: no PulseAudio socket at $sock, so there will be no sound in or out" >&2
    fi
    # The image has no group file, so give the camera's group by number.
    [ -e /dev/video0 ] && args+=(--device /dev/video0 --group-add "$(stat -c %g /dev/video0)")
    # Pass the key through if it is set; otherwise the first-run dialog asks for it.
    [ -n "${ANTHROPIC_API_KEY:-}" ] && args+=(-e ANTHROPIC_API_KEY)
    exec docker run "${args[@]}" "$@" "$name"
    ;;
  *)
    sed -n 2,10p "$0"; exit 1 ;;
esac
