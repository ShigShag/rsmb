# Router that adds latency at a forwarding hop (serves wan and wan-bad). See docs/test-environment.md.
FROM alpine:3.24.4
RUN apk add --no-cache iptables iproute2
COPY router.sh /usr/local/bin/
ENTRYPOINT ["sh", "/usr/local/bin/router.sh"]
