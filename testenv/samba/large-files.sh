#!/bin/sh
# The large fixture files, in their own image layer so that changing fixtures.sh does not
# regenerate 5.5 GiB. Deterministic: AES-128-CTR keystream, fixed key, one IV per file.
# Every length is odd, so none divides by 64 KiB, 1 MiB or 8 MiB (or any other chunk size).
set -eu
dir=$1
KEY=000102030405060708090a0b0c0d0e0f
gen() { openssl enc -aes-128-ctr -K "$KEY" -iv "$2" -in /dev/zero 2>/dev/null | head -c "$3" > "$dir/$1"; }
mkdir -p "$dir"
gen big.bin    00000000000000000000000000000001 1181116007   # 1.1 GiB
gen over4g.bin 00000000000000000000000000000002 4617089843   # 4.3 GiB, offsets above 2^32
gen medium.bin 00000000000000000000000000000003 52432897     # 50 MiB + 4097
