# Zones and routing

In the web UI, a user groups speakers into zones (rooms) and picks what
each zone plays. A speaker is in at most one zone. Speakers in a zone
routed to a source play it. A speaker with no zone, or in an unrouted
zone, stays silent.

## Sub-features

- `zone-upsert` creates a zone, or renames it when the id exists.
- `zone-move` puts a speaker in a zone, moves it to another zone, or removes it from all zones.
- `zone-route` makes every speaker in a zone play a source.
- `zone-unroute` silences a zone.

## How to get to it (user POV)

- Create or rename a room in the web UI (`UpsertZone`).
- Drag a speaker into a room or out of all rooms (`MoveClient`).
- Pick what a room plays, or nothing (`RouteZone`).

## Driving it with bounce-local

Preconditions:

- The baseline from the README holds: two clients online, no zones,
  source `0` is the test tone.
- `B=.claude/skills/verify/bin/bounce-local`, `C1=$($B client-id 1)`, `C2=$($B client-id 2)`.

- **Create zone.** Run
  `$B api POST /api/admin '{"UpsertZone":{"zone":1,"name":"Living room"}}'`,
  then `$B wait '.zones | map(select(.id == 1 and .name == "Living room")) | length == 1'`.
- **Upsert is idempotent.** Send the same request again, then run
  `$B wait '.zones | length == 1'`. There is still one zone.
- **Rename.** Run
  `$B api POST /api/admin '{"UpsertZone":{"zone":1,"name":"Lounge"}}'`,
  then `$B wait '.zones[] | select(.id == 1) | .name == "Lounge"'`.
- **Move in.** Run
  `$B api POST /api/admin '{"MoveClient":{"client":"'$C1'","zone":1}}'`,
  then `$B wait '(.clients[] | select(.id == "'$C1'") | .zone == 1) and (.zones[] | select(.id == 1) | .members == ["'$C1'"])'`.
- **Route.** Run
  `$B api POST /api/admin '{"RouteZone":{"zone":1,"source":0}}'`, then
  `$B wait '.clients[] | select(.id == "'$C1'") | .reported.player | type == "object" and has("Playing")'`.
  After 3 s, run
  `$B peak "$(ls -t target/verify/current/evidence/wav/client-1/stream-*.wav | head -1)" 1`.
  It prints a non-zero peak. Client 2 is in no zone, so it stays `"Idle"`
  and `evidence/wav/client-2/` has no WAV.
- **Move between zones.** Create zone 2 with `UpsertZone`, then move
  client 1 there with `MoveClient` and `"zone":2`. Run
  `$B wait '(.zones[] | select(.id == 1) | .members == []) and (.clients[] | select(.id == "'$C1'") | .zone == 2 and .reported.player == "Idle")'`.
  It left zone 1, and zone 2 is unrouted, so it goes silent.
- **Move out.** Run `MoveClient` with `"zone":null`, then wait for
  `.zone == null` on client 1.
- **Unroute.** Move client 1 back to zone 1 and wait for `Playing`. Run
  `$B api POST /api/admin '{"RouteZone":{"zone":1,"source":null}}'`, then
  wait for `.reported.player == "Idle"`. Check the WAV size with
  `stat -c %s` twice, 2 s apart. It no longer grows.
- **Proof.** Keep `api.log`, the `peak` output, and the WAV listing for
  both clients. Then run `$B down`.

## Gotchas

- `POST /api/admin` returns 202 before the hub applies the request.
  Always wait on `/api/state` afterwards.
- `Playing` in `reported.player` alone is not proof of sound. Check the
  WAV peak too.
- `Buffering` appears briefly before `Playing`. Waits must accept the end
  state, not the first change.
- The WAV file name contains the stream id. A new stream (after
  unrouting and rerouting) writes a new file. Glob `stream-*.wav` and pick
  the newest.
- Zone ids are chosen by the caller. Reusing an id renames that zone
  instead of creating a new one.
