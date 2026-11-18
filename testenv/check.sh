#!/usr/bin/env bash
# Checks that the running test environment is what the tests assume, using only tools that do not
# depend on this project's code: the host's smbclient, bash /dev/tcp, python3 (SOCKS5), iperf3,
# docker exec (kinit, kvno and smbclient inside the DC). Prints one line per check; exits non-zero
# if any check fails. See docs/test-environment.md for what each check protects.
set -uo pipefail
cd "$(dirname "$0")"
# The host's smbclient tries Kerberos by default and would read the host's /etc/krb5.conf, which
# may describe another realm: nothing here may depend on it.
export KRB5_CONFIG=/dev/null
# Exclusive for the whole run: it resets connection counters and turns the blackhole on and off.
# Waits while a spike or benchmark holds the shared lock (docs/test-environment.md, "The lock").
exec 9>>/tmp/rsmb-env.lock
flock -x -w 1800 9 || { echo "$(basename "$0"): could not take /tmp/rsmb-env.lock within 30 minutes: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2; exit 1; }
. samba/names.sh

FAST=10.213.45.10 SLOW=10.213.45.30 BAD=10.213.45.31 EDGE=10.213.45.11 PROXY=10.213.45.20
PRIVATE_IP=10.213.47.10 PRIVATE_NAME=fileserver.rsmb.test DC=10.213.45.12
CRED='TESTDOM\tester%testpass'
fails=0

ok() { printf 'ok    %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }
# check <description> <command...>: ok if the command succeeds
check() { local what=$1; shift; if "$@"; then ok "$what"; else fail "$what"; fi; }
# smb <host> <share> <smbclient args...>: output (stdout and stderr) of one smbclient run
smb() { local host=$1 share=$2; shift 2; smbclient -s /dev/null "//$host/$share" "$@" 2>&1; }
# says <text> <host> <share> <smbclient args...>: the smbclient output contains <text>
says() { local want=$1 out; shift; out=$(smb "$@"); grep -qF -- "$want" <<<"$out"; }

# Helpers for what bash does badly: parsing smbclient listings and speaking SOCKS5.
read -r -d '' PY <<'EOF'
import hashlib, re, socket, struct, sys
LINE = re.compile(rb"^  (.*?)\s+([DAHSRN]+)\s+(\d+)\s+\w{3} \w{3} [ \d]\d \d\d:\d\d:\d\d \d{4}$")
def names(listing):
    out = []
    for line in listing.split(b"\n"):
        m = LINE.match(line)
        if m and m.group(1) not in (b".", b".."):
            out.append(m.group(1))
    return out
cmd = sys.argv[1]
if cmd == "count":                      # stdin: smbclient ls output
    print(len(names(sys.stdin.buffer.read())))
elif cmd == "names":                    # stdin: listing; argv[2]: file with expected names
    got = set(names(sys.stdin.buffer.read()))
    want = set(n for n in open(sys.argv[2], "rb").read().split(b"\n") if n)
    for n in sorted(want - got): print("  missing:", n)
    for n in sorted(got - want): print("  unexpected:", n)
    sys.exit(got != want)
elif cmd == "mangled":                  # stdin: listing; every name must be an 8.3 mangled name
    got = names(sys.stdin.buffer.read())
    print(len(got))
    sys.exit(not all(re.fullmatch(rb"[A-Z0-9_]{6}~[A-Z0-9](\.[A-Z0-9]{1,3})?", n) for n in got))
elif cmd == "hash":                     # stdin: data; prints sha256 and length
    h, n = hashlib.sha256(), 0
    while chunk := sys.stdin.buffer.read(1 << 20):
        h.update(chunk); n += len(chunk)
    print(h.hexdigest(), n)
elif cmd == "socks":                    # argv: proxy port host [user pass]; prints the outcome
    proxy, port, host, *auth = sys.argv[2:]
    s = socket.create_connection((proxy, int(port)), timeout=5)
    s.sendall(b"\x05\x01" + (b"\x02" if auth else b"\x00"))
    if s.recv(2) != (b"\x05\x02" if auth else b"\x05\x00"):
        print("method-refused"); sys.exit()
    if auth:
        u, p = (a.encode() for a in auth)
        s.sendall(b"\x01" + bytes([len(u)]) + u + bytes([len(p)]) + p)
        if s.recv(2) != b"\x01\x00":
            print("auth-refused"); sys.exit()
    s.sendall(b"\x05\x01\x00\x03" + bytes([len(host)]) + host.encode() + struct.pack(">H", 445))
    if s.recv(10)[:2] != b"\x05\x00":
        print("connect-refused"); sys.exit()
    # Minimal SMB2 NEGOTIATE offering dialect 2.0.2; the server must answer with an SMB2 header.
    hdr = b"\xfeSMB" + struct.pack("<HHIHHIIQIIQ16s", 64, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, b"")
    body = struct.pack("<HHHHI16sQH", 36, 1, 1, 0, 0, bytes(16), 0, 0x0202)
    s.sendall(struct.pack(">I", len(hdr + body)) + hdr + body)
    print("smb2" if s.recv(8)[4:8] == b"\xfeSMB" else "no-smb2")
EOF
py() { python3 -c "$PY" "$@"; }

# --- services ---
for c in samba samba-edge samba-private wan wan-bad socks samba-dc; do
    st=$(docker inspect -f '{{.State.Health.Status}}' "rsmb-$c" 2>/dev/null || echo missing)
    [ "$st" = healthy ] || { fail "rsmb-$c is $st (logs: docker logs rsmb-$c; restart: testenv/down.sh && testenv/up.sh)"; exit 1; }
done
ok "all seven containers healthy"
ok "Samba version: $(docker exec rsmb-samba smbd -V)"

# --- latency: TCP connect time, median of 5 after one warm-up connect ---
connect_us() {
    local s e
    s=${EPOCHREALTIME/./}
    exec 3<>"/dev/tcp/$1/$2" || return 1
    e=${EPOCHREALTIME/./}
    exec 3>&-
    echo $((e - s))
}
median_connect_us() {
    connect_us "$1" "$2" >/dev/null || return 1
    for _ in 1 2 3 4 5; do connect_us "$1" "$2" || return 1; done | sort -n | sed -n 3p
}
us=$(median_connect_us $FAST 445); check "fast endpoint $FAST connect ${us}us (< 5000)" test "${us:-999999}" -lt 5000
us=$(median_connect_us $SLOW 445)
check "slow endpoint $SLOW connect ${us}us (195000..230000)" test "${us:-0}" -ge 195000 -a "${us:-0}" -le 230000
us=$(median_connect_us $BAD 445); check "bad endpoint $BAD connect ${us}us (> 150000)" test "${us:-0}" -gt 150000

# --- logins ---
check "login TESTDOM\\tester" says "blocks available" $FAST share -U "$CRED" -c ls
check "wrong password refused" says NT_STATUS_LOGON_FAILURE $FAST share -U 'TESTDOM\tester%wrong' -c ls
check "guest login (unknown user) reads the guest share" says "getting file" $FAST guest -U 'rsmb-nobody%' -c 'get hello.txt /dev/null'
check "guest refused on share" says NT_STATUS_ACCESS_DENIED $FAST share -U 'rsmb-nobody%' -c ls
check "anonymous login reads the guest share" says "getting file" $FAST guest -U '%' -c 'get hello.txt /dev/null'
check "encrypted share readable with SMB3" says "getting file" $FAST encrypted -U "$CRED" -c 'get sizes/1.bin /dev/null'
check "encrypted share refused without encryption (SMB 2.1)" says NT_STATUS_ACCESS_DENIED $FAST encrypted -m SMB2_10 -U "$CRED" -c ls
check "slow endpoint serves the share" says "65537.bin" $SLOW share -U "$CRED" -c 'ls sizes/*'

# --- fixtures ---
n=$(smb $FAST share -U "$CRED" -c 'ls many/*' | py count); check "many/ has $n entries (10000)" test "$n" = 10000
n=$(smb $FAST share -U "$CRED" -c 'ls huge/*' | py count); check "huge/ has $n entries (100000)" test "$n" = 100000
n=$(smb $FAST share -U "$CRED" -c 'ls fanout/*' | py count); check "fanout/ has $n entries (2000)" test "$n" = 2000
n=$(smb $FAST share -U "$CRED" -c 'ls longnames/*' | py count); check "longnames/ has $n entries (2000)" test "$n" = 2000
for d in forbidden tree/forbidden traverse-only; do
    check "$d/ cannot be listed" says NT_STATUS_ACCESS_DENIED $FAST share -U "$CRED" -c "ls $d/*"
done
for f in forbidden/secret.txt tree/forbidden/secret.txt tree/unreadable.txt; do
    check "$f cannot be read" says NT_STATUS_ACCESS_DENIED $FAST share -U "$CRED" -c "get $f /dev/null"
done
check "traverse-only/known.txt readable by name" says "getting file" $FAST share -U "$CRED" -c 'get traverse-only/known.txt /dev/null'
check "write to readonly refused" says NT_STATUS_ACCESS_DENIED $FAST readonly -U "$CRED" -c 'put /etc/hostname upload/check.txt'
for share in share tiny; do
    dir=$([ $share = share ] && echo upload/ || echo "")
    out=$(smb $FAST $share -U "$CRED" -c "put /etc/hostname ${dir}check.txt; del ${dir}check.txt")
    check "write and delete on $share" test -z "$(echo "$out" | grep NT_STATUS)"
done

for f in large/big.bin large/over4g.bin large/medium.bin sizes/{0,1,65535,65536,65537,1048575,1048576,1048577,8388607,8388608,8388609}.bin; do
    want=$(docker exec rsmb-samba sha256sum "/srv/share/$f" | cut -d' ' -f1)
    size=$(docker exec rsmb-samba stat -c %s "/srv/share/$f")
    got=$(smbclient -s /dev/null //$FAST/share -U "$CRED" -c "get $f /dev/stdout" 2>/dev/null | py hash)
    check "$f: $size bytes, sha256 ${want:0:16}... via SMB" test "$got" = "$want $size"
done

want=$(mktemp)
{ printf '%s\n' "$CJK_NAMES" nested
  set -f; IFS=$'\n'; set -- $CJK_NAMES; unset IFS; set +f
  for i in $(seq 0 999); do b=$(( i % 9 + 1 )); printf '%s_%04d.txt\n' "${!b}" "$i"; done; } >"$want"
check "cjk/ lists 1010 names with the exact bytes" py names "$want" < <(smb $FAST share -U "$CRED" -c 'ls cjk/*')
{ printf '%s\n' "$SPECIAL_NAMES" "dir with space"; } >"$want"
check "special/ lists 22 names with the exact bytes" py names "$want" < <(smb $FAST share -U "$CRED" -c 'ls special/*')
rm -f "$want"
p=cjk/nested; set -f; IFS=$'\n'; for b in $CJK_NAMES; do p=$p/$b; done; unset IFS; set +f
check "cjk/nested/... 9 levels deep readable" says "getting file" $FAST share -U "$CRED" -c "get \"$p/leaf.txt\" /dev/null"
check "special/dir with space/... readable" says "getting file" $FAST share -U "$CRED" -c "get \"special/dir with space/$UMLAUT/$CYRILLIC/$EMOJI/leaf.txt\" /dev/null"
n=$(py mangled < <(smb $FAST share -U "$CRED" -c 'ls mangled/*')); r=$?
check "mangled/ lists $n entries (9), all as 8.3 mangled names" test "$r" = 0 -a "$n" = 9
out=$(smbclient -s /dev/null //$FAST/share -U "$CRED" -c 'get links/a/to-c/file.txt /dev/stdout' 2>/dev/null)
check "links/a/to-c/file.txt reads 'linked' via SMB and links/a/to-c is a symbolic link on disk" \
    test "$out" = linked -a "$(docker exec rsmb-samba readlink /srv/share/links/a/to-c)" = ../b/c
n=$(docker exec rsmb-samba ls /srv/share/mangled | wc -l)
check "mangled/ has $n entries on disk (12): the 3 legacy-encoded names are hidden by Samba" test "$n" = 12

# --- SOCKS5 and the private server ---
getent hosts $PRIVATE_NAME >/dev/null; check "$PRIVATE_NAME does not resolve on the host" test $? != 0
timeout 3 bash -c "exec 3<>/dev/tcp/$PRIVATE_IP/445" 2>/dev/null; check "direct connection to $PRIVATE_IP:445 fails" test $? != 0
before=$(docker logs rsmb-socks 2>&1 | grep -c "connected to $PRIVATE_NAME:445")
./ctl.sh conns-reset samba-private
check "SOCKS5 no-auth (1080) reaches SMB on $PRIVATE_NAME" test "$(py socks $PROXY 1080 $PRIVATE_NAME)" = smb2
check "SOCKS5 user/pass (1081) reaches SMB on $PRIVATE_NAME" test "$(py socks $PROXY 1081 $PRIVATE_NAME tester testpass)" = smb2
check "SOCKS5 wrong password refused" test "$(py socks $PROXY 1081 $PRIVATE_NAME tester wrong)" = auth-refused
check "SOCKS5 1081 without credentials refused" test "$(py socks $PROXY 1081 $PRIVATE_NAME)" = method-refused
after=$(docker logs rsmb-socks 2>&1 | grep -c "connected to $PRIVATE_NAME:445")
check "proxy log shows 'connected to $PRIVATE_NAME:445' for both" test $((after - before)) = 2
n=$(./ctl.sh conns samba-private); check "samba-private counted $n connections (2)" test "$n" = 2

# --- Kerberos and the DC ---
# Inside the DC: its own kinit, kvno and smbclient, with a cache of their own.
kdc_lines() { local l; l=$(docker logs rsmb-samba-dc 2>&1); grep -c 'TGS-REQ tester@RSMB.TEST from ipv4:' <<<"$l"; }
out=$(docker exec rsmb-samba-dc sh -c 'c=$(mktemp -u); trap "rm -f $c" EXIT; echo testpass | kinit -c "FILE:$c" tester@RSMB.TEST >/dev/null && KRB5CCNAME="FILE:$c" smbclient -s /dev/null //dc.rsmb.test/share --use-kerberos=required -c "get hello.txt /dev/stdout" 2>/dev/null')
check "a Kerberos login in the DC (kinit, smbclient --use-kerberos=required) reads the share" test "$out" = "kerberos works"
before=$(kdc_lines)
docker exec rsmb-samba-dc sh -c 'c=$(mktemp -u); trap "rm -f $c" EXIT; echo testpass | kinit -c "FILE:$c" tester@RSMB.TEST >/dev/null && kvno -q -c "FILE:$c" cifs/10.213.45.12 cifs/127.0.0.1 cifs/10.213.45.10 cifs/dc.rsmb.test'
check "the four SPNs exist (kvno cifs/10.213.45.12 cifs/127.0.0.1 cifs/10.213.45.10 cifs/dc.rsmb.test)" test $? = 0
after=$(kdc_lines)
check "the KDC's log names the source of each request ($((after - before)) new 'TGS-REQ tester@RSMB.TEST from ipv4:' lines, at least 4)" test $((after - before)) -ge 4
docker exec rsmb-samba-dc sh -c 'c=$(mktemp -u); trap "rm -f $c" EXIT; echo testpass | kinit -c "FILE:$c" tester@RSMB.TEST >/dev/null && kvno -q -c "FILE:$c" cifs/nosuch.rsmb.test' 2>/dev/null
check "an unknown SPN (cifs/nosuch.rsmb.test) is refused" test $? != 0
cc=$(docker exec rsmb-samba-dc mkcache -f tester cifs/dc.rsmb.test | od -A n -t x1 -v | tr -d ' \n')
check "mkcache writes a ticket cache ($(( ${#cc} / 2 )) bytes, at least 1000, starting 05 04)" test "${cc:0:4}" = 0504 -a "${#cc}" -ge 2000
for n in dc.rsmb.test rsmb.test; do
    getent hosts $n >/dev/null; check "$n does not resolve on the host" test $? != 0
    check "the proxy (1080) resolves $n and reaches SMB on the DC" test "$(py socks $PROXY 1080 $n)" = smb2
done
timeout 3 bash -c "exec 3<>/dev/tcp/$DC/88" 2>/dev/null; check "the KDC $DC:88 is reachable from the host without the proxy" test $? = 0
check "a password login at the DC with a wrong password is refused" says NT_STATUS_LOGON_FAILURE $DC share --use-kerberos=off -U 'RSMB\tester%wrong' -c ls

# --- samba-edge ---
n=$(smbclient -s /dev/null -L //$EDGE -U "$CRED" 2>&1 | grep -cE '^\s+edge[0-9]{3}\$?\s+Disk')
check "samba-edge lists $n edge shares (300, 30 of them hidden)" test "$n" = 300

# --- controls (ctl.sh) ---
./ctl.sh conns-reset samba
for _ in 1 2 3; do connect_us $FAST 445 >/dev/null; done
connect_us $SLOW 445 >/dev/null
sleep 0.2
n=$(./ctl.sh conns samba); check "connection count on samba: $n (4: three direct, one through wan)" test "$n" = 4
./ctl.sh blackhole off
timeout 2 bash -c "exec 3<>/dev/tcp/$SLOW/4450" 2>/dev/null; check "blackhole port 4450 open when off" test $? = 0
./ctl.sh blackhole on
timeout 2 bash -c "exec 3<>/dev/tcp/$SLOW/4450" 2>/dev/null; check "blackhole port 4450 silent when on" test $? = 124
connect_us $SLOW 445 >/dev/null; check "port 445 on wan unaffected by the blackhole" test $? = 0
./ctl.sh blackhole off
read_ms() { docker exec rsmb-samba sh -c 's=$(date +%s%N); cat /srv/share/large/big.bin >/dev/null; echo $(( ($(date +%s%N) - s) / 1000000 ))'; }
read_ms >/dev/null; cached=$(read_ms); ./ctl.sh evict samba /srv/share/large/big.bin; cold=$(read_ms)
check "evict: big.bin read ${cached} ms cached, ${cold} ms after eviction (at least 3x slower)" test "$cold" -ge $((3 * cached))

# --- raw TCP ceiling and drops ---
rate=$(iperf3 -c $SLOW -t 2 -f m 2>&1 | awk '/receiver/ { print $7 " " $8 }')
check "iperf3 through wan: ${rate:-failed} over 2 s (still in slow start; see docs)" test -n "$rate"
drops=$(./ctl.sh drops) || drops=unknown
check "no packets dropped by netem on wan ($drops)" test "$drops" = 0
ok "wan-bad netem: $(docker exec rsmb-wan-bad tc -s qdisc show | grep -o 'dropped [0-9]*' | paste -sd' ') (loss there is intended)"

echo
if [ $fails -gt 0 ]; then echo "check.sh: $fails check(s) FAILED"; exit 1; fi
echo "check.sh: all checks passed"
