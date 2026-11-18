# microsocks from a pinned release (Alpine has it only in edge/testing). Built and the compiler
# removed in one layer: a multi-stage build would leave its build stage as a dangling image with
# the classic builder (no buildx on this machine).
FROM alpine:3.24.4
RUN apk add --no-cache --virtual .build build-base \
    && wget -q -O /m.tgz https://github.com/rofl0r/microsocks/archive/refs/tags/v1.0.5.tar.gz \
    && echo "939d1851a18a4c03f3cc5c92ff7a50eaf045da7814764b4cb9e26921db15abc8  /m.tgz" | sha256sum -c - \
    && tar xzf /m.tgz && make -C microsocks-1.0.5 && cp microsocks-1.0.5/microsocks /usr/local/bin/ \
    && rm -rf /m.tgz microsocks-1.0.5 && apk del .build
# Port 1080 without authentication, 1081 with tester/testpass. Logging stays on: the line
# "connected to <name>:<port>" is the proof that a client went through the proxy.
ENTRYPOINT ["sh", "-c", "microsocks -i 0.0.0.0 -p 1080 & exec microsocks -i 0.0.0.0 -p 1081 -u tester -P testpass"]
