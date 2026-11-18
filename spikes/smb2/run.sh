#!/usr/bin/env bash
# Stage 0 spike for the smb2 crate. Runs the 13 steps of docs/plans/approved-plan.md ("Spikes")
# against the running test environment and prints one results table.
#   spikes/smb2/run.sh            all steps
#   spikes/smb2/run.sh 4 7        only steps 4 and 7
# Takes /tmp/rsmb-env.lock itself: shared for ordinary steps, exclusive for the disruptive ones
# (5 and 6: timed 1.1 GiB transfers and cache eviction; 10: pause, blackhole), and always undoes
# pause/blackhole inside the same lock. The raw rows are kept in target/results.tsv.
set -uo pipefail
cd "$(dirname "$0")"
LOCK=/tmp/rsmb-env.lock
CTL=../../testenv/ctl.sh
BIN=target/release/spike-smb2
OUT=target/results.tsv
steps=("$@")
[ ${#steps[@]} -eq 0 ] && steps=(1 2 3 4 5 6 7 8 9 10 11 12 13)

cargo build --release -q || exit 1
: >"$OUT"

# An interrupted step 10 must not leave samba paused or the blackhole on.
undo() { "$CTL" unpause samba 2>/dev/null; "$CTL" blackhole off 2>/dev/null; }
case " ${steps[*]} " in *" 10 "*) trap undo EXIT; trap 'exit 130' INT TERM ;; esac
drops_before=$("$CTL" drops) || drops_before=unknown

step12() {
    local v t0 dir errs lines rc root=$PWD
    printf 'ROW\t12\tcrates in dependency tree of smb2 0.43.0 (normal deps)\t-\t%s\t\n' \
        "$(cargo tree -q -p smb2 -e normal --prefix none | sed 's/ (\*)$//' | sort -u | wc -l)"
    printf 'ROW\t12\tcrates in the spike binary (smb2 + tokio full + tokio-socks + sha2 + smbtap)\t-\t%s\t\n' \
        "$(cargo tree -q -e normal --prefix none | sed 's/ (\*)$//' | sort -u | wc -l)"
    for what in "-p smb2" ""; do
        dir=$(mktemp -d)
        t0=$(date +%s)
        CARGO_TARGET_DIR=$dir cargo build -q --release $what 2>/dev/null
        printf 'ROW\t12\tclean release build %s\t-\t%s s\t%s cores, crates already downloaded\n' \
            "${what:-of the spike}" "$(($(date +%s) - t0))" "$(nproc)"
        rm -rf "$dir"
    done
    # The spike compiled against earlier releases: lines that break.
    for v in 0.40.0 0.41.0 0.42.1 0.43.0; do
        dir=$(mktemp -d)
        mkdir "$dir/src" && cp src/main.rs "$dir/src/"
        sed -e "s/^smb2 = .*/smb2 = \"=$v\"/" -e "s#path = \"../../smbtap\"#path = \"$root/../../smbtap\"#" \
            Cargo.toml >"$dir/Cargo.toml"
        out=$(cd "$dir" && CARGO_TARGET_DIR=$root/target/compat cargo check -q --message-format short 2>&1); rc=$?
        errs=$(grep -c ': error' <<<"$out")
        lines=$(grep ': error' <<<"$out" | cut -d: -f1,2 | sort -u | wc -l)
        # cargo failing with no counted compiler error (resolution, download) is a failure, not "0 errors"
        printf 'ROW\t12\tspike compiled against smb2 %s\t-\t%s errors on %s lines%s\t%s\n' "$v" "$errs" "$lines" \
            "$([ $rc -ne 0 ] && [ "$errs" = 0 ] && echo " FAIL: cargo check exited $rc without a compiler error")" \
            "$(grep ': error' <<<"$out" | sed 's/^[^ ]* error\(\[[A-Z0-9]*\]\)\?: //' | sort | uniq -c | sort -rn | head -4 | tr -s ' ' | paste -sd ';')"
        rm -rf "$dir"
    done
    local src
    src=$(cargo metadata -q --format-version 1 | python3 -c 'import json,sys; m=json.load(sys.stdin); print([p for p in m["packages"] if p["name"]=="smb2"][0]["manifest_path"].rsplit("/",1)[0])')
    printf 'ROW\t12\tNT-hash login in smb2 0.43.0\t-\t%s\t%s\n' \
        "$(grep -rqiE 'pub fn .*(nt_hash|from_hash|with_hash)' "$src/src/auth" && echo present || echo 'not found')" \
        "$(grep -rliE 'nt_hash|nthash' "$src/src/auth" | sed "s#$src/##" | paste -sd ' ')"
    printf 'ROW\t12\tKerberos login in smb2 0.43.0\t-\t%s\t%s\n' \
        "$(grep -q 'pub async fn setup_kerberos' "$src/src/client/session.rs" && echo present || echo 'not found')" \
        "Session::setup_kerberos / setup_kerberos_from_ccache, KerberosAuthenticator (not tested)"
}

for s in "${steps[@]}"; do
    case $s in
        12) step12 ;;
        5 | 6) flock -x "$LOCK" "$BIN" "$s" ;;
        10) flock -x "$LOCK" bash -c "'$BIN' 10; '$CTL' unpause samba 2>/dev/null; '$CTL' blackhole off" ;;
        *) flock -s "$LOCK" "$BIN" "$s" ;;
    esac | tee -a "$OUT" >&2
done

# Remove anything the spike may have left behind after an aborted run (only its own paths).
flock -s "$LOCK" bash -c '
    docker exec rsmb-samba rm -rf /srv/share/upload/spike-smb2 /srv/tiny/spike-smb2-full.bin
    docker exec rsmb-samba-edge rm -rf /srv/share/upload/spike-smb2' 2>/dev/null

drops_after=$("$CTL" drops) || drops_after=unknown
echo
echo "$(date -u +%Y-%m-%dT%H:%MZ), Samba $(docker exec rsmb-samba smbd -V 2>/dev/null)"
echo "netem drops on wan: $drops_before before, $drops_after after the run"
{
    printf 'step\tcheck\tendpoint\tresult\tdetail\n'
    grep '^ROW' "$OUT" | cut -f2-
} | column -t -s $'\t' -o ' | '
[ "$drops_before" = "$drops_after" ] && [ "$drops_after" != unknown ] \
    || { echo "FAIL: netem dropped packets on wan (or could not be asked); slow-endpoint times are suspect" >&2; exit 1; }
