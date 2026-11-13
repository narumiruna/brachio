#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf '%s\n' '--call--' "$@" >> "$root/log"
mode=normal
if [ -f "$root/mode" ]; then IFS= read -r mode < "$root/mode"; fi
if [ "$mode" = offline ]; then printf '%s\n' 'server disconnected' >&2; exit 1; fi
# Five mandatory global arguments.
[ "$1" = '--autostart=false' ] && [ "$2" = '--timeout' ] && [ "$3" = '3s' ] && [ "$4" = '--color' ] && [ "$5" = 'never' ] || exit 90
shift 5
client=
if [ "$1" = '-C' ]; then client=$2; shift 2; fi
case "$1 ${2-}" in
  'ls --json')
    if [ "$mode" = badjson ]; then printf '{broken'; else cat "$root/sessions.json"; fi ;;
  'client ls')
    case "$mode" in
      multiple) cat "$root/clients-multiple.json" ;;
      no-client) cat "$root/clients-none.json" ;;
      *) cat "$root/clients.json" ;;
    esac ;;
  'session inspect')
    case "$3" in
      session:00000000-0000-0000-0000-000000000001)
        case "$mode" in
          removed) cat "$root/view-a-removed.json" ;;
          moved) cat "$root/view-a-moved.json" ;;
          *) cat "$root/view-a.json" ;;
        esac ;;
      session:00000000-0000-0000-0000-000000000002) cat "$root/view-b.json" ;;
      *) exit 91 ;;
    esac ;;
  'block ls')
    case "$4" in
      session:00000000-0000-0000-0000-000000000001)
        case "$mode" in
          removed) cat "$root/blocks-a-removed.json" ;;
          moved) cat "$root/blocks-a-moved.json" ;;
          *) cat "$root/blocks-a.json" ;;
        esac ;;
      session:00000000-0000-0000-0000-000000000002) cat "$root/blocks-b.json" ;;
      *) exit 92 ;;
    esac ;;
  'block call')
    if [ "$mode" = no-status ] && [ "$7" = program_status ]; then printf 'not_supported' >&2; exit 1; fi
    case "$6 $7" in
      'block:00000000-0000-0000-0000-000000000001 program_status')
        if [ "$mode" = updated ]; then cat "$root/status-pi-updated.json"; else cat "$root/status-pi.json"; fi ;;
      'block:00000000-0000-0000-0000-000000000002 program_status') cat "$root/status-codex.json" ;;
      'block:00000000-0000-0000-0000-000000000005 program_status') cat "$root/status-done.json" ;;
      *' program_status') cat "$root/status-empty.json" ;;
      'block:00000000-0000-0000-0000-000000000001 process') cat "$root/process-pi.json" ;;
      'block:00000000-0000-0000-0000-000000000008 process') cat "$root/process-claude.json" ;;
      'block:00000000-0000-0000-0000-000000000005 process') cat "$root/process-exited.json" ;;
      *' process') cat "$root/process-node.json" ;;
      *) exit 93 ;;
    esac ;;
  'actions --json')
    [ -n "$client" ] || exit 94
    if [ "$mode" = unsupported ]; then printf '{"actions":[]}'; else cat "$root/actions.json"; fi ;;
  'focus -s')
    if [ "$mode" = focus-fail ]; then printf 'pane not found' >&2; exit 1; fi
    if [ "$mode" = concurrent-move ]; then cp "$root/view-a-moved.json" "$root/view-a.json"; fi ;;
  'do session.select')
    [ -n "$client" ] || exit 95
    if [ "$mode" = permission ]; then printf 'Remote Control disabled' >&2; exit 1; fi ;;
  'events --all')
    previous=
    while :; do
      if [ -f "$root/mode" ]; then IFS= read -r mode < "$root/mode"; fi
      if [ "$mode" = offline ]; then printf 'stream disconnected' >&2; exit 1; fi
      if [ "$mode" != "$previous" ] && [ ! -f "$root/quiet-events" ]; then
        printf '%s\n' '{"type":"session_event","payload":{"session_id":"session:00000000-0000-0000-0000-000000000001","event":{"type":"block_event","payload":{"name":"program_status_changed"}}}}'
        previous=$mode
      fi
      sleep 0.05
    done ;;
  *) printf 'unexpected command' >&2; exit 99 ;;
esac
