#!/usr/bin/env bash
# Small controls for tests and spikes. See docs/test-environment.md.
#   ctl.sh blackhole on|off        drop every packet of 10.213.45.30:4450 (new and open connections)
#   ctl.sh evict <service> <path>  drop a file from the server's page cache (path inside the container)
#   ctl.sh conns <service>         TCP connections to port 445 from outside since the last reset
#   ctl.sh conns-reset <service>   set that count to zero
#   ctl.sh drops                   packets netem on wan has dropped so far (fails if wan cannot be asked)
#   ctl.sh pause|unpause <service> freeze the server: TCP is accepted, nothing answers
# <service> is samba, samba-edge or samba-private (pause also takes wan, wan-bad, socks, samba-dc;
# the DC has no connection counter, so conns and evict do not apply to it).
set -euo pipefail

rule=(FORWARD -p tcp -m conntrack --ctorigdstport 4450 -j DROP)

case "${1:-} ${2:-}" in
    "blackhole on")
        docker exec rsmb-wan iptables -C "${rule[@]}" 2>/dev/null \
            || docker exec rsmb-wan iptables -I "${rule[@]}" ;;
    "blackhole off")
        while docker exec rsmb-wan iptables -D "${rule[@]}" 2>/dev/null; do :; done ;;
    "evict "?*)
        docker exec "rsmb-$2" dd if="$3" iflag=nocache count=0 status=none ;;
    "conns "?*)
        docker exec "rsmb-$2" iptables -t nat -L PREROUTING -v -x -n \
            | awk '$3 == "ACCEPT" && /dpt:445/ { print $1 }' ;;
    "conns-reset "?*)
        docker exec "rsmb-$2" iptables -t nat -Z PREROUTING ;;
    "drops "*)
        out=$(docker exec rsmb-wan tc -s qdisc show)
        awk '/^qdisc netem/ { q = 1 } q && /dropped/ { gsub(",", ""); s += $7; q = 0 } END { print s + 0 }' <<<"$out" ;;
    "pause "?* | "unpause "?*)
        docker "$1" "rsmb-$2" >/dev/null ;;
    *)
        sed -n '2,10p' "$0" >&2
        exit 2 ;;
esac
