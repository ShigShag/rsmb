# Performance

What rsmb costs in requests, round trips and time, against the targets of the brief and of
[`plans/master-plan.md`](plans/master-plan.md) section 5, and beside smbclient-ng. How requests
and flights are counted: [`measuring.md`](measuring.md). The tests that enforce each target:
[`testing.md`](testing.md).

## The final comparison

From the final benchmark run on 2026-11-18, on the final code (commit `93712f1` with the Stage 4
changes, before their commit), rsmb 0.1.0, smbclient-ng 3.1.0 (impacket 0.13.1), Samba 4.23.8:
`testenv/bench-smbclientng.sh` (a fresh baseline on the current image), `testenv/bench-rsmb.sh`,
its `transfers` mode (the 50 MiB rows from a second run of that mode's last section, below) and
its `walk` mode; netem dropped nothing in any of them. Every cell is from those runs except the
proxy row, which is the measurement made while planning Stage 4 and says so. The sections below
have the whole output of each run.

| At 100 ms each way | smbclient-ng 3.1.0 | rsmb | Faster by |
|---|---|---|---|
| `cd` | 1,609 ms; 6 requests in 6 round trips; 2 new TCP connections | 201 ms; 2 requests in 1 round trip; none | 8× |
| `ls`, 5 entries | 1,208 ms; 5 round trips | 201 ms; 5 requests in 1 round trip | 6× |
| `ls`, 10,000 entries | 4,559 ms; 17 round trips | 424 ms (1.5 s as the first listing of a connection); 1 round trip | 11× (3×) |
| `ls`, 100,000 entries | 37.6 s; 139 round trips | 3.2 s; 1 round trip | 12× |
| `tree`, 85 directories | 69.3 s; 342 round trips, one after another | 1.61 s; 340 requests in 47 flights | 43× |
| download, 50 MiB | 16.7 s (median of 3) | 5.7 s (median of 3) | 2.9× |
| upload, 50 MiB | 66.6 s (median of 3) | 5.7 s (median of 3) | 12× |
| 1.1 GiB each way | not run (about 6 and 24 minutes at its 50 MiB rates) | 62.9 s and 63.6 s (medians of 3), 18.8 and 18.6 MB/s against iperf3's 17.6 and 17.3 | |
| the same through a SOCKS5 proxy (measured while planning, 2026-11-18, one session each) | no proxy option | `cd` and `ls` unchanged (201 ms); walks and small files 1.1 to 1.3×, a 50 MiB download 1.6× the direct time with microsocks | |
| a second Tab in a directory | a full listing each time | 0 requests | |

The two transfer rows are process times (with the command − without it), so they include the
start of a TCP connection in slow start: rsmb's 50 MiB download reaches about 9 MB/s, not the
18.8 MB/s of the 1.1 GiB file.

## Targets and measured values

Samba 4.23.8; slow = 100 ms each way (200 ms round trip), fast = direct. Measured 2026-11-18.

| Target | Must hold | Measured | Test |
|---|---|---|---|
| no command opens another TCP connection (brief) | 1 connection in the tap and on the server for a session that runs every command | 1 and 1, fast and edge | `one_connection_fast`, `one_connection_edge` |
| `cd` costs at most two requests (brief) | CREATE + CLOSE, 1 flight | 2 requests, 1 flight, one compound (fast, slow, CJK and spaced names) | `cd_is_one_compound`, `cd_slow` |
| a second Tab in the same directory costs 0 requests (brief) | first Tab one listing, second Tab 0, Tab after `ls` 0 | 5 / 1, then 0, 0 (`many/`, `huge/`, slow) | `completion_costs_slow`, `completion_costs` |
| report `cd` and `ls` times at 100 ms (brief) | table below | below | `bench-rsmb.sh`, `timing_*` |
| start-up | NEGOTIATE + 2 SESSION_SETUP, nothing else | 3 requests in 3 flights | `startup_is_three_requests`, `startup_and_use_slow` |
| start-up with `-k` (Kerberos) | NEGOTIATE + 1 SESSION_SETUP | 2 requests in 2 flights, with the service ticket in the cache and (direct) with a TGT only; the KDC asked once per login with a TGT only, 0 times with the service ticket | `kerberos_login`, `kerberos_kdc_is_asked_through_the_proxy` |
| `use share` | 1 the first time, 0 after | 1 / 0 | `use_once_per_share` |
| `cd /`, `cd ..`, `pwd`, `help`, `refresh` | 0 requests | 0 | `free_commands_send_nothing` |
| `ls` small, slow | ≤ f = 1 flight | 5 requests, 1 flight | `ls_small_slow` |
| `ls many/`, slow | ≤ 4 flights | 1 flight | `ls_many_slow` |
| `ls huge/`, slow | ≤ 7 flights, peak RSS ≤ 150 MB | 1 flight, 43.3 MB peak for the whole process | `ls_huge_slow` |
| idle 12 s | 0 requests | 0 | `idle_sends_nothing` |
| `cd` wall time, slow | ≤ 1.25 × connect time | 200.9 ms against 200.4 ms (gate), 200.7 against 200.70 (bench) | `timing_cd` |
| `cd` wall time, fast | ≤ 20 ms | 0.5 ms (gate), 0.2 ms (bench) | `timing_cd` |
| `ls` small wall time, slow | ≤ 1.25 × 1 × connect time | 200.9 ms against 200.2 ms (gate), 200.7 against 200.81 (bench) | `timing_ls_small` |
| a 1.1 GiB file each way (brief), fast | sha256 equal, ≤ 16 outstanding, peak RSS ≤ 64 MB | 1.8 s down, 2.1 s up; 8 outstanding; 21.9 MB | `big_file` |
| a 4.3 GiB file each way (offsets above 2^32) | sha256 equal | 4.5 s down, 6.7 s up at fast; 239.3 s down, 239.0 s up at slow (19.3 MB/s) | `over_4g`, bench |
| bulk rate at slow | beside iperf3 | `big.bin` 18.6–18.8 MB/s down, 18.5–18.7 up, against iperf3 17.6 / 17.3 (three runs each, within about 1 %) | bench |
| 100 small files at slow (`d01`) | ≤ 1.5 × 8 round trips down, ≤ 1.5 × 22 up | 1,689 ms down (bound 2,402; one after another 20.2 s), 4,523 ms up (bound 6,606; one after another 60.3 s) | `timing_small_files` |
| 2,000 small files | requests 6,084 down, 14,042 up; ≤ 64 / ≤ 48 outstanding; RSS ≤ 64 MB | exactly those; 64 / 48; 9.2 MB / 10.7 MB; at fast 2.1 s and 4.6 s | `two_thousand_small_files` |
| one connection for every Stage 2 command | 1 in the tap, 1 on the server | 1 and 1, fast and edge | `one_connection_fast`, `one_connection_edge` |
| a walk keeps 16 listings in flight (3.3) | `find /tree`, `tree /tree`: 1,368 requests, peak outstanding ≤ 64, one connection | 1,368; 64; 1 | `walk_counts_slow` |
| walk wall time, slow (3.3) | `tree/` ≤ 1.25 × 23 waves × round trip (5.75 s) | `find /tree` 4.82 s, `tree /tree` 4.62 s (gate); 4.82 s three times (bench) | `timing_walk_slow` |
| deep, narrow trees (3.7) | one flight per level | `chain/` 244 / 61, `deep/` 52 / 13 (2.71 s at slow) | `walk_counts_fast`, `walk_deep_slow` |
| walk on 16 credits (3.4) | `fanout/` within 60 s, never a credit not granted | 0.79 s, 8,011 requests, peak outstanding 16, lowest balance 0 | `walk_edge` |
| a walk's memory | `find /huge`, `tree /huge` peak RSS ≤ 64 MB | 28.7 MB and 25.1 MB for the whole process | `walk_counts_fast` |
| a closed stdout stops `find` | ≤ 200 requests after the reader left | 0 (8 in all, exit 1 after 2.1 s) | `find_and_a_closed_pipe` |
| everything through the SOCKS5 proxy (4.1) | the same requests and flights as direct, every count assertion of `tests/fast.rs` | all 69 tests of `tests/fast.rs` pass through port 1080 with their counts unchanged; 33 commands counted equal while planning | the proxied run of `tests/fast`, `one_connection_through_the_proxy` |
| one connection through the proxy (4.3) | 1 in the tap, 1 on the private server, 1 proxy log line per session | 1, 1, 1 on both ports; a reconnect 1 more of each | `one_connection_through_the_proxy` |
| a 1.1 GiB file through the proxy (4.8) | sha256 equal, ≤ 16 outstanding, peak RSS ≤ 64 MB, 0 new connections | 1.41 s down, 1.69 s up at fast; 12.8 / 13.7 MB | `big_file_through_the_proxy` |

## Navigation against smbclient-ng

`./testenv/bench-rsmb.sh`, 2026-11-18 (the final run of Stage 4, on the final code: commit
`93712f1` plus the Stage 4 working tree), 5 runs per row, 505 s, netem dropped
nothing. rsmb in text mode, `rsmb -H <host> … -N -C 'use share' [-C <setup>] -C <cmd> × N`; the
smbclient-ng columns are the baseline renewed the same day on the current image
(`testenv/baseline-smbclientng.txt`, 1,067 s, `docs/measuring.md`). Counts are per command;
`reqs` includes encrypted frames (smbclient-ng's are all encrypted).

```
endpoint command                          N  conn_ms    ms/cmd    fresh   paused  conns   reqs flights |  ng_ms/cmd ng_req   ng_fl
fast   cd /tree/d{0..3}                10     0.44       0.2        -        -    0.0    2.0     1.0 |        4.1    6.0     6.0
fast   ls small (tree/d0, 5 entries)   10     0.48       0.3        -        -    0.0    5.0     1.0 |        3.9    5.0     5.0
fast   ls many/ (10,000 entries)       10     0.48      92.1        -        -    0.0    5.0     1.0 |      823.7   17.0    17.0
fast   ls huge/ (100,000 entries)       1     0.45     906.4        -        -    0.0    5.0     1.0 |     8308.2  139.0   139.0
slow   cd /tree/d{0..3}                10   200.70     200.7        -        -    0.0    2.0     1.0 |     1608.8    6.0     6.0
slow   ls small (tree/d0, 5 entries)   10   200.81     200.7    202.2    201.0    0.0    5.0     1.0 |     1207.8    5.0     5.0
slow   ls many/ (10,000 entries)       10   200.71     423.5   1511.8    969.4    0.0    5.0     1.0 |     4558.7   17.0    17.0
slow   ls huge/ (100,000 entries)       1   200.68    3174.0   3169.8   3224.8    0.0    5.0     1.0 |    37561.1  139.0   139.0

wan-bad  ls huge/ once, default --timeout: 121.1 s (start-up included), 100000 entries

rsmb 93712f1+changes, Samba 4.23.8, 5 run(s) per row, 2026-11-18, 505 s
smbclient-ng columns: smbclient-ng v3.1.0, Samba 4.23.8, 5 run(s) per row, 2026-11-18, 1067 s
netem drops on wan during the run: 0
```

Columns: `conn_ms` the TCP connect time just before the row; `ms/cmd` = median over 5 runs of
(t_N − t_0) ÷ N; `fresh` = t_1 − t_0, the first listing of a new connection; `paused` = the mean
time of each measured command sent over stdin 1 s after the previous one finished; `conns`,
`reqs`, `flights` per command from one 0-run and one N-run through `smbtap`.

Reading it:

- At the slow endpoint `cd` and a small `ls` are one round trip (201 ms against a 201 ms connect),
  8× and 6× faster than smbclient-ng (1,609 and 1,208 ms). No command opens a TCP connection (smbclient-ng opens one
  or two per command for its liveness probe).
- Every listing is 5 requests in 1 flight. What remains above one round trip for `many/` and
  `huge/` is the reply's size: 1.2 MiB and 11.4 MiB through a 200 ms path. `fresh` and `paused`
  show TCP slow start: on a new connection, or after 1 s of quiet, the window has to grow again
  (`many/` 1.5 s fresh, 1.0 s after a pause, 0.4 s back to back). `huge/` with N = 1 is a
  fresh listing in every column (3.2 s; about 1.9 s warm, `docs/library-decision.md`).
- At the fast endpoint the large listings are bound by the server enumerating the directory
  (about 0.9 s for 100,000 entries) and by printing the lines; smbclient-ng is about 9× slower.
- The cd row alternates `/tree/d0` … `/tree/d3`, because rsmb sends nothing for a `cd` to the
  working directory or above; smbclient-ng's row repeated `cd /tree/d0`, which it checks every
  time. Both rows therefore measure one checked `cd`. The bench voids a row whose tapped run does
  not show 2 requests per `cd` or 5 per `ls`.
- **wan-bad** (150 ms ± 40 ms each way, 1 % loss, 20 Mbit/s): `ls huge/` (one 11.4 MiB reply)
  completed in 121.1 s with the default `--timeout 30` (96.4 s in the Stage 1 run: the lossy link
  varies), under its limit of 6 × 30 s = 180 s for one reply. It is the end-to-end check that a reply still arriving is not taken for silence; without
  receive progress the connection was torn down after 30 s (measured while planning). In the
  gate, `wan_bad_smoke` lists `many/` there (12.8 s on the last run).

## Transfers

`./testenv/bench-rsmb.sh transfers`, 2026-11-18 (the final run, the same code), 1,616 s,
exclusive lock, netem dropped nothing, round trip at slow 200.2 ms. Seconds are the product's own
(`seconds` of the `transfer` or `summary` object: from the command's first request to its last
answer); every file was checked by sha256 on both sides; iperf3 (10 s each direction) ran just
before the rows it stands beside. The `medium.bin` rows beside smbclient-ng are below
("50 MiB beside smbclient-ng").

```
endpoint row                                    seconds     MB/s  beside
slow    big.bin down, run 1                     63.513     18.6  iperf3 -R 17.6 MB/s
slow    big.bin up, run 1                       63.626     18.6  iperf3 17.3 MB/s
slow    big.bin down, run 2                     62.865     18.8  iperf3 -R 17.6 MB/s
slow    big.bin up, run 2                       63.262     18.7  iperf3 17.3 MB/s
slow    big.bin down, run 3                     62.917     18.8  iperf3 -R 17.6 MB/s
slow    big.bin up, run 3                       63.832     18.5  iperf3 17.3 MB/s
slow    over4g.bin down                        239.293     19.3  checksum ok
slow    over4g.bin up                          238.992     19.3  checksum ok
slow    get -r /smallfiles (2,000 files)         27.91        -  wave bound 126 x 1 rt = 25 s, one after another 2,000 rt = 400 s
slow    put -r smallfiles                       77.832        -  wave bound 126 x 3 rt = 76 s, one after another 6,000 rt = 1201 s
        get -r /smallfiles through the tap: {"requests":6090,"flights":150,"peak_outstanding":64,"connections":1}
        put -r smallfiles through the tap: {"requests":14048,"flights":3755,"peak_outstanding":48,"connections":1}
slow    get -r /tree (341 dirs, 2 denied)        9.552        -  23 waves + 22 x 1 rt = 9 s
wan-bad sizes/8388609.bin down                  56.207      0.1  iperf3 -R 0.3 MB/s
wan-bad sizes/8388609.bin up                    71.875      0.1  iperf3 0.1 MB/s
fast    get -r /smallfiles (sync each file)      1.513        -  2,000 renames without a sync 0.07 s; with a sync one after another 12.3 s (plan 2.5)

rsmb 93712f1+changes, Samba 4.23.8, 2026-11-18, 1616 s; round trip at slow 200.2 ms; netem drops on wan: 0
```

Reading it:

- **Bulk at the slow endpoint runs at the raw TCP ceiling.** `big.bin` (1.1 GiB) moved at 18.6–18.8 MB/s
  down and 18.5–18.7 MB/s up in three runs each (spread about 1 %), against 17.6 and 17.3 MB/s for
  iperf3 over 10 s through the same router; `over4g.bin` at 19.3 MB/s each way. The rate is bound
  by TCP windows at 200 ms, not by the client: the crate keeps up to 4 MiB in flight (8 READs of 512
  KiB were seen outstanding at the fast endpoint). A short transfer is still in slow start:
  `medium.bin` (50 MiB) ran at 10.8 / 10.0 MB/s in `timing_medium_slow`.
- **Small files go 16 at a time.** 2,000 files of 1–4 KiB at 200 ms: 27.9 s down against the wave
  bound of 25 s (126 waves of one round trip) and 400 s one after another; 77.8 s up against 76 s
  and 1,201 s (three round trips a file: the compound write, its CLOSE, the rename). The tapped
  runs show the counts the gate asserts (6,084 and 14,042, plus the 6 requests of start-up, `use`
  and `cd`), 64 and 48 requests outstanding at most, one connection.
- **The walk is one flight per listing.** `get -r /tree` (341 directories, 340 files, 2 denied)
  took 9.6 s against 23 listing waves plus 22 file waves of one round trip (9 s). Stage 0's walk
  through the crate took 18.5 s for the listings alone (4 flights a listing).
- **wan-bad** (150 ± 40 ms each way, 1 % loss, 20 Mbit/s) carried 0.1 to 0.3 MB/s with iperf3
  and 0.1 MB/s with rsmb: 8 MiB in 56 s down and 72 s up (68 and 56 s in the Stage 2 run). The link, not the client, is the limit; every
  512 KiB request there arrived well within its 180 s.
- **The sync before each rename** (P7) costs little when 16 files overlap: 2,000 small files at
  the fast endpoint in 1.5 s with a `sync_data` each, against 0.07 s with no sync and 12.3 s with
  one sync after another (measured while planning).

Per-file costs on Samba (requests / round trips), all asserted by `transfer_costs`,
`threshold_boundary` and `two_thousand_small_files`:

| Transfer | Requests | Round trips |
|---|---|---|
| `get` of one file up to 512 KiB (the stream path) | 7 | 3 |
| a small file inside `get -r` | 3 | 1 (16 at a time) |
| `put` of at most 256 KiB | 7 | 3 |
| `put` above 256 KiB | 2 + 4 + one WRITE per 512 KiB + FLUSH + CLOSE + 3 | 4 + the stream |
| a small `put` refused because the target exists | 13 | 6 |
| a walk listing | 4 | 1 (16 at a time) |
| `mkdir`; `rm`, `rmdir` | 2; 3 | 1 |

**Memory** (VmHWM of the whole process, `big_file`, `two_thousand_small_files`, `cat_output`):
21.9 MB after `big.bin` down and up, 9.2 MB after `get -r /smallfiles`, 10.7 MB after the `put
-r`, under 64 MB for `cat` of 50 MiB.

### 50 MiB beside smbclient-ng

`medium.bin` (52,432,897 bytes) at the slow endpoint, both clients by one method: the process time
of a run with the command minus that of the same run without it, three runs each, sha256 checked
on the receiving side; smbclient-ng's run counts only with its login line, no error line and its
prompts, as in `bench-smbclientng.sh`. These rows come from a second run of the `transfers` mode's
last section on the same code and day: in the full run the smbclient-ng download rows failed (the
bench had changed into the upload directory first, so smbclient-ng's relative `large/medium.bin`
did not exist, and the failure inside `$(…)` did not stop the script); the script was fixed and
only that section was run again (`docs/plans/stage-4.md`, deviations).

```
endpoint row                                    seconds     MB/s  beside
slow    rsmb medium.bin down, run 1               5.75        -  process time
slow    smbclient-ng medium.bin down, run 1      16.66        -  process time
slow    rsmb medium.bin down, run 2               5.74        -  process time
slow    smbclient-ng medium.bin down, run 2      16.64        -  process time
slow    rsmb medium.bin down, run 3               4.84        -  process time
slow    smbclient-ng medium.bin down, run 3      16.68        -  process time
slow    rsmb medium.bin up, run 1                 5.85        -  process time
slow    smbclient-ng medium.bin up, run 1        66.58        -  process time
slow    rsmb medium.bin up, run 2                 5.65        -  process time
slow    smbclient-ng medium.bin up, run 2        66.52        -  process time
slow    rsmb medium.bin up, run 3                 5.65        -  process time
slow    smbclient-ng medium.bin up, run 3        66.70        -  process time

rsmb 93712f1+changes, Samba 4.23.8, 2026-11-18, 374 s; round trip at slow 200.2 ms; netem drops on wan: 0
```

smbclient-ng moved 50 MiB in 16.7 s down (3.1 MB/s) and 66.6 s up (0.79 MB/s); rsmb in 5.7 s
each way. The full run's rsmb rows were 4.84, 4.84, 5.74 s down and 5.85, 5.45, 5.45 s up.

## Walks (`tree`, `find`)

`./testenv/bench-rsmb.sh walk`, 2026-11-18 (the final run, the same code), 364 s, shared lock,
netem dropped nothing, round trip at slow 200.6 ms. The `tree /tree/d0` rows are one process per
run, (time with the command − time without it), for both clients, with the counts from a further
run through the tap (requests include encrypted frames; all of smbclient-ng's are); the other rows
are rsmb's own `walk_summary.seconds` (from the first listing to the last answer).

```
endpoint row                                   seconds  requests  flights  beside
slow     rsmb tree /tree/d0 (85 dirs), run 1-3 1.61/1.61/1.62       340       47  7 waves = 1.4 s
slow     smbclient-ng tree tree/d0, 1 run        69.30       342      342  one after another 85 x 4 rt = 68.2 s
slow     find /tree, run 1                        4.82         -        -  341 listings, 682 entries; 23 waves = 4.6 s, one after another 342 rt = 68.6 s
slow     find /tree, run 2                       4.825         -        -  341 listings, 682 entries; 23 waves = 4.6 s, one after another 342 rt = 68.6 s
slow     find /tree, run 3                        4.82         -        -  341 listings, 682 entries; 23 waves = 4.6 s, one after another 342 rt = 68.6 s
slow     find /fanout, run 1                    26.328         -        -  2001 listings, 4000 entries; 126 waves = 25.3 s, one after another 2,001 rt = 401.4 s
slow     find /fanout, run 2                    26.323         -        -  2001 listings, 4000 entries; 126 waves = 25.3 s, one after another 2,001 rt = 401.4 s
slow     find /fanout, run 3                    26.329         -        -  2001 listings, 4000 entries; 126 waves = 25.3 s, one after another 2,001 rt = 401.4 s
slow     find /chain                            12.258         -        -  61 listings, 61 entries; 61 rt one after another = 12.2 s
slow     find /huge                              5.349         -        -  1 listings, 100000 entries; ls /huge on a new connection 3.23 s (1 flight)
slow     tree /huge                              5.231         -        -  1 listings, 100000 entries; ls /huge on a new connection 3.23 s (1 flight)
slow     find / (the whole share)               47.009         -        -  2475 listings, 119880 entries; 
wan-bad  find /tree                             10.003         -        -  341 listings, 682 entries; 150 ms each way, 40 ms jitter, 1 % loss, 20 Mbit/s

rsmb 93712f1+changes, Samba 4.23.8, smbclient-ng v3.1.0, 2026-11-18, 364 s; round trip at slow 200.6 ms; netem drops on wan: 0
```

Reading it:

- **`tree tree/d0` is 43× faster than in smbclient-ng**: 1.61 s against 69.3 s for the same 85
  directories, the same number of requests (340 against 342), in 47 to 60 flights (it varies from
  run to run while 16 are in flight) against 342. rsmb
  lists 16 directories at a time on the one connection; smbclient-ng one after another, 4 round
  trips each (`docs/measuring.md`). On all of `tree/` smbclient-ng stops with an `[error]` line at
  the denied directory, so `tree/d0` is the fair row.
- **A walk is bound by its waves.** `tree/` (342 listings) in 4.82 s against 23 waves of one round
  trip (4.6 s) and 68.6 s one after another; `fanout/` (2,001) in 26.3 s against 126 waves
  (25.3 s) and 401 s. Stage 0's 16-wide walk of `tree/` through the crate took 18.5 s (4 flights a
  listing); rsmb's listing is one compound. `chain/` cannot overlap: 61 round trips, 12.25 s.
- **A very large directory costs one flight per MiB inside a walk** (the walk's buffer is 1 MiB so
  that 16 listings fit a new connection's credits): `huge/` 5.35 s (`find`) and 5.23 s (`tree`) in 15 flights against 3.2 s for
  `ls /huge` on a new connection (1 flight of up to 16 MiB). Not changed in Stage 3.
- **The price of the queue order** (results taken in the order the listings were queued, so the
  output is the same on every run): nothing on `tree/` and `fanout/`, where listings take equal
  time; on the whole share 47.0 s here and 46.7 s while planning, against 43.5 s in completion
  order, because the walk waits behind `huge/` (`docs/plans/stage-3.md` 2.3).
- **wan-bad**: `tree/` in 10.0 s on the lossy link (9.1 s in the Stage 3 run).

What a walk costs (requests / flights through the tap at the fast endpoint, measured while
planning and asserted by the tests): one listing is 4 requests in 1 flight (CREATE, a query of
1 MiB, the 64 KiB probe, CLOSE); `tree/` 1,368 / 326 (peak outstanding 64); `fanout/` 8,004 /
1,986 (64); `chain/` 244 / 61 (4); `cjk/` 80 / 11; `loops/` 4 / 1; `many/` 9 / 5; `huge/` 19 / 15;
the whole share (2,471 listings before `links/` was added) 9,921 / 2,356. On edge (16 credits):
`tree/` 1,368, `fanout/` 8,011 (its 2,000-entry root needs the listing's second phase), peak
outstanding 16, no stall.

**Memory** (VmHWM of the whole process, measured while planning and in F43): `find` of the whole
share (119,874 entries, nothing kept) 26.4 MB; `tree` of it (every listing kept) 25.7 MB;
`huge/` alone 25.2 MB streaming, 23.7 MB kept; `fanout/` (2,001 listings) 8.8 MB. About 90 to 140
bytes an entry and about 230 bytes a directory; at `tree`'s limit of 500,000 entries, synthetic,
44 to 68 MB (167 MB when every directory holds one entry).

## Through a SOCKS5 proxy

**Measured while planning Stage 4** (2026-11-18, the release build of commit `93712f1`, Samba
4.23.8, microsocks 1.0.5; `docs/plans/stage-4.md` 2.3 and 2.10), not by a bench mode: there is no
smbclient-ng column to stand beside (it has no proxy option), and what the timed rows show is
microsocks. The counts are asserted by the gate anyway: all of `tests/fast.rs` runs a second time
through the proxy with its count assertions unchanged.

**Requests and flights.** One session per route, each command through `smbtap`: direct to `samba`;
through the tap's SOCKS5 mode and port 1080 to the private server; the same through port 1081
(with a login at the proxy). Requests + opaque frames / flights, peak outstanding:

| Command | Direct | Port 1080 | Port 1081 |
|---|---|---|---|
| start-up | 3 / 3, 1 connection | the same | the same |
| `use share` | 1 / 1 | = | = |
| `cd tree/d0`, `cd d1` | 2 / 1 each | = | = |
| `cd /`, `pwd`, `lcd` | 0 | = | = |
| `ls /sizes`, `ls /many`, `ls /huge` | 5 / 1 each | = | = |
| `shares`, again | 19 / 8, 18 / 7 | = | = |
| `get /sizes/1.bin`, `cat` of a small file | 7 / 3 | = | = |
| `get /sizes/1048577.bin` | 9 / 4 | = | = |
| `put` of 4,000 bytes | 7 / 3 | = | = |
| `put` of 1 MiB + 1 byte | 14 / 7 | = | = |
| `put` refused, target exists | 13 / 6 | = | = |
| `mkdir`; `rmdir`, `rm` | 2 / 1; 3 / 1 | = | = |
| `get -r /smallfiles/d01` | 304, peak 48 (14 flights) | 304, peak 48 (15) | 304, peak 48 (14) |
| `put -r d01` | 702, peak 48 (207) | 702, peak 48 (169) | 702, peak 48 (180) |
| `find /tree`; `tree /tree` | 1,368, peak 64 (316; 302) | 1,368, peak 64 (317; 269) | 1,368, peak 64 (307; 305) |
| `find /chain` | 244 / 61, peak 4 | = | = |
| `tree /loops` | 4 / 1 | = | = |
| `find /fanout` | 8,004, peak 64 (1,982) | 8,004, peak 64 (1,966) | 8,004, peak 64 (1,972) |
| `find /many -name 'f0999?'` | 9 / 5 | = | = |
| `use encrypted`; `ls /sizes` there | 1 / 1; 0 + 1 opaque / 1 | = | = |

The request count is the same on all 33 rows, and so is the flight count wherever requests do not
overlap; where 16 are in flight it varies from run to run, as it does direct. A proxy adds no
request: the handshake is the TCP connect to the proxy and 2 round trips to it (3 with a login),
once per connection; every connection rsmb opened was one line in the proxy's log and one
connection on the private server (5 and 5 across a session with 4 reconnects). An idle session
through port 1081 sent 0 requests in 12 s.

**Time.** Medians of 20 (`cd`, small `ls`) or 5 (`ls /many`), one session per route, each command
timed from its line to its `done` object, `ctl.sh drops` 0; "slow via the proxy" is `--proxy
socks5://10.213.45.20:1080 -H 10.213.45.30` beside `-H 10.213.45.30`: the proxy sits next to the
client on `pub`, so the 100 ms each way lie between the proxy and the server. ms:

| Route | Process with `pwd` | `use` | `cd` | `ls` small | `ls /many` |
|---|---|---|---|---|---|
| fast, direct | 4.0 | 0.5 | 0.21 | 0.35 | 114 |
| fast, port 1080, private server | 4.6 | 0.5 | 0.23 | 0.33 | 103 |
| fast, port 1081, private server | 4.3 | 0.5 | 0.21 | 0.34 | 101 |
| slow, direct | 806.2, 806.1 | 200.9 | 200.67, 200.63 | 200.99, 200.88 | 313.8 |
| slow, port 1080 | 806.2, 806.3 | 201.2 | 200.75, 200.80 | 200.80, 200.93 | 315.7 |
| slow, port 1081 | 806.4, 806.5 | 200.9 | 200.89, 200.93 | 200.88, 200.95 | 315.7 |

Start-up, `cd` and `ls` cost the same through the proxy when the proxy is near the client. With
the latency between client and proxy instead, a connection would cost 1 + 2 (or 3) round trips
more than the direct 4; this environment cannot produce that.

What overlaps is slower through this proxy. At the slow endpoint, one run each, wall time:

| Command | Direct | Through the proxy | Ratio |
|---|---|---|---|
| `find /tree/d0` (85 listings) | 1.609 s | 2.006 s | 1.25 |
| `find /tree` (342) | 4.619 | 5.615 | 1.22 |
| `find /fanout` (2,001) | 25.308 | 28.301 | 1.12 |
| `get -r /smallfiles/d01` (100 files) | 1.699 | 2.028 | 1.19 |
| `put -r d01` | 4.513 | 5.814 | 1.29 |
| `get /large/medium.bin` (50 MiB) | 4.250 | 6.650 | 1.56 |
| `put medium.bin` | 5.053 | 6.055 | 1.20 |
| `cd` | 0.201 | 0.201 | 1.00 |

The cause is the proxy, seen in a capture of its side towards `wan` during `find /tree/d0`: a
burst of requests leaves as full segments at once and its last, partial segment one round trip
later (1,332 and 1,448 bytes at 1.207 s, the remaining 360 at 1.407 s). That is Nagle's algorithm
on the proxy's socket to the server: microsocks does not set `TCP_NODELAY`. rsmb sets it on its
own socket; nothing a client does changes the proxy's. At the fast endpoint the same shows as the
40 ms of a delayed ACK:

| Command, fast | Direct | Port 1080 |
|---|---|---|
| `find /tree`; `find /fanout`; `find /chain` | 86; 472; 14 ms | 117; 476; 16 ms |
| `ls /huge`; `tree /huge` | 998; 1,100 | 999; 1,386 |
| `get -r /smallfiles/d01`; `put -r d01` | 94; 99 | 86; 396 |
| `get -r /smallfiles` (2,000 files); `put -r smallfiles` | 1,507; 1,769 | 1,486; 5,284 |
| `get /large/medium.bin`; `put medium.bin` | 40; 41 | 106; 130 |
| `get -r /tree`; `get -r /cjk` | 352; 830 | 461; 940 |
| `big.bin` (1.1 GiB) down; up | 1.8 s; 2.1 s in the gate | 1.35 s; 1.23 s |

In the gate, `big_file_through_the_proxy` moved `big.bin` through port 1080 in 1.41 s down and
1.69 s up with a peak RSS of 12.8 and 13.7 MB (2026-11-18, the run that added the test).

64 rsmb processes side by side, 32 per listener, each `use share`, `ls /many`, `find /tree`: all
64 printed their 10,682 entries, 864 ms for the lot; the proxy does not limit a parallel test run.

**How to repeat it by hand**: the command lines above, one session per route under the exclusive
lock, `--json`, each command timed from writing its line to reading its `done` object; the throwaway
scripts were not committed (`docs/development.md`, "Throwaway measurements").

**No name lookup on this host** (repeated by hand at the end of Stage 4, 2026-11-18): a capture
on every interface (`dumpcap -i any`, ports 53, 5353, 5355, 853) while rsmb ran `use share` and
`ls /sizes` through port 1080 by name saw 2 packets, none naming `rsmb.test` (background mDNS);
the control, direct to the same name, saw 18, 8 of them the A and AAAA queries for
`fileserver.rsmb.test` and their answers. A recorded measurement, not a test (a resolver cache or
encrypted DNS would blind it); the gate's proof is `no_way_around_the_proxy`.

## Other costs

| What | Cost |
|---|---|
| start-up (connect, NEGOTIATE, SESSION_SETUP × 2) | 3 requests in 3 flights, about 0.8 s at the slow endpoint; 1.2 s on wan-bad |
| start-up with `-k` (Kerberos) | 2 requests (NEGOTIATE, 1 SESSION_SETUP) in 2 flights: with the service ticket in the cache 3 round trips with the TCP connect (NTLM: 4); with a TGT only, plus a TCP connect to the KDC and one exchange, 5 round trips. Counted from the messages, not timed at the slow endpoint (the DC has no latency in front of it). At the fast endpoint, while planning: the Kerberos step 4.7–6.6 ms with a service ticket, 9.7–10.9 ms with a TGT (then through the loopback relay of `docs/plans/stage-5.md`; after the deviation V12 rsmb sends one TCP exchange to the KDC itself, the same round trips and one loopback datagram less, not re-timed; 12.9–13.4 ms with the crate's own UDP-then-TCP client) |
| `shares` | the list: 7 requests in 7 flights the first time on a connection (TREE_CONNECT `IPC$`, CREATE, 2 WRITE, 2 READ, CLOSE), 6 after; 306 shares on edge in one answer. The access column: a TREE_CONNECT and a TREE_DISCONNECT per share, up to 16 shares per compound, as many compounds in flight as the credits pay for. Table below |
| reconnect after a cut | 4 round trips (dial, NEGOTIATE, 2 SESSION_SETUP) plus 1 TREE_CONNECT on the next use of the share |
| first Tab in an uncached directory | one listing (5 requests, 1 flight); every further Tab there 0 |
| a listing on a server with 64 KiB buffers and 16 credits (edge) | 16 requests in 1 flight up to about 7,000 short names; `many/` 44 in 4, `huge/` 213 in 17 |

**`shares` with and without the access column** (requests / flights through `smbtap`; time
from sending the line to the `done` line, direct, median of 5; the same session runs it twice):

| Endpoint | Without, first | Without, again | With, first | With, again |
|---|---|---|---|---|
| fast (6 shares) | 7 / 7, 1.1 ms | 6 / 6, 0.7 ms | 19 / 8, 2.8 ms | 18 / 7, 2.3 ms |
| slow, 100 ms each way (6 shares) | 7 / 7, 1,403 ms | 6 / 6, 1,202 ms | 19 / 8, 1,606 ms | 18 / 7, 1,404 ms |
| edge (306 shares, 16 credits) | 13 / 13, 6.4 ms | 12 / 12, 5.2 ms | 625 / 52, 65 ms | 624 / 51, 57 ms |

On the normal server the column costs exactly one round trip (200 ms at the slow endpoint); on
edge 39 compounds of 8 shares one after another, 60 ms. **On a server with few credits and a
long link this is where the column gets slow**: 16 credits allow 8 shares per compound and one
compound at a time, so ceil(306 ÷ 8) = 39 compounds of one round trip each, about 7.8 s at 100 ms
each way (computed, not measured: no test server has both 16 credits and a slow link). Through the proxy (ports 1080 and 1081)
the counts are the same. "Without" is the same build with the access call removed.

**Memory.** A listing holds its reply frame (up to 16 MiB) and the parsed entries at once. `ls
huge/` (100,000 entries) peaked at 43.3 MB for the whole process (VmHWM, `ls_huge_slow`). The
cache keeps up to 500,000 units, one per entry plus one per listing (about 50 MB when directories
hold many entries, about 115 MB if every listing were empty).

## Method

`testenv/bench-rsmb.sh` uses the rows and the method of `testenv/bench-smbclientng.sh`
(`docs/measuring.md`): a shared lock for the whole run, netem drops checked before and after, the
connect time per row, 5 runs per row (`quick`: 1, no `huge/` and no wan-bad row), direct to the
endpoint for times and through `smbtap` for counts. A run counts only if rsmb exits 0 and printed
N × entries listing lines. The gate's own timing tests (`timing_cd`, `timing_ls_small`,
`timing_ls_large`) print the same rsmb numbers on every full run, with the baseline beside them.
The `transfers` and `walk` modes and the smbclient-ng rows in them are described with their
tables above and in `docs/measuring.md`.

## Not done

Deliberately not built, each with what it would cost:

- A compounded share list (the 7 requests of `shares` could be fewer); the list is kept after the
  first call.
- Per-directory access in `shares` or `ls`: a listing carries no rights, so it would cost an open
  per directory; the access column is per share only.
- No measurement against a real WAN or VPN; netem is a clean model (`docs/known-limitations.md`).
- No latency between client and proxy, and no proxy other than microsocks; no `proxy` bench mode
  (it would time microsocks, and smbclient-ng has no proxied column).
- smbclient-ng's 1.1 GiB transfers: at its 50 MiB rates about 6 minutes down and 24 up per run;
  the 50 MiB rows stand for them.
- **A `put` in two flights**: a compound of our own, `[CREATE, WRITE, FLUSH]` then `[SET_INFO
  rename, CLOSE]` on the same handle, would bring a small `put` from 3 round trips to 2 and would
  also close the window in which a second uploader of the same name can slip in (the limitation in
  `docs/known-limitations.md`). About 70 lines on the raw API; not built, it changes every count
  of a small `put` (S2-2), so it is the user's call.
- **A compound read for a single small `get`**: a single `get` always streams (3 round trips),
  because its size is not known without asking and a blind 256 KiB read would move 256 KiB of a
  large file for nothing.
- **A larger buffer for one very large directory inside a walk**: `huge/` costs 15 flights in a
  walk against 1 for `ls` (about 2 s more at the slow endpoint).
- **`-mindepth`** for `find`: one comparison in the filter; not built until someone asks.
- **Skipping what exists** in a recursive transfer: a failed recursive transfer is run again with
  `-f` and transfers everything again.

Done in the final run: the smbclient-ng baseline renewed on the current image (its columns had
been from 2026-11-17), smbclient-ng's transfers measured (50 MiB each way).
