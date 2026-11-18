#!/usr/bin/env bash
# The one test command (docs/testing.md).
#   ./test.sh fast   environment, check.sh, fmt, clippy, build, unit tests, tests/fast (direct, then
#                    again through the SOCKS5 proxy to the private server)
#   ./test.sh        the same, then tests/slow, tests/serial and smbtap against tshark
# Steps run in order and the first failure stops the run. Each step prints how long the previous
# one took; the last line prints the counted time (total minus environment up and build) against
# the budget. Over budget does not change the exit code; it is a finding to fix.
set -euo pipefail
cd "$(dirname "$0")"

mode=${1:-full}
case $mode in full) budget=600 ;; fast) budget=150 ;; *) echo "usage: ./test.sh [fast]" >&2; exit 2 ;; esac
lock=/tmp/rsmb-env.lock
lock_wait=1800 # the longest holder is bench-rsmb.sh transfers, about 21 minutes

# locked -s|-x <command...>: the command under the lock, waiting at most lock_wait seconds for it.
locked() {
    local kind=$1 rc=0
    shift
    flock "$kind" -E 99 -w "$lock_wait" "$lock" "$@" || rc=$?
    if [ "$rc" -eq 99 ]; then
        echo "FAIL: could not take $lock ($kind) within $lock_wait s: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2
    fi
    return "$rc"
}

start=$SECONDS
step_start=$SECONDS
step_name=""
uncounted=0
times=()

finish_step() {
    if [ -n "$step_name" ]; then
        local took=$((SECONDS - step_start))
        times+=("$(printf '%5d s  %s' "$took" "$step_name")")
        printf -- '--- %s: %d s\n' "$step_name" "$took"
        case $step_name in "test environment up" | build) uncounted=$((uncounted + took)) ;; esac
    fi
}
step() {
    finish_step
    step_name=$*
    step_start=$SECONDS
    printf '\n=== %s\n' "$*"
}

step "test environment up"
./testenv/up.sh

step "stale test directories"
# Exclusive: a test that holds the shared lock may be writing to one of these directories.
locked -x bash -c 'for c in rsmb-samba rsmb-samba-edge rsmb-samba-private; do
    docker exec "$c" sh -c "rm -rf /srv/share/upload/rsmb-test-*"
done'


step "test environment check"
./testenv/check.sh

step "cargo fmt --check"
cargo fmt --all --check

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "build"
cargo test --workspace --lib --bins --no-run
cargo test --release --workspace --no-run

step "unit tests"
cargo test --workspace --lib --bins

step "tests/fast"
locked -s cargo test --release --test fast

# The same binary through the proxy (RSMB_TEST_VIA=socks5). Every connection a proxied run opens
# is one line in the proxy's log; a direct run opens about 150 and adds none of them, so fewer
# than 100 new lines means the variable did not reach the tests (docs/testing.md).
step "tests/fast through the proxy"
proxy_log() {
    local l
    l=$(docker logs rsmb-socks 2>&1)
    grep -c 'connected to fileserver.rsmb.test:445' <<<"$l" || true
}
before=$(proxy_log)
RSMB_TEST_VIA=socks5 locked -s cargo test --release --test fast
added=$(($(proxy_log) - before))
echo "the proxy logged $added connections to fileserver.rsmb.test:445"
[ "$added" -ge 100 ] || {
    echo "FAIL: tests/fast through the proxy added $added lines to the proxy's log, expected at least 100: did the tests see RSMB_TEST_VIA?" >&2
    exit 1
}

if [ "$mode" = full ]; then
    step "tests/slow"
    locked -s cargo test --release --test slow

    step "tests/serial"
    locked -x cargo test --release --test serial -- --test-threads=1

    step "smbtap against tshark"
    ./testenv/validate-smbtap.sh
fi
finish_step

total=$((SECONDS - start))
counted=$((total - uncounted))
printf '\n=== step durations\n'
printf '%s\n' "${times[@]}"
verdict="within budget"
[ "$counted" -le "$budget" ] || verdict="OVER BUDGET"
printf '\n=== all passed (%s): total %d s, counted %d s of %d s budget: %s\n' \
    "$mode" "$total" "$counted" "$budget" "$verdict"
