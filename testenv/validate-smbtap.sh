#!/usr/bin/env bash
# Cross-checks smbtap against tshark: one smbclient navigation session runs through the tap while
# dumpcap captures the tap's upstream leg on the rsmb-pub bridge; then the per-command request
# counts, responses, interim responses, connections and flights from both are printed side by side.
# Needs: the environment up, dumpcap usable without root (wireshark group), tshark, smbclient.
# Usage: testenv/validate-smbtap.sh [upstream-ip]   (default 10.213.45.10, the fast endpoint)
set -euo pipefail
cd "$(dirname "$0")/.."
upstream=${1:-10.213.45.10}
work=$(mktemp -d)
cap=
trap 'rm -rf "$work"; [ -z "$cap" ] || kill "$cap" 2>/dev/null' EXIT

cargo build -q -p smbtap
coproc TAP { exec target/debug/smbtap "$upstream:445"; }
read -r first <&"${TAP[0]}"
port=${first##*:}; port=${port%\"\}}

dumpcap -q -B 1024 -i rsmb-pub -f "host $upstream and tcp port 445" -w "$work/cap.pcapng" 2>/dev/null &
cap=$!
sleep 1.5   # dumpcap needs a moment before it captures

smbclient -s /dev/null //127.0.0.1/share -p "$port" -U 'TESTDOM\tester%testpass' -c '
    ls; cd many; ls; cd ..; cd cjk; ls; cd ../tree/d0/d1; ls; cd /; cd special;
    ls; cd /; get sizes/65537.bin /dev/null; get large/medium.bin /dev/null;
    allinfo sizes/1.bin; ls forbidden/*; cd nonexistent; ls huge/f00001*' >"$work/smbclient.out" 2>&1 \
    || { echo "validate-smbtap: the smbclient session failed:" >&2; tail -5 "$work/smbclient.out" >&2; exit 1; }

echo "settle 300 10000" >&"${TAP[1]}"; read -r _ <&"${TAP[0]}"
echo "stats" >&"${TAP[1]}"; read -r stats <&"${TAP[0]}"
echo quit >&"${TAP[1]}"; wait "$TAP_PID" 2>/dev/null || true
sleep 1; kill "$cap"; wait "$cap" 2>/dev/null || true; cap=

# tshark: one line per packet carrying SMB2, fields: source address, response flags, commands,
# statuses (comma-separated for compound chains).
tshark -r "$work/cap.pcapng" -Y smb2 -T fields -E occurrence=a -E separator=';' \
    -e ip.src -e smb2.flags.response -e smb2.cmd -e smb2.nt_status -e smb2.flags.async >"$work/smb2.txt"
syns=$(tshark -r "$work/cap.pcapng" -Y 'tcp.flags.syn == 1 && tcp.flags.ack == 0' | wc -l)

python3 - "$work/smb2.txt" "$stats" "$syns" <<'EOF'
import json, sys
NAMES = ["NEGOTIATE","SESSION_SETUP","LOGOFF","TREE_CONNECT","TREE_DISCONNECT","CREATE","CLOSE",
         "FLUSH","READ","WRITE","LOCK","IOCTL","CANCEL","ECHO","QUERY_DIRECTORY","CHANGE_NOTIFY",
         "QUERY_INFO","SET_INFO","OPLOCK_BREAK"]
tap = json.loads(sys.argv[2])
req, resp, interim, flights, last_from_server, sent = {}, 0, 0, 0, False, False
for line in open(sys.argv[1]):
    src, flags, cmds, statuses, asyncs = line.rstrip("\n").split(";")
    cmds = cmds.split(",")
    if src == "10.213.45.1":                                 # from the client side (the tap)
        if not sent or last_from_server: flights += 1
        sent, last_from_server = True, False
        for c in cmds: req[NAMES[int(c)]] = req.get(NAMES[int(c)], 0) + 1
    else:
        last_from_server = True
        st, asy = statuses.split(","), asyncs.split(",")
        for i, c in enumerate(cmds):
            resp += 1
            if st[i] in ("0x00000103", "259") and asy[i] in ("1", "True"): interim += 1
rows = [(k, tap["per_command"].get(k, 0), req.get(k, 0)) for k in sorted(set(req) | set(tap["per_command"]))]
rows += [("requests (total)", tap["requests"], sum(req.values())), ("responses", tap["responses"], resp),
         ("interim responses", tap["interim"], interim), ("flights", tap["flights"], flights),
         ("TCP connections", tap["connections"], int(sys.argv[3]))]
print(f"{'':20} {'smbtap':>8} {'tshark':>8}")
for k, a, b in rows: print(f"{k:20} {a:8} {b:8}{'' if a == b else '   <-- differs'}")
if sum(req.values()) < 20:                                   # a real session sends about 86
    print(f"validate-smbtap: only {sum(req.values())} requests captured; no SMB traffic went through", file=sys.stderr)
    sys.exit(1)
sys.exit(any(a != b for _, a, b in rows))
EOF
