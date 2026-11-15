#!/usr/bin/env bash
# Play every scenario in crates/tresse-gui/tests/ui/ into the real
# application and fail if one of them does not come out the way it says it
# should.
#
#   build-aux/ui-tests.sh                 on the display you are sitting at
#   build-aux/ui-tests.sh --headless      under a private headless sway
#   build-aux/ui-tests.sh [--headless] phone new-expense   only these
#
# A scenario is a TRESSE_GUI_SCRIPT (crates/tresse-gui/src/script.rs)
# kept in a file, one step per line, with what it needs said in its header:
#
#   # mode: fresh                     an installation with nothing set up yet
#   # folder: yes                     give the run a Tresse repository to add
#   # size: 1240x800          the window it was written against
#   # input: touch            optional: drawn for a finger, as on a phone
#   # restart: wait:2000;expect:account=Cash;quit   optional second launch
#
# It passes when the application reaches the script's `quit` and exits with
# status 0: an `expect:` step that does not hold exits with 1, a crash exits
# with something else, and a script that never gets to its end runs into the
# timeout.
#
# Clicks are coordinates inside the window, so they only mean something at
# the size the scenario names. A tiling compositor gives a window the size it
# likes, not the one asked for; that is what --headless is for, and it is how
# CI runs these. Without it, scenarios that click may fail on your desktop
# while the keyboard-only ones still hold.
#
# Every run gets its own empty XDG directories, so a scenario never sees your
# folders or your settings, and no scenario sees what another left.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

scenarios_dir=crates/tresse-gui/tests/ui
app=${TRESSE_GUI_BIN:-target/debug/tresse-gui}
timeout_s=${UI_TEST_TIMEOUT:-90}

headless=false
if [[ ${1:-} == --headless ]]; then
  shift
  headless=true
  runtime=$(mktemp -d)
  chmod 700 -- "$runtime"
  export XDG_RUNTIME_DIR=$runtime
  unset DISPLAY WAYLAND_DISPLAY
  # sway on wlroots' headless backend. It tiles, which here is the point: with
  # no borders and no bar the one window is exactly as large as the output,
  # and the output can be resized per scenario. (weston's headless backend
  # has no wl_seat at all, and the toolkit will not start without one.)
  # pixman, so the compositor needs no GPU; the application draws itself
  # through whatever Vulkan driver there is — lavapipe in CI.
  # --unsupported-gpu: sway refuses to start when the kernel has Nvidia's
  # module loaded, and a container sees its host's modules. Nothing here
  # touches the GPU, so the refusal is about a driver that is never used.
  printf '%s\n' 'default_border none' 'default_floating_border none' \
    'focus_follows_mouse no' > "$runtime/sway.conf"
  WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway --unsupported-gpu -c "$runtime/sway.conf" >"$runtime/sway.log" 2>&1 &
  compositor_pid=$!
  for _ in $(seq 100); do
    socket=$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit)
    [[ -n $socket ]] && break
    sleep 0.1
  done
  [[ -n ${socket:-} ]] || { cat "$runtime/sway.log" >&2; exit 1; }
  export WAYLAND_DISPLAY=${socket##*/}
  SWAYSOCK=$(find "$runtime" -maxdepth 1 -name 'sway-ipc.*' -print -quit)
  export SWAYSOCK
fi

[[ -x $app ]] || cargo build -p tresse-gui

# No scenarios yet: the stage that brings the script engine adds them. Until
# then there is nothing to play and nothing to fail.
shopt -s nullglob
if (( $# )); then
  files=()
  for name in "$@"; do files+=("$scenarios_dir/$name.script"); done
else
  files=("$scenarios_dir"/*.script)
fi
if (( ${#files[@]} == 0 )); then
  echo "no UI scenarios in $scenarios_dir yet"
  exit 0
fi

work=$(mktemp -d)
cleanup() {
  [[ -z ${compositor_pid:-} ]] || kill "$compositor_pid" 2>/dev/null || true
  rm -rf -- "$work" ${runtime:+"$runtime"}
}
trap cleanup EXIT

header() { sed -n "s/^# *$2: *//p" "$1" | head -n1; }

failed=()
for file in "${files[@]}"; do
  name=$(basename -- "$file" .script)
  [[ -f $file ]] || { echo "no such scenario: $name" >&2; exit 2; }
  mode=$(header "$file" mode)
  size=$(header "$file" size)
  # The steps: every line that is not blank or a comment, joined by `;`.
  script=$(grep -v -E '^\s*(#|$)' "$file" | paste -sd ';')

  home=$work/$name
  mkdir -p -- "$home"
  env=(
    XDG_CONFIG_HOME="$home/config" XDG_DATA_HOME="$home/data"
    TRESSE_GUI_SIZE="${size:-1240x800}" TRESSE_GUI_SCRIPT="$script"
  )
  [[ $(header "$file" input) == touch ]] && env+=(TRESSE_GUI_INPUT=touch)
  case $mode in
    fresh) ;;
    *) echo "$name: header must say '# mode: fresh' (a folder fixture is '# folder: yes')" >&2; exit 2 ;;
  esac
  # A folder the scenario is about, already a Tresse repository: the picker
  # has no hand to answer it in a scripted run, so the folder is named.
  if [[ $(header "$file" folder) == yes ]]; then
    mkdir -p -- "$home/folder/.tresse"
    printf '[languages.markdown]\nstrategy = "tree-sitter"\n' > "$home/folder/tresse.toml"
    env+=(TRESSE_GUI_FOLDER="$home/folder")
  fi

  $headless && swaymsg -q output HEADLESS-1 resolution "${size:-1240x800}"

  status=0
  env "${env[@]}" timeout "$timeout_s" "$app" >"$home/app.log" 2>&1 || status=$?

  restart=$(header "$file" restart)
  if (( status == 0 )) && [[ -n $restart ]]; then
    env "${env[@]}" TRESSE_GUI_SCRIPT="$restart" \
      timeout "$timeout_s" "$app" >"$home/restart.log" 2>&1 || status=$?
    if (( status != 0 )); then
      cat "$home/restart.log" >> "$home/app.log"
    fi
  fi

  if (( status == 0 )); then
    echo "ok    $name"
  else
    case $status in
      124) why="did not finish in ${timeout_s}s" ;;
      1)   why=$(grep -m1 '^script: expected' "$home/app.log" || echo 'exit status 1') ;;
      *)   why="exit status $status" ;;
    esac
    echo "FAIL  $name — $why"
    # An annotation is readable from the run's checks without log rights.
    echo "::error title=UI scenario $name::$why"
    grep -E 'panicked|ERROR' "$home/app.log" | head -n 5 | sed 's/^/        /' || true
    failed+=("$name")
  fi
done

echo
if (( ${#failed[@]} )); then
  echo "${#failed[@]} of ${#files[@]} failed: ${failed[*]}"
  # The job log needs rights to read; the summary shows in the run's overview.
  if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    printf '### UI scenarios\n\n' >> "$GITHUB_STEP_SUMMARY"
    printf '%s\n' "${failed[@]/#/- }" >> "$GITHUB_STEP_SUMMARY"
  fi
  exit 1
fi
echo "all ${#files[@]} passed"
