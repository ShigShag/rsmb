#!/bin/sh
set -eu
ln -sf "role-$ROLE.conf" /etc/samba/role.conf

# Independent count of TCP connections to port 445 from outside the container (testenv/ctl.sh
# conns). The nat table is consulted only for the first packet of a connection, so a retransmitted
# SYN is not counted twice; loopback traffic (the healthcheck) never passes nat PREROUTING.
iptables -t nat -A PREROUTING -p tcp --dport 445 ! -i lo -j ACCEPT

# Raw TCP ceiling for bulk transfers, reached through the wan router on port 5201.
if [ "$ROLE" = main ]; then iperf3 -s -D; fi

exec smbd --foreground --no-process-group --debug-stdout
