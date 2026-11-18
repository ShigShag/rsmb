#!/bin/sh
# Creates every fixture except the large files (large-files.sh). Runs once, at image build, as root.
# Each fixture and what it targets is described in docs/test-environment.md.
# Usage: fixtures.sh <root>   (XL=1 in the environment adds xl/ with 1,000,000 files)
set -eu
root=$1
here=$(dirname "$0")
. "$here/names.sh"
KEY=000102030405060708090a0b0c0d0e0f

# Deterministic pseudo-random bytes: AES-128-CTR keystream with a fixed key and the given IV.
stream() { openssl enc -aes-128-ctr -K "$KEY" -iv "$1" -in /dev/zero 2>/dev/null | head -c "$2"; }
# n empty files named <prefix><zero-padded number> in dir
emptyfiles() { mkdir -p "$1"; (cd "$1" && seq -f "$2%0${3}.0f" 1 "$4" | xargs touch); }

share=$root/share
mkdir -p "$share" "$root/guest"
cd "$share"

# --- scale ---
emptyfiles many f 5 10000
emptyfiles huge f 6 100000
if [ "${XL:-0}" = 1 ]; then emptyfiles xl f 7 1000000; fi

mkdir longnames
pad=$(printf '%0245d' 0 | tr 0 x)
seq -f "%04.0f_$pad" 1 2000 | (cd longnames && xargs touch)   # names of 250 bytes

mkdir fanout
for d in $(seq -f 'd%04.0f' 1 2000); do mkdir "fanout/$d"; echo "$d" > "fanout/$d/file.txt"; done

# tree/: breadth 4, depth 4 = 4+16+64+256 = 340 directories, one file in each
mktree() {
    for b in d0 d1 d2 d3; do
        mkdir "$1/$b"
        echo "$1/$b" > "$1/$b/file.txt"
        if [ "$2" -lt 4 ]; then mktree "$1/$b" $(($2 + 1)); fi
    done
}
mkdir tree && mktree tree 1

p=deep;  for i in $(seq -w 1 12); do p=$p/l$i; done; mkdir -p "$p" && echo bottom > "$p/bottom.txt"
p=chain; for i in $(seq -w 1 60); do p=$p/c$i; done; mkdir -p "$p" && echo bottom > "$p/bottom.txt"

# smallfiles/: 20 directories x 100 files of 1024..4096 bytes, sliced from one keystream
stream 00000000000000000000000000000004 8388608 > /tmp/pool
for d in $(seq -w 1 20); do
    mkdir -p "smallfiles/d$d"
    for f in $(seq -w 1 100); do
        i=$(( (1$d - 101) * 100 + (1$f - 1001) ))   # 0..1999; the 1 prefix avoids octal
        size=$(( 1024 + (i * 997) % 3073 ))
        dd if=/tmp/pool of="smallfiles/d$d/f$f.bin" iflag=skip_bytes,count_bytes \
            skip=$((i * 4096)) count=$size status=none
    done
done
rm /tmp/pool

# --- sizes ---
mkdir sizes
for n in 0 1 65535 65536 65537 1048575 1048576 1048577 8388607 8388608 8388609; do
    stream 00000000000000000000000000000005 "$n" > "sizes/$n.bin"
done

# --- names --- (no globbing, split lists on newlines only)
set -f
IFS='
'
mkdir cjk
for b in $CJK_NAMES; do mkdir "cjk/$b"; echo "$b" > "cjk/$b/$b.txt"; done
set -- $CJK_NAMES
i=0
while [ $i -lt 1000 ]; do
    eval "b=\${$((i % 9 + 1))}"
    echo "$i" > "cjk/${b}_$(printf %04d $i).txt"
    i=$((i + 1))
done
p=cjk/nested; for b in $CJK_NAMES; do p=$p/$b; done
mkdir -p "$p" && echo leaf > "$p/leaf.txt"

mkdir special
for n in $SPECIAL_NAMES; do printf '%s\n' "$n" > "special/$n"; done
p="special/dir with space/$UMLAUT/$CYRILLIC/$EMOJI"
mkdir -p "$p" && echo leaf > "$p/leaf.txt"

# Names Samba cannot present as they are on disk: legacy encodings, characters Windows forbids,
# trailing dot or space.
mkdir mangled
for n in $MANGLED_NAMES; do printf '%s\n' "$n" > "mangled/$n"; done
unset IFS
set +f

# --- hostile structure ---
mkdir forbidden tree/forbidden traverse-only loops upload
echo secret > forbidden/secret.txt
echo secret > tree/forbidden/secret.txt
echo secret > tree/unreadable.txt
echo known > traverse-only/known.txt
echo loop > loops/file.txt
ln -s . loops/self
ln -s .. loops/up
ln -s /nonexistent loops/dangling
# Two paths to one directory: a link to a sibling (which of the two a walk lists).
mkdir -p links/a links/b/c
echo linked > links/b/c/file.txt
ln -s ../b/c links/a/to-c

echo "guest share" > "$root/guest/hello.txt"

chown -R root:root "$share" "$root/guest"
chmod 0700 forbidden tree/forbidden
chmod 0600 tree/unreadable.txt
chmod 0711 traverse-only
chown tester:tester upload
