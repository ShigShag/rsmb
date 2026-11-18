#!/usr/bin/env bash
# rsmb against the smbclient-ng baseline: the rows and the method of testenv/bench-smbclientng.sh
# (docs/measuring.md, docs/performance.md), for target/release/rsmb. Not part of the gate.
#   time per command = median over REPS runs of (t_N - t_0) / N, direct to the endpoint;
#   connections, requests (named + encrypted) and flights per command = the same difference,
#   counted once through smbtap. A row is void, and the script stops, unless its tapped run shows
#   the requests the command must send (2 x N for cd, 5 x N for ls): a command that sent nothing
#   must not be published as a fast one.
#   At the slow endpoint the ls rows add "fresh" = t_1 - t_0 (the first listing of a new
#   connection, TCP slow start) and "paused" = the N commands fed over stdin with 1 s of quiet
#   before each, (t_N - t_0 - N s) / N.
# The smbclient-ng columns come from testenv/baseline-smbclientng.txt, the output of the last full
# ./testenv/bench-smbclientng.sh run (renew: ./testenv/bench-smbclientng.sh | tee that file).
# Usage: testenv/bench-rsmb.sh [quick|transfers|walk]   (quick: 1 run per row, no huge/ rows, no
# wan-bad row; transfers: the transfer rows of docs/plans/stage-2.md 6.5 and the 50 MiB rows
# beside smbclient-ng of docs/plans/stage-4.md 6.6, about 27 minutes, under the exclusive lock; walk: the tree and find rows of docs/plans/stage-3.md 6.5, about 6 minutes,
# under the shared lock)
set -euo pipefail
cd "$(dirname "$0")/.."

case ${1:-full} in
    full) REPS=5 ;;
    quick) REPS=1 ;;
    transfers) REPS=0 ;;
    walk) REPS=-1 ;;
    *) echo "usage: testenv/bench-rsmb.sh [quick|transfers|walk]" >&2; exit 2 ;;
esac

exec 9>>/tmp/rsmb-env.lock

# walk: the rows of docs/plans/stage-3.md 6.5 (docs/performance.md). Shared lock: it changes
# nothing. tree /tree/d0 beside smbclient-ng: one process per run, (time with the command - time
# without it), counts through the tap in a further run each; the other rows are the product's own
# walk_summary.seconds (from the first listing to the last answer).
if [ "$REPS" = -1 ]; then
    flock -s -w 1800 9 || { echo "$(basename "$0"): could not take /tmp/rsmb-env.lock within 30 minutes: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2; exit 1; }
    cargo build -q --release -p rsmb -p smbtap
    RSMB=target/release/rsmb
    LOGIN=(-u tester -p testpass -d TESTDOM)
    SLOW=10.213.45.30 BAD=10.213.45.31
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    start=$SECONDS
    drops_before=$(./testenv/ctl.sh drops)
    die() { echo "FAIL: $*" >&2; exit 1; }
    us() { echo "${EPOCHREALTIME/./}"; }
    secs() { awk -v d="$1" 'BEGIN { printf "%.2f", d / 1e6 }'; }
    connect_ms() {
        local s e
        connect_us() { s=$(us); exec 3<>"/dev/tcp/$1/445"; e=$(us); exec 3>&-; echo $((e - s)); }
        connect_us "$1" >/dev/null
        for _ in 1 2 3 4 5; do connect_us "$1"; done | sort -n | sed -n 3p | awk '{ printf "%.1f", $1 / 1000 }'
    }
    rt=$(connect_ms "$SLOW")
    times_rt() { awk -v rt="$rt" -v n="$1" 'BEGIN { printf "%.1f", n * rt / 1000 }'; }
    # rs <host> <port> <out> [line]: one rsmb run, use share and the line; --json.
    rs() {
        local args=(-C 'use share')
        [ -n "${4:-}" ] && args+=(-C "$4")
        "$RSMB" -H "$1" -P "$2" "${LOGIN[@]}" --json -N "${args[@]}" >"$3" 2>"$3.err" ||
            die "rsmb ${4:-} on $1 exited $?: $(cat "$3.err") $(tail -n 2 "$3")"
    }
    # ng <host> <port> <out> [line]: one smbclient-ng run; checked by the rules of
    # bench-smbclientng.sh (the login line, no [error], the prompts) and by its file.txt lines.
    ng() {
        local args=(-C 'use share') prompts=2 files=0
        if [ -n "${4:-}" ]; then args+=(-C "$4"); prompts=3 files=85; fi
        args+=(-C '')
        smbclientng --no-colors -H "$1" -P "$2" -d TESTDOM -u tester -p testpass -N "${args[@]}" \
            >"$3" 2>&1 || true
        grep -qF "[+] Successfully authenticated to '$1' as 'TESTDOM\tester'!" "$3" || die "smbclient-ng: no login in $3"
        ! grep -qE '^\[error\]|Unknown command|Traceback|^\[x\]' "$3" || die "smbclient-ng: error in $3"
        [ "$(grep -c '^\[v\]\[' "$3")" = "$prompts" ] || die "smbclient-ng: prompts in $3"
        [ "$(grep -c 'file.txt' "$3")" = "$files" ] || die "smbclient-ng: $(grep -c file.txt "$3") file.txt lines in $3, expected $files"
    }
    summary() { jq -r 'select(.type == "walk_summary") | "\(.seconds) \(.directories) \(.entries)"' "$1"; }
    fmt='%-8s %-34s %10s %9s %8s  %s\n'
    printf "$fmt" endpoint row seconds requests flights beside

    # 1. tree /tree/d0 at slow: rsmb 3 runs, smbclient-ng 1, each (with - without); then the tap.
    coproc TAP { exec target/release/smbtap "$SLOW:445"; }
    read -r first <&"${TAP[0]}"
    port=${first##*:} port=${port%\"\}}
    tap_cmd() { echo "$1" >&"${TAP[1]}"; read -r R <&"${TAP[0]}"; }
    field() { [[ $1 =~ \"$2\":([0-9]+) ]] && echo "${BASH_REMATCH[1]}"; }
    # counts <client> <line>: requests and flights of the line through the tap (with - without).
    counts() {
        local q0 f0
        tap_cmd reset; "$1" 127.0.0.1 "$port" "$work/t0" ""
        # requests include encrypted frames (smbclient-ng's are all encrypted)
        tap_cmd "settle 300 60000"; tap_cmd stats
        q0=$(($(field "$R" requests) + $(field "$R" opaque_requests))) f0=$(field "$R" flights)
        tap_cmd reset; "$1" 127.0.0.1 "$port" "$work/t1" "$2"
        tap_cmd "settle 300 60000"; tap_cmd stats
        echo "$(($(field "$R" requests) + $(field "$R" opaque_requests) - q0)) $(($(field "$R" flights) - f0))"
    }
    : >"$work/rs"
    for i in 1 2 3; do
        t0=$(us); rs "$SLOW" 445 "$work/a" ""; t1=$(us); rs "$SLOW" 445 "$work/b" "tree /tree/d0"; t2=$(us)
        [ "$(jq -s '[.[] | select(.type == "entry")] | length' "$work/b")" = 169 ] || die "tree /tree/d0: entries"
        secs $(((t2 - t1) - (t1 - t0))) >>"$work/rs"; echo >>"$work/rs"
    done
    read -r q f < <(counts rs "tree /tree/d0")
    printf "$fmt" slow "rsmb tree /tree/d0 (85 dirs), run 1-3" "$(paste -sd/ "$work/rs")" "$q" "$f" \
        "7 waves = $(times_rt 7) s"
    t0=$(us); ng "$SLOW" 445 "$work/n0" ""; t1=$(us); ng "$SLOW" 445 "$work/n1" "tree tree/d0"; t2=$(us)
    read -r q f < <(counts ng "tree tree/d0")
    printf "$fmt" slow "smbclient-ng tree tree/d0, 1 run" "$(secs $(((t2 - t1) - (t1 - t0))))" "$q" "$f" \
        "one after another 85 x 4 rt = $(times_rt 340) s"
    tap_cmd quit
    wait "$TAP_PID" 2>/dev/null || true

    # 2-6. The product's own seconds.
    row() { # <host> <label> <line> <beside> <endpoint>
        rs "$1" 445 "$work/w" "$3"
        read -r s d e < <(summary "$work/w")
        [ -n "$s" ] || die "$3: no walk_summary"
        printf "$fmt" "$5" "$2" "$s" - - "$d listings, $e entries; $4"
    }
    for i in 1 2 3; do
        row "$SLOW" "find /tree, run $i" "find /tree" \
            "23 waves = $(times_rt 23) s, one after another 342 rt = $(times_rt 342) s" slow
    done
    for i in 1 2 3; do
        row "$SLOW" "find /fanout, run $i" "find /fanout" \
            "126 waves = $(times_rt 126) s, one after another 2,001 rt = $(times_rt 2001) s" slow
    done
    row "$SLOW" "find /chain" "find /chain" "61 rt one after another = $(times_rt 61) s" slow
    t0=$(us); rs "$SLOW" 445 "$work/a" ""; t1=$(us); rs "$SLOW" 445 "$work/b" "ls /huge"; t2=$(us)
    lshuge=$(secs $(((t2 - t1) - (t1 - t0))))
    row "$SLOW" "find /huge" "find /huge" "ls /huge on a new connection $lshuge s (1 flight)" slow
    row "$SLOW" "tree /huge" "tree /huge" "ls /huge on a new connection $lshuge s (1 flight)" slow
    row "$SLOW" "find / (the whole share)" "find /" "" slow
    row "$BAD" "find /tree" "find /tree" "150 ms each way, 40 ms jitter, 1 % loss, 20 Mbit/s" wan-bad

    drops=$(($(./testenv/ctl.sh drops) - drops_before))
    echo
    echo "rsmb $(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet 2>/dev/null || echo +changes)," \
        "Samba $(docker exec rsmb-samba smbd -V | grep -o '[0-9][0-9.]*')," \
        "$(smbclientng --help 2>/dev/null | grep -o 'v[0-9][0-9.]*' | head -n 1 | sed 's/^/smbclient-ng /')," \
        "$(date -I), $((SECONDS - start)) s; round trip at slow $rt ms; netem drops on wan: $drops"
    [ "$drops" = 0 ] || die "netem dropped packets on wan; the slow-endpoint times are suspect"
    exit 0
fi

# transfers: the rows of docs/plans/stage-2.md 6.5 (docs/performance.md). Exclusive lock: timed
# bulk transfers at the slow endpoint. Times are the product's own (`seconds` of the transfer or
# summary object: from the command's first request to its last answer); every file is checked by
# sha256 on both sides; the raw TCP ceiling is iperf3 through the same router just before.
# Removes what it wrote, on the server and locally, also when it fails.
if [ "$REPS" = 0 ]; then
    flock -x -w 1800 9 || { echo "$(basename "$0"): could not take /tmp/rsmb-env.lock within 30 minutes: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2; exit 1; }
    cargo build -q --release -p rsmb -p smbtap
    RSMB=target/release/rsmb
    LOGIN=(-u tester -p testpass -d TESTDOM)
    work=$(mktemp -d target/bench-transfers.XXXXXX)
    work=$(cd "$work" && pwd)
    remote=rsmb-bench-$$
    docker exec rsmb-samba sh -c "mkdir -p /srv/share/upload/$remote && chown tester /srv/share/upload/$remote"
    cleanup() {
        rm -rf "$work"
        docker exec rsmb-samba rm -rf "/srv/share/upload/$remote"
    }
    trap cleanup EXIT
    start=$SECONDS
    drops_before=$(./testenv/ctl.sh drops)

    die() { echo "FAIL: $*" >&2; exit 1; }
    # run <host> <line>: one rsmb run (use share, lcd, cd into the bench directory, the line), --json;
    # sets S and B to the seconds and bytes of the last transfer or summary object.
    run() {
        local out
        out=$("$RSMB" -H "$1" "${LOGIN[@]}" --json -N -C 'use share' -C "lcd $work" \
            -C "cd /upload/$remote" -C "$2") || die "$2 on $1: $out"
        read -r S B < <(jq -r 'select(.type == "summary" or .type == "transfer") | "\(.seconds) \(.bytes)"' <<<"$out" | tail -1)
    }
    local_sha() { sha256sum "$1" | cut -d' ' -f1; }
    server_sha() { docker exec rsmb-samba sha256sum "$1" | cut -d' ' -f1; }
    mbs() { awk -v b="$2" -v s="$1" 'BEGIN { printf "%.1f", b / s / 1e6 }'; }
    iperf() { # <host> [-R]: MB/s over 10 s
        iperf3 -c "$1" -t 10 -J ${2:-} | jq '.end.sum_received.bits_per_second / 8e6 * 10 | round / 10'
    }
    connect_ms() {
        local s e
        s=${EPOCHREALTIME/./}; exec 3<>"/dev/tcp/$1/445"; e=${EPOCHREALTIME/./}; exec 3>&-
        awk -v d=$((e - s)) 'BEGIN { printf "%.1f", d / 1000 }'
    }
    median() { sort -n | sed -n 2p; }

    SLOW=10.213.45.30
    BIG_SHA=ad2f1026d65a08bc81d0198783ffd36c104bf57a6c379a650794c3dd7e14de9a
    fmt='%-7s %-36s %9s %8s  %s\n'
    printf "$fmt" endpoint row seconds MB/s "beside"

    # 1. big.bin at slow, three times each way, against iperf3 just before.
    down_ip=$(iperf "$SLOW" -R) up_ip=$(iperf "$SLOW")
    : >"$work/down"; : >"$work/up"
    for i in 1 2 3; do
        run "$SLOW" "get -f /large/big.bin"; s=$S b=$B
        [ "$(local_sha "$work/big.bin")" = "$BIG_SHA" ] || die "big.bin down: checksum"
        echo "$s" >>"$work/down"
        printf "$fmt" slow "big.bin down, run $i" "$s" "$(mbs "$s" "$b")" "iperf3 -R $down_ip MB/s"
        run "$SLOW" "put -f big.bin"; s=$S b=$B
        [ "$(server_sha "/srv/share/upload/$remote/big.bin")" = "$BIG_SHA" ] || die "big.bin up: checksum"
        echo "$s" >>"$work/up"
        printf "$fmt" slow "big.bin up, run $i" "$s" "$(mbs "$s" "$b")" "iperf3 $up_ip MB/s"
    done
    for d in down up; do
        m=$(median <"$work/$d")
        while read -r t; do
            if awk -v s="$t" -v m="$m" 'BEGIN { exit !(s > 3 * m) }'; then
                die "big.bin $d: a run took $t s, more than 3 x the median $m s"
            fi
        done <"$work/$d"
    done
    rm -f "$work/big.bin"
    docker exec rsmb-samba rm -f "/srv/share/upload/$remote/big.bin"

    # 2. over4g.bin at slow, once each way.
    OVER=d0b7cf9be6a8e93f2198475dd8a6a728d536f3bcb1489f545aa2f39cf4ad4d85
    run "$SLOW" "get /large/over4g.bin"; s=$S b=$B
    [ "$(local_sha "$work/over4g.bin")" = "$OVER" ] || die "over4g.bin down: checksum"
    printf "$fmt" slow "over4g.bin down" "$s" "$(mbs "$s" "$b")" "checksum ok"
    run "$SLOW" "put over4g.bin"; s=$S b=$B
    [ "$(server_sha "/srv/share/upload/$remote/over4g.bin")" = "$OVER" ] || die "over4g.bin up: checksum"
    printf "$fmt" slow "over4g.bin up" "$s" "$(mbs "$s" "$b")" "checksum ok"
    rm -f "$work/over4g.bin"
    docker exec rsmb-samba rm -f "/srv/share/upload/$remote/over4g.bin"

    # 3. smallfiles/ at slow, both ways; requests through the tap in a second run.
    rt=$(connect_ms "$SLOW")
    waves() { awk -v rt="$rt" -v n="$1" 'BEGIN { printf "%.0f", n * rt / 1000 }'; }
    run "$SLOW" "get -r /smallfiles"; s=$S
    printf "$fmt" slow "get -r /smallfiles (2,000 files)" "$s" - \
        "wave bound 126 x 1 rt = $(waves 126) s, one after another 2,000 rt = $(waves 2000) s"
    run "$SLOW" "put -r smallfiles"; s=$S
    printf "$fmt" slow "put -r smallfiles" "$s" - \
        "wave bound 126 x 3 rt = $(waves 378) s, one after another 6,000 rt = $(waves 6000) s"
    cmp <(cd "$work/smallfiles" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) \
        <(docker exec rsmb-samba sh -c "cd /srv/share/upload/$remote/smallfiles && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum") ||
        die "smallfiles: the trees differ"
    coproc TAP { exec target/release/smbtap "$SLOW:445"; }
    read -r first <&"${TAP[0]}"
    port=${first##*:} port=${port%\"\}}
    tap_cmd() { echo "$1" >&"${TAP[1]}"; read -r R <&"${TAP[0]}"; }
    rm -rf "$work/smallfiles"
    docker exec rsmb-samba rm -rf "/srv/share/upload/$remote/smallfiles"
    for line in "get -r /smallfiles" "put -r smallfiles"; do
        tap_cmd reset
        "$RSMB" -H 127.0.0.1 -P "$port" "${LOGIN[@]}" -N -C 'use share' -C "lcd $work" \
            -C "cd /upload/$remote" -C "$line" >/dev/null || die "$line through the tap"
        tap_cmd "settle 300 60000"
        tap_cmd stats
        echo "        $line through the tap: $(jq -c '{requests, flights, peak_outstanding, connections}' <<<"$R")"
    done
    tap_cmd quit
    wait "$TAP_PID" 2>/dev/null || true
    rm -rf "$work/smallfiles"

    # 4. get -r /tree at slow (two items are denied, so the run exits 1: --keep-going, and the
    # summary).
    s=$({ "$RSMB" -H "$SLOW" "${LOGIN[@]}" --json -N --keep-going -C 'use share' -C "lcd $work" \
        -C 'get -r -f /tree' || true; } | jq -r 'select(.type == "summary") | .seconds')
    printf "$fmt" slow "get -r /tree (341 dirs, 2 denied)" "$s" - \
        "23 waves + 22 x 1 rt = $(waves 45) s"
    rm -rf "$work/tree"

    # 5. wan-bad: iperf3 both ways, then 8 MiB down and up.
    bad_down=$(iperf 10.213.45.31 -R) bad_up=$(iperf 10.213.45.31)
    run 10.213.45.31 "get /sizes/8388609.bin"; s=$S b=$B
    printf "$fmt" wan-bad "sizes/8388609.bin down" "$s" "$(mbs "$s" "$b")" "iperf3 -R $bad_down MB/s"
    run 10.213.45.31 "put 8388609.bin"; s=$S b=$B
    printf "$fmt" wan-bad "sizes/8388609.bin up" "$s" "$(mbs "$s" "$b")" "iperf3 $bad_up MB/s"
    [ "$(server_sha "/srv/share/upload/$remote/8388609.bin")" = "$(local_sha "$work/8388609.bin")" ] ||
        die "8388609.bin: checksum"

    # 6. fast endpoint: get -r /smallfiles, the sync before every rename as it is in the product.
    run 10.213.45.10 "get -r /smallfiles"; s=$S
    printf "$fmt" fast "get -r /smallfiles (sync each file)" "$s" - \
        "2,000 renames without a sync 0.07 s; with a sync one after another 12.3 s (plan 2.5)"

    # 7. medium.bin (50 MiB) each way at slow, rsmb and smbclient-ng, by one method: the process
    # time with the command minus the time of the same run without it, 3 runs each; sha256 on the
    # receiving side; smbclient-ng's run counts only with its login line, no error line and its
    # prompts (the rules of bench-smbclientng.sh).
    MEDIUM_SHA=50556313740b0f8de619aaeccebf8c30140d68c25957b2d8164d53c374cc62ca
    us() { echo "${EPOCHREALTIME/./}"; }
    secs() { awk -v d="$1" 'BEGIN { printf "%.2f", d / 1e6 }'; }
    rs1() { # [line]: rsmb in the bench directories
        local args=(-C 'use share' -C "lcd $work" -C "cd /upload/$remote")
        [ -n "${1:-}" ] && args+=(-C "$1")
        "$RSMB" -H "$SLOW" "${LOGIN[@]}" -N "${args[@]}" >"$work/rs.out" 2>&1 || die "rsmb ${1:-}: $(cat "$work/rs.out")"
    }
    ng1() { # <remote dir> [line]: smbclient-ng with $work as its working directory
        local args=(-C 'use share' -C "cd $1") prompts=3
        shift
        if [ -n "${1:-}" ]; then args+=(-C "$1"); prompts=4; fi
        args+=(-C '')
        (cd "$work" && smbclientng --no-colors -H "$SLOW" -d TESTDOM -u tester -p testpass -N "${args[@]}") \
            >"$work/ng.out" 2>&1 || true
        grep -qF "[+] Successfully authenticated to '$SLOW' as 'TESTDOM\tester'!" "$work/ng.out" ||
            die "smbclient-ng ${1:-}: no login: $(tail -n 3 "$work/ng.out")"
        ! grep -qE '^\[error\]|Unknown command|Traceback|^\[x\]|^\[!\]' "$work/ng.out" ||
            die "smbclient-ng ${1:-}: $(grep -m1 -E '^\[error\]|Unknown command|Traceback|^\[x\]|^\[!\]' "$work/ng.out")"
        [ "$(grep -c '^\[v\]\[' "$work/ng.out")" = "$prompts" ] || die "smbclient-ng ${1:-}: prompts in $work/ng.out"
    }
    # timed <client> <line> <check>: (with - without) in seconds; <check> runs after the run with
    # the line and removes what it wrote. Called in $(...): a failure ends only that subshell, so
    # the caller checks the exit status (row).
    timed() {
        local t0 t1 t2
        t0=$(us); "$1"; t1=$(us); "$1" "$2"; t2=$(us)
        "$3"
        secs $(((t2 - t1) - (t1 - t0)))
    }
    row() { # <label> <client> <line> <check>
        local t
        t=$(timed "$2" "$3" "$4") || die "$1: the run failed"
        printf "$fmt" slow "$1" "$t" - "process time"
    }
    ng_root() { ng1 / "$@"; }
    ng_up() { ng1 "upload/$remote" "$@"; }
    rs_down() { [ "$(local_sha "$work/medium.bin")" = "$MEDIUM_SHA" ] || die "rsmb medium.bin down: checksum"; rm -f "$work/medium.bin"; }
    ng_down() { [ "$(local_sha "$work/large/medium.bin")" = "$MEDIUM_SHA" ] || die "smbclient-ng medium.bin down: checksum"; rm -rf "$work/large"; }
    up_check() {
        [ "$(server_sha "/srv/share/upload/$remote/medium.bin")" = "$MEDIUM_SHA" ] || die "medium.bin up: checksum"
        docker exec rsmb-samba rm -f "/srv/share/upload/$remote/medium.bin"
    }
    docker exec rsmb-samba cat /srv/share/large/medium.bin >"$work/up.bin"
    [ "$(local_sha "$work/up.bin")" = "$MEDIUM_SHA" ] || die "medium.bin copy: checksum"
    for i in 1 2 3; do
        row "rsmb medium.bin down, run $i" rs1 'get /large/medium.bin' rs_down
        row "smbclient-ng medium.bin down, run $i" ng_root 'get large/medium.bin' ng_down
    done
    cp "$work/up.bin" "$work/medium.bin"
    for i in 1 2 3; do
        row "rsmb medium.bin up, run $i" rs1 'put medium.bin' up_check
        row "smbclient-ng medium.bin up, run $i" ng_up 'put medium.bin' up_check
    done
    rm -f "$work/medium.bin" "$work/up.bin"

    drops=$(($(./testenv/ctl.sh drops) - drops_before))
    echo
    echo "rsmb $(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet 2>/dev/null || echo +changes)," \
        "Samba $(docker exec rsmb-samba smbd -V | grep -o '[0-9][0-9.]*'), $(date -I), $((SECONDS - start)) s;" \
        "round trip at slow $rt ms; netem drops on wan: $drops"
    [ "$drops" = 0 ] || die "netem dropped packets on wan; the slow-endpoint rates are suspect"
    exit 0
fi

flock -s -w 1800 9 || { echo "$(basename "$0"): could not take /tmp/rsmb-env.lock within 30 minutes: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo build -q --release -p rsmb -p smbtap
RSMB=target/release/rsmb
LOGIN=(-u tester -p testpass -d TESTDOM)
BASELINE=testenv/baseline-smbclientng.txt

# Rows: label | baseline label | setup line | measured command ({i} = 0..3 in turn) | N | entries
# per listing (0 for cd) | requests the command must send on Samba.
ROWS=(
    "cd /tree/d{0..3}|cd /tree/d0||cd /tree/d{i}|10|0|2"
    "ls small (tree/d0, 5 entries)|ls small (tree/d0, 5 entries)|cd /tree/d0|ls|10|5|5"
    "ls many/ (10,000 entries)|ls many/ (10,000 entries)|cd /many|ls|10|10000|5"
)
[ "$REPS" -gt 1 ] && ROWS+=("ls huge/ (100,000 entries)|ls huge/ (100,000 entries)|cd /huge|ls|1|100000|5")

die() { echo "FAIL: $*" >&2; exit 1; }

# lines <setup> <cmd> <n>: the command lines of one run, one per line.
lines() {
    local i
    echo "use share"
    [ -n "$1" ] && echo "$1"
    for ((i = 0; i < $3; i++)); do echo "${2//\{i\}/$((i % 4))}"; done
}
# one <host> <port> <out> <setup> <cmd> <n>: one run with -C lines; fails the script on a non-zero exit.
one() {
    local host=$1 port=$2 out=$3 args=() l
    while IFS= read -r l; do args+=(-C "$l"); done < <(lines "$4" "$5" "$6")
    "$RSMB" -H "$host" -P "$port" "${LOGIN[@]}" -N "${args[@]}" >"$out" 2>"$out.err" ||
        die "rsmb exited $? ($5 x $6 on $host): $(cat "$out.err")"
}
# paused <host> <setup> <cmd> <n> <entries>: the lines fed over stdin in --json mode, each measured
# command sent 1 s after the previous one finished; prints the mean ms from sending it to its done
# line, which equals (t_N - t_0 - N s) / N of the two runs. Python, because bash reads 100,000
# JSON lines too slowly to time them.
paused() {
    python3 -I - "$RSMB" "$@" "${LOGIN[@]}" <<'PY'
import subprocess, sys, time
rsmb, host, setup, cmd, n, entries, *login = sys.argv[1:]
p = subprocess.Popen([rsmb, "-H", host, *login, "--json"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
def run(line):
    t0 = time.monotonic(); p.stdin.write(line.encode() + b"\n"); p.stdin.flush(); got = 0
    for l in p.stdout:
        if b'"type":"entry"' in l: got += 1
        if b'"type":"done"' in l:
            if b'"ok":true' not in l: sys.exit(f"paused: {line} failed: {l!r}")
            return time.monotonic() - t0, got
    sys.exit("paused: rsmb ended early")
for l in ["use share"] + ([setup] if setup else []): run(l)
total = 0.0
for i in range(int(n)):
    time.sleep(1)
    t, got = run(cmd.replace("{i}", str(i % 4)))
    if got != int(entries): sys.exit(f"paused: {got} entries, expected {entries}")
    total += t
p.stdin.close(); p.wait()
print(round(total * 1e6 / int(n)))
PY
}
# check <out> <n> <entries>: the run printed n x entries listing lines.
check() {
    local got
    got=$(grep -c '^[d-] *[0-9][0-9]*  ' "$1" || true)
    [ "$got" = $(($2 * $3)) ] || die "$got listing lines in $1, expected $(($2 * $3))"
}
us() { echo "${EPOCHREALTIME/./}"; }
median() { sort -n | sed -n "$(((REPS + 1) / 2))p"; }
connect_us() {
    local s e
    s=$(us); exec 3<>"/dev/tcp/$1/445" || return 1; e=$(us); exec 3>&-
    echo $((e - s))
}
median_connect_us() {
    connect_us "$1" >/dev/null
    for _ in 1 2 3 4 5; do connect_us "$1"; done | sort -n | sed -n 3p
}
per() { awk -v d="$1" -v n="$2" -v p="${3:-1}" 'BEGIN { printf "%.*f", p, d / n }'; }

tap_cmd() { echo "$1" >&"${TAP[1]}"; read -r R <&"${TAP[0]}"; }
field() { [[ $1 =~ \"$2\":([0-9]+) ]] && echo "${BASH_REMATCH[1]}"; }
# tap_session <setup> <cmd> <n> <entries>: one run through the tap; sets C Q F.
tap_session() {
    one 127.0.0.1 "$port" "$work/tap.txt" "$1" "$2" "$3"
    check "$work/tap.txt" "$3" "$4"
    tap_cmd "settle 300 60000"
    tap_cmd stats
    C=$(field "$R" connections) F=$(field "$R" flights)
    Q=$(($(field "$R" requests) + $(field "$R" opaque_requests)))
    tap_cmd reset
}
# baseline <endpoint> <label> <column>: a column of the smbclient-ng table (5 = ms, 7 = reqs, 8 = flights).
baseline() {
    awk -v ep="$1" -v label="$2" -v col="$3" '
        $1 == ep && index($0, label) { n = split(substr($0, index($0, label) + length(label)), f, " ");
                                       print f[col - 2]; exit }' "$BASELINE"
}

start=$SECONDS
drops_before=$(./testenv/ctl.sh drops)
fmt='%-6s %-30s %3s %8s %9s %8s %8s %6s %6s %7s | %10s %6s %7s\n'
printf "$fmt" endpoint command N conn_ms ms/cmd fresh paused conns reqs flights ng_ms/cmd ng_req ng_fl
for ep in fast:10.213.45.10 slow:10.213.45.30; do
    name=${ep%%:*} host=${ep#*:}
    coproc TAP { exec target/release/smbtap "$host:445"; }
    read -r first <&"${TAP[0]}"
    port=${first##*:} port=${port%\"\}}
    for row in "${ROWS[@]}"; do
        IFS='|' read -r label nglabel setup cmd n entries must <<<"$row"
        tap_session "$setup" "$cmd" 0 "$entries"; C0=$C Q0=$Q F0=$F
        tap_session "$setup" "$cmd" "$n" "$entries"
        [ $((Q - Q0)) -ge $((must * n)) ] ||
            die "row '$label' on $name is void: $((Q - Q0)) requests for $n commands, expected $((must * n))"
        rtt=$(median_connect_us "$host")
        : >"$work/diffs"; : >"$work/fresh"; : >"$work/paused"
        for ((r = 0; r < REPS; r++)); do
            t0=$(us); one "$host" 445 "$work/0.txt" "$setup" "$cmd" 0; t1=$(us)
            one "$host" 445 "$work/n.txt" "$setup" "$cmd" "$n"; t2=$(us)
            check "$work/n.txt" "$n" "$entries"
            echo $(((t2 - t1) - (t1 - t0))) >>"$work/diffs"
            if [ "$name" = slow ] && [ "$entries" != 0 ]; then
                one "$host" 445 "$work/1.txt" "$setup" "$cmd" 1; t3=$(us)
                check "$work/1.txt" 1 "$entries"
                echo $(((t3 - t2) - (t1 - t0))) >>"$work/fresh"
                paused "$host" "$setup" "$cmd" "$n" "$entries" >>"$work/paused"
            fi
        done
        ms=$(per "$(median <"$work/diffs")" $((n * 1000)))
        fresh=- pause=-
        if [ -s "$work/fresh" ]; then
            fresh=$(per "$(median <"$work/fresh")" 1000)
            pause=$(per "$(median <"$work/paused")" 1000)
        fi
        printf "$fmt" "$name" "$label" "$n" "$(per "$rtt" 1000 2)" "$ms" "$fresh" "$pause" \
            "$(per $((C - C0)) "$n")" "$(per $((Q - Q0)) "$n")" "$(per $((F - F0)) "$n")" \
            "$(baseline "$name" "$nglabel" 5)" "$(baseline "$name" "$nglabel" 7)" "$(baseline "$name" "$nglabel" 8)"
    done
    tap_cmd quit
    wait "$TAP_PID" 2>/dev/null || true
done
drops_after=$(./testenv/ctl.sh drops)

if [ "$REPS" -gt 1 ]; then
    # wan-bad: one reply of 11.4 MiB on a lossy 20 Mbit/s link, default --timeout. The end-to-end
    # check that a slow reply is not taken for silence (docs/architecture.md, "Timeouts").
    echo
    t0=$(us)
    if "$RSMB" -H 10.213.45.31 "${LOGIN[@]}" -N -C 'use share' -C 'ls /huge' >"$work/bad.txt" 2>"$work/bad.err"; then
        t1=$(us)
        echo "wan-bad  ls huge/ once, default --timeout: $(per $((t1 - t0)) 1000000 1) s (start-up included)," \
            "$(grep -c '^[d-] *[0-9][0-9]*  ' "$work/bad.txt") entries"
    else
        t1=$(us)
        echo "wan-bad  ls huge/ once, default --timeout: FAILED after $(per $((t1 - t0)) 1000000 1) s: $(cat "$work/bad.err")"
    fi
fi

echo
echo "rsmb $(git rev-parse --short HEAD 2>/dev/null)$(git diff --quiet 2>/dev/null || echo +changes)," \
    "Samba $(docker exec rsmb-samba smbd -V | grep -o '[0-9][0-9.]*'), $REPS run(s) per row, $(date -I), $((SECONDS - start)) s"
echo "smbclient-ng columns: $(grep -m1 '^smbclient-ng ' "$BASELINE")"
drops=$((drops_after - drops_before))
echo "netem drops on wan during the run: $drops"
[ "$drops" = 0 ] || die "netem dropped packets on wan; the slow-endpoint times are suspect"
