# One image for samba, samba-edge and samba-private; ROLE selects the configuration.
FROM alpine:3.24.4

# coreutils: GNU dd (iflag=nocache for cache eviction, skip_bytes for fixtures) and seq -f.
# iptables: the connection counter. iperf3: the raw TCP ceiling.
RUN apk add --no-cache samba-server samba-client samba-common-tools coreutils openssl \
        iperf3 iptables \
    && adduser -D -H -s /sbin/nologin tester

COPY large-files.sh /build/
RUN sh /build/large-files.sh /srv/share/large

ARG XL=0
COPY names.sh fixtures.sh /build/
RUN XL=$XL sh /build/fixtures.sh /srv

COPY smb.conf role-main.conf role-edge.conf role-private.conf /etc/samba/
RUN for i in $(seq -w 1 300); do \
        name=edge$i; [ $((1$i % 10)) = 0 ] && name="edge$i\$"; \
        printf '[%s]\n  path = /srv/guest\n  comment = edge share %s\n  read only = yes\n' "$name" "$i"; \
    done >> /etc/samba/role-edge.conf \
    && ln -s role-main.conf /etc/samba/role.conf \
    && testparm -s >/dev/null \
    && printf 'testpass\ntestpass\n' | smbpasswd -a -s tester

COPY entrypoint.sh /usr/local/bin/
ENTRYPOINT ["sh", "/usr/local/bin/entrypoint.sh"]
