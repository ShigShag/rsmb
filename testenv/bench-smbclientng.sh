#!/usr/bin/env bash
# Baseline: what one `cd` and one `ls` cost in smbclient-ng (the client rsmb replaces), at the fast
# (10.213.45.10) and the slow (10.213.45.30, 100 ms each way) endpoint. Method in docs/measuring.md:
#   time per command = (time for N commands - time for 0 commands) / N, median of REPS runs,
#   timed directly against the endpoint, never through the tap;
#   connections, requests and flights per command = the same difference, counted once through smbtap.
# Every run's output is parsed (smbclient-ng exits 0 even when the login fails), so a failed run
# stops the script instead of being timed as a fast one.
# Usage: testenv/bench-smbclientng.sh [quick]   (quick: 1 run instead of 5, no huge/ row)
set -euo pipefail
cd "$(dirname "$0")/.."

case ${1:-full} in
    full) REPS=5 ;;
    quick) REPS=1 ;;
    *) echo "usage: testenv/bench-smbclientng.sh [quick]" >&2; exit 2 ;;
esac

# Shared with the other users of the environment: whoever needs it exclusively (pause, blackhole,
# conns-reset) waits until this run is over, and this run waits for them.
exec 9>>/tmp/rsmb-env.lock
flock -s -w 1800 9 || { echo "$(basename "$0"): could not take /tmp/rsmb-env.lock within 30 minutes: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo build -q -p smbtap

# Rows: label | command run before the measured ones | measured command | N | listing lines per
# command (0 for cd; . and .. included) | working directory the last prompt must show.
ROWS=(
    "cd /tree/d0||cd /tree/d0|10|0|tree\\d0\\"
    "ls small (tree/d0, 5 entries)|cd /tree/d0|ls|10|7|tree\\d0\\"
    "ls many/ (10,000 entries)|cd /many|ls|10|10002|many\\"
)
# huge/ with N=1: one listing takes about 8 s fast and 37 s slow; ten would add about 35 minutes.
[ "$REPS" -gt 1 ] && ROWS+=("ls huge/ (100,000 entries)|cd /huge|ls|1|100002|huge\\")

die() { echo "FAIL: $*" >&2; exit 1; }

# ng <host> <port> <out> <setup> <cmd> <n>: one smbclient-ng run: `use share`, <setup>, <cmd> n
# times, then an empty line, whose prompt shows the state after the last command.
ng() {
    local host=$1 port=$2 out=$3 setup=$4 cmd=$5 n=$6 i args=(-C 'use share')
    [ -n "$setup" ] && args+=(-C "$setup")
    for ((i = 0; i < n; i++)); do args+=(-C "$cmd"); done
    args+=(-C '')
    smbclientng --no-colors -H "$host" -P "$port" -d TESTDOM -u tester -p testpass -N "${args[@]}" \
        >"$out" 2>&1 || true
}

# verify <out> <host> <setup> <n> <lines> <cwd>: the run did what it claims, or the script stops.
verify() {
    local out=$1 host=$2 setup=$3 n=$4 lines=$5 cwd=$6 got want last
    grep -qF "[+] Successfully authenticated to '$host' as 'TESTDOM\\tester'!" "$out" ||
        die "no login in $out: $(tail -n 3 "$out")"
    ! grep -qE '^\[error\]|Unknown command|Traceback|^\[x\]' "$out" ||
        die "error in $out: $(grep -m1 -E '^\[error\]|Unknown command|Traceback|^\[x\]' "$out")"
    want=$((n + 2)); [ -n "$setup" ] && want=$((want + 1))   # use share, setup, n, ''
    [ "$n" = 0 ] && [ -z "$setup" ] && cwd=''                  # nothing ran that changes directory
    got=$(grep -c '^\[v\]\[' "$out" || true)
    [ "$got" = "$want" ] || die "$got prompts in $out, expected $want"
    got=$(grep -c '^[d-][a-][c-][h-][n-][r-][s-][t-] ' "$out" || true)
    [ "$got" = $((n * lines)) ] || die "$got listing lines in $out, expected $((n * lines))"
    last=$(tail -n 1 "$out")
    [ "$last" = "[v][\\\\$host\\share\\$cwd]> " ] || die "last prompt '$last' in $out, expected ...\\share\\$cwd]>"
}

# TCP connect time in µs: median of 5 after one warm-up connect (as testenv/check.sh measures it).
connect_us() {
    local s e
    s=${EPOCHREALTIME/./}
    exec 3<>"/dev/tcp/$1/445" || return 1
    e=${EPOCHREALTIME/./}
    exec 3>&-
    echo $((e - s))
}
median() { sort -n | sed -n "$(((REPS + 1) / 2))p"; }
median_connect_us() {
    connect_us "$1" >/dev/null
    for _ in 1 2 3 4 5; do connect_us "$1"; done | sort -n | sed -n 3p
}

# The tap, as a coprocess (its file descriptors exist only in this shell, so no $(...) around q).
tap_cmd() { echo "$1" >&"${TAP[1]}"; read -r R <&"${TAP[0]}"; }
field() { [[ $1 =~ \"$2\":([0-9]+) ]] && echo "${BASH_REMATCH[1]}"; }
# tap_session <setup> <cmd> <n> <lines> <cwd>: one run through the tap; sets C Q F (connections,
# requests including encrypted ones, flights).
tap_session() {
    ng 127.0.0.1 "$port" "$work/tap.txt" "$1" "$2" "$3"
    verify "$work/tap.txt" 127.0.0.1 "$1" "$3" "$4" "$5"
    tap_cmd "settle 300 30000"
    tap_cmd stats
    C=$(field "$R" connections) F=$(field "$R" flights)
    Q=$(($(field "$R" requests) + $(field "$R" opaque_requests)))
    tap_cmd reset
}
per() { awk -v d="$1" -v n="$2" -v p="${3:-1}" 'BEGIN { printf "%.*f", p, d / n }'; }

start=$SECONDS
drops_before=$(./testenv/ctl.sh drops)   # fails the run if wan cannot be asked
fmt='%-6s %-30s %3s %10s %12s %10s %10s %10s\n'
printf "$fmt" endpoint command N connect_ms ms_per_cmd conns/cmd reqs/cmd flights/cmd
for ep in fast:10.213.45.10 slow:10.213.45.30; do
    name=${ep%%:*} host=${ep#*:}
    coproc TAP { exec target/debug/smbtap "$host:445"; }
    read -r first <&"${TAP[0]}"
    port=${first##*:} port=${port%\"\}}
    for row in "${ROWS[@]}"; do
        IFS='|' read -r label setup cmd n lines cwd <<<"$row"
        rtt=$(median_connect_us "$host")
        for ((r = 0; r < REPS; r++)); do
            t0=${EPOCHREALTIME/./}; ng "$host" 445 "$work/0.txt" "$setup" "$cmd" 0; t1=${EPOCHREALTIME/./}
            ng "$host" 445 "$work/n.txt" "$setup" "$cmd" "$n"; t2=${EPOCHREALTIME/./}
            verify "$work/0.txt" "$host" "$setup" 0 "$lines" "$cwd"
            verify "$work/n.txt" "$host" "$setup" "$n" "$lines" "$cwd"
            echo $(((t2 - t1) - (t1 - t0)))
        done >"$work/diffs"
        ms=$(per "$(median <"$work/diffs")" $((n * 1000)))
        tap_session "$setup" "$cmd" 0 "$lines" "$cwd"; C0=$C Q0=$Q F0=$F
        tap_session "$setup" "$cmd" "$n" "$lines" "$cwd"
        printf "$fmt" "$name" "$label" "$n" "$(per "$rtt" 1000 2)" "$ms" \
            "$(per $((C - C0)) "$n")" "$(per $((Q - Q0)) "$n")" "$(per $((F - F0)) "$n")"
    done
    tap_cmd quit
    wait "$TAP_PID" 2>/dev/null || true
done
drops_after=$(./testenv/ctl.sh drops)
drops=$((drops_after - drops_before))
echo
echo "smbclient-ng $(smbclientng --help 2>/dev/null | grep -o 'v[0-9][0-9.]*' | head -n 1)," \
    "Samba $(docker exec rsmb-samba smbd -V | grep -o '[0-9][0-9.]*'), $REPS run(s) per row, $(date -I), $((SECONDS - start)) s"
echo "netem drops on wan during the run: $drops"
[ "$drops" = 0 ] || die "netem dropped packets on wan; the slow-endpoint times are suspect"
