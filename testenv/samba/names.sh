# Exact bytes of the unusual fixture names, written as printf byte escapes so this file stays ASCII.
# Sourced by fixtures.sh (image build, busybox sh) and by check.sh (host, bash).
# docs/test-environment.md lists the same bytes; change both together.

# East Asian, used in cjk/
ZH_HANS=$(printf '\xe7\xae\x80\xe4\xbd\x93\xe4\xb8\xad\xe6\x96\x87')            # Chinese simplified
ZH_HANT=$(printf '\xe7\xb9\x81\xe9\xab\x94\xe4\xb8\xad\xe6\x96\x87')            # Chinese traditional
KO_NFC=$(printf '\xed\x95\x9c\xea\xb8\x80')                                     # Hangul, precomposed
KO_NFD=$(printf '\xe1\x84\x92\xe1\x85\xa1\xe1\x86\xab\xe1\x84\x80\xe1\x85\xb3\xe1\x86\xaf') # Hangul, jamo
JA_HIRA_NFD=$(printf '\xe3\x81\x8b\xe3\x82\x99\xe3\x81\xa3\xe3\x81\x93\xe3\x81\x86') # hiragana, combining dakuten
JA_KATA_NFD=$(printf '\xe3\x83\x8f\xe3\x82\x9a\xe3\x83\xb3')                    # katakana, combining handakuten
HAN_SMP=$(printf '\xf0\xa0\xae\xb7\xe9\x87\x8e\xe5\xae\xb6')                    # U+20BB7 outside the BMP
FULLWIDTH=$(printf '\xef\xbc\xa6\xef\xbc\xb5\xef\xbc\xac\xef\xbc\xac')          # full-width Latin
IDEO_SPACE=$(printf '\xe6\x9d\xb1\xe4\xba\xac\xe3\x80\x80\xe5\xa4\xa7\xe9\x98\xaa') # contains U+3000

# Other scripts and forms, used in special/
UMLAUT=$(printf 'Gr\xc3\xbc\xc3\x9fe')
CYRILLIC=$(printf '\xd0\x9f\xd1\x80\xd0\xb8\xd0\xb2\xd0\xb5\xd1\x82')
EMOJI=$(printf '\xf0\x9f\x98\x80')
CAFE_NFC=$(printf 'caf\xc3\xa9')
CAFE_NFD=$(printf 'cafe\xcc\x81')

# Legacy encodings (not UTF-8), used in mangled/
EUCKR=$(printf '\xc7\xd1\xb1\xdb')     # the Hangul word above in EUC-KR
GBK=$(printf '\xd6\xd0\xce\xc4')       # "Chinese" (two Han characters) in GBK
SJIS=$(printf '\x93\xfa\x96\x7b')      # "Japan" (two Han characters) in Shift-JIS

# The nine East Asian names, one per line (none contains a newline)
CJK_NAMES="$ZH_HANS
$ZH_HANT
$KO_NFC
$KO_NFD
$JA_HIRA_NFD
$JA_KATA_NFD
$HAN_SMP
$FULLWIDTH
$IDEO_SPACE"

# special/: one name per line. Leading spaces are part of the second name.
SPECIAL_NAMES="with space.txt
  two leading spaces.txt
$UMLAUT.txt
$CYRILLIC.txt
$EMOJI smile.txt
$CAFE_NFC.txt
$CAFE_NFD.txt
it's.txt
-rf
--help.txt
[abc].txt
{a,b}.txt
bang!.txt
#hash.txt
\$dollar.txt
semi;colon.txt
per%cent.txt
amp&.txt
tick\`.txt
Case.txt
case.txt"

# mangled/: names Samba cannot present as they are. The last one ends in a space.
MANGLED_NAMES="$EUCKR.txt
$GBK.txt
$SJIS.txt
colon:name.txt
question?.txt
star*.txt
pipe|.txt
less<more>.txt
quote\".txt
back\\slash.txt
trailingdot.
trailingspace "
