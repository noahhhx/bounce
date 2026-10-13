# Multi-room sync

Every speaker playing the same source plays the same sample at the same
moment, whether the speakers share a zone or not. This holds for a speaker
that joins mid-stream and for one that reconnects. Without it, two rooms
heard together sound like an echo.

## Sub-features

- `sync-same-zone` keeps two speakers in one zone aligned.
- `sync-cross-zone` keeps speakers in different zones on the same source aligned.
- `sync-late-join` aligns a speaker that starts playing after the stream began.
- `sync-reconnect` realigns a speaker after it drops and rejoins mid-stream.

## How to get to it (user POV)

- Route one or more rooms to the same source and walk between them.

## Driving it with bounce-local

Preconditions:

- The baseline from the README holds.
- `B=.claude/skills/verify/bin/bounce-local`, `C1=$($B client-id 1)`, `C2=$($B client-id 2)`.

- **Same zone.** Create zone 1, move both clients into it, and route it to
  source `0` (see [zones](./zones.md)). Run
  `$B wait '[.clients[] | select(.reported.player | type == "object" and has("Playing"))] | length == 2'`.
  Wait 6 s, then run `$B sync`. Every `clients=2` line has `spread_us`
  under 1000.
- **Cross zone.** Start a fresh run. Put client 1 in zone 1 and client 2
  in zone 2, route both zones to source `0`, wait for both to play, wait
  6 s, and run `$B sync`. Same bar.
- **Late join.** Start a fresh run. Route zone 1 with only client 1 in it,
  wait 3 s, then move client 2 into zone 1. Once both play, wait 6 s and
  run `$B sync`. The seconds where both clients have a click meet the same
  bar. Client 2's first click is within those seconds, because late
  joiners get the pending chunks.
- **Reconnect.** With both playing, run `$B stop client-2 --kill` and
  `$B start client-2`. Wait for client 2 to report `Playing` again, wait
  6 s, and run `$B sync`. The seconds after it returned meet the bar. The
  restart may start a new WAV for client 2, and `sync` reads all of them.
- **Proof.** Keep `sync.txt`, `clicks.txt`, the WAV and `.timing` files,
  and `api.log`. Then run `$B down`.

## Gotchas

- The 1000 µs bar is a starting point for loopback, not a product
  requirement. Replace it once the project sets one.
- On one machine every process shares a clock and loopback has no jitter.
  A pass here is the best case. It proves chunk scheduling and the
  `play_at` arithmetic, not sync over Wi-Fi or at a real DAC.
- Lines with `clients=1` are seconds that only one client played, such as
  before a late join. They are not failures.
- `sync` needs the `.timing` sidecar next to each WAV. Without it a click
  has no server time and is skipped.
- `sync` detects clicks at half of each file's loudest sample. Keep
  volume constant and above 0 during a sync run, or quieter clicks are
  missed.
