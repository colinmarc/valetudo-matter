#!/bin/sh
umask 077
export VALETUDO_MATTER_BASE_URI=http://127.0.0.1:80
export VALETUDO_MATTER_PERSISTENCE=/mnt/data/valetudo-matter/persistence
export RUST_LOG=info
# This Matter implementation runs on the same native host and Wi-Fi interface as Valetudo.
export VALETUDO_MATTER_NATIVE_DIAGNOSTICS=1
mkdir -p "$VALETUDO_MATTER_PERSISTENCE"
child=
trap 'if [ -n "$child" ]; then kill "$child" 2>/dev/null; wait "$child" 2>/dev/null; fi; exit 0' TERM INT
while :; do
    # Keep logs in RAM, retaining the previous attempt for diagnosis.
    if [ -f /tmp/valetudo-matter.log ]; then
        mv /tmp/valetudo-matter.log /tmp/valetudo-matter.previous.log
    fi
    /mnt/data/valetudo-matter/valetudo-matter >> /tmp/valetudo-matter.log 2>&1 &
    child=$!
    wait "$child"
    child=
    sleep 30 &
    child=$!
    wait "$child"
    child=
done
