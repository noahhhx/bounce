# Volume

Each zone has a volume, and each source has a master volume, which is the
volume slider in the casting app. A speaker's output volume combines the
two. A change to either reaches every affected speaker without a restart.

## Sub-features

- `vol-zone` changes one zone's speakers and leaves other zones alone.
- `vol-source` changes every speaker playing the source.
- `vol-clamp` stores a requested volume above 100 as 100.
- `vol-zero` makes the output silent.

## How to get to it (user POV)

- Move a room's volume slider in the web UI (`SetZoneVolume`).
- Move the source's volume slider in the web UI (`SetSourceVolume`), or in
  the casting app for Spotify. The test tone has no app.

## Driving it with bounce-local

Preconditions:

- Zones 1 and 2 exist. Client 1 is in zone 1 and client 2 in zone 2.
  Both zones are routed to source `0`, and both clients report
  `Playing`. Set this up with [zones](./zones.md).
- `B=.claude/skills/verify/bin/bounce-local`, `C1=$($B client-id 1)`, `C2=$($B client-id 2)`.
- `W1=$(ls -t target/verify/current/evidence/wav/client-1/stream-*.wav | head -1)`, and likewise `W2` for client 2.

- **Baseline level.** Run `$B peak "$W1" 1` and `$B peak "$W2" 1`. Record
  both values, and `.desired.volume` for each client from
  `$B api GET /api/state`.
- **Zone volume.** Run
  `$B api POST /api/admin '{"SetZoneVolume":{"zone":1,"volume":30}}'`,
  then `$B wait '.clients[] | select(.id == "'$C1'") | .reported.applied == .desired.revision and .desired.volume < 100'`.
  Wait 2 s, then run `$B peak "$W1" 1`. It is lower than the baseline.
  `$B peak "$W2" 1` is unchanged, and client 2's `.desired.volume` is too.
- **Source volume.** Run
  `$B api POST /api/admin '{"SetSourceVolume":{"source":0,"volume":50}}'`,
  then `$B wait '.sources[] | select(.id == 0) | .volume == 50'`. Both
  clients' `.desired.volume` drop, and both WAV peaks drop after 2 s.
- **Clamp.** Run
  `$B api POST /api/admin '{"SetZoneVolume":{"zone":2,"volume":150}}'`.
  It returns 202. Then run `$B wait '.zones[] | select(.id == 2) | .volume == 100'`.
- **Zero.** Set zone 1 to `0`, wait for client 1 to converge, wait 2 s,
  and run `$B peak "$W1" 1`. It prints `0`.
- **Proof.** Keep `api.log` and each `peak` output with the volume that
  produced it. Then run `$B down`.

## Gotchas

- `peak` measures the last N seconds of the file. Wait at least that long
  after convergence, or the window mixes the old and new levels.
- The test tone's click is the loudest sample, so `peak` tracks the click
  level. Each 1 s window contains one click.
- The exact curve that combines zone and source volume belongs to
  `Hub::desired_state`. Assert direction, zero, and clamping, not exact
  sample values.
- `DesiredState.muted` exists, but no `AdminRequest` sets it, so mute has
  no user path to verify yet.
