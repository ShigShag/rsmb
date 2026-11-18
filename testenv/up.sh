#!/usr/bin/env bash
# Builds the images where needed and starts the test environment. Returns when every service is
# healthy. Safe to run when the environment is already up. See docs/test-environment.md.
set -euo pipefail
cd "$(dirname "$0")"

# The routers load sch_netem on first use. After a kernel upgrade without a reboot the module for
# the running kernel is gone from disk, and the slow endpoints would fail to start.
k=$(uname -r)
if [ ! -d /sys/module/sch_netem ] && ! compgen -G "/lib/modules/$k/kernel/net/sched/sch_netem.ko*" >/dev/null \
    && ! grep -qs sch_netem "/lib/modules/$k/modules.builtin"; then
    echo "up.sh: no netem module for the running kernel $k in /lib/modules/$k." >&2
    echo "The kernel was probably upgraded without a reboot. Reboot, then run up.sh again." >&2
    exit 1
fi

# The classic builder (no buildx here) ignores --quiet, so the build log is shown only on failure.
echo "up.sh: building images (cached: seconds; from nothing: about 68 s)"
log=$(mktemp)
trap 'rm -f "$log"' EXIT
if ! docker compose build >"$log" 2>&1; then
    cat "$log" >&2
    echo "up.sh: image build failed" >&2
    exit 1
fi
# An interrupted spike can leave a server paused or the blackhole on; undo both (under the
# exclusive lock, so a spike that is still running is not disturbed).
flock -x -E 99 -w 1800 /tmp/rsmb-env.lock bash -c \
    'docker ps -q --filter name=rsmb- --filter status=paused | xargs -r docker unpause >/dev/null; ./ctl.sh blackhole off' || {
    [ $? -ne 99 ] || echo "up.sh: could not take /tmp/rsmb-env.lock within 30 minutes: another test run, spike or benchmark holds it (docs/test-environment.md, 'The lock')" >&2
    exit 1
}
# A server that was paused for a while still reads "unhealthy" until its next check: try twice.
docker compose up -d --wait || { sleep 5; docker compose up -d --wait; }
