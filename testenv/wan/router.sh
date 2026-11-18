#!/bin/sh
# Forwards PUB_ADDR:445 and :5201 to TARGET and delays every packet with netem on both interfaces.
# PUB_ADDR:4450 also forwards to TARGET:445 and is the port testenv/ctl.sh can blackhole.
# Interfaces are found by address, never by name. "router.sh health" is the healthcheck.
# Environment: PUB_ADDR, BACK_ADDR (this container), TARGET (samba), NETEM (netem parameters).
set -eu

ifname() { ip -o -4 addr show | awk -v a="$1" '{ split($4, p, "/"); if (p[1] == a) print $2 }'; }
pub_if=$(ifname "$PUB_ADDR")
back_if=$(ifname "$BACK_ADDR")
[ -n "$pub_if" ] && [ -n "$back_if" ] || { echo "no interface for $PUB_ADDR or $BACK_ADDR" >&2; exit 1; }

if [ "${1:-}" = health ]; then
    for i in "$pub_if" "$back_if"; do
        tc qdisc show dev "$i" | grep -q "^qdisc netem .* root .*limit 100000 " || exit 1
    done
    rules=$(iptables -t nat -S)
    for r in "--to-destination $TARGET:445" "--to-destination $TARGET:5201" "--to-source $BACK_ADDR"; do
        echo "$rules" | grep -q -- "$r" || exit 1
    done
    exit 0
fi

for port in 445 5201; do
    iptables -t nat -A PREROUTING -d "$PUB_ADDR" -p tcp --dport $port -j DNAT --to-destination "$TARGET:$port"
done
iptables -t nat -A PREROUTING -d "$PUB_ADDR" -p tcp --dport 4450 -j DNAT --to-destination "$TARGET:445"
iptables -t nat -A POSTROUTING -d "$TARGET" -j SNAT --to-source "$BACK_ADDR"

# shellcheck disable=SC2086  # NETEM is a list of tc arguments
tc qdisc replace dev "$pub_if" root netem $NETEM limit 100000
# shellcheck disable=SC2086
tc qdisc replace dev "$back_if" root netem $NETEM limit 100000

# Warm-up: ARP passes through netem as well, so resolve both neighbours now rather than during
# the first measured connection.
# One retry, because wan-bad loses packets.
for host in "$TARGET" "${PUB_ADDR%.*}.1"; do
    ping -c 1 -W 5 "$host" >/dev/null || ping -c 1 -W 5 "$host" >/dev/null
done

echo "router up: $pub_if/$back_if netem $NETEM"
exec sleep infinity
