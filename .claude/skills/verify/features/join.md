# Join and liveness

A speaker that powers on finds the server, shows up in the web UI, and
converges on what the server wants it to do. When it loses power or
network, the UI shows it offline. When it comes back, it rejoins without
anyone touching it. A server restart does not make offline speakers
vanish.

## Sub-features

- `join-first` lets a new client appear online, unassigned, converged and idle.
- `join-offline-clean` marks a client offline when its connection closes.
- `join-offline-silent` marks a client offline when it stops talking but its socket stays open.
- `join-return` brings a stopped or silent client back online and converged.
- `join-server-restart` reconnects running clients to a restarted server and keeps offline clients listed.

## How to get to it (user POV)

- Plug in a speaker (start a client).
- Pull a speaker's power (kill a client).
- Lose Wi-Fi on a speaker (freeze a client).
- Restart the server.
- Watch the client list in the web UI (`GET /api/state`).

## Driving it with bounce-local

Preconditions:

- The baseline from the README holds: two clients online, no zones.
- `B=.claude/skills/verify/bin/bounce-local`, `C1=$($B client-id 1)`, `C2=$($B client-id 2)`.

- **First join.** Both clients start with the run. Run
  `$B wait '[.clients[] | select(.online and .zone == null and .reported.applied == .desired.revision and .reported.player == "Idle")] | length == 2'`.
  Both clients are online, in no zone, converged, and idle.
- **Clean disconnect.** Shut client 2 down. Run `$B stop client-2`, then
  `$B wait '.clients[] | select(.id == "'$C2'") | .online == false' 10`.
  Client 2 is still listed, now offline. Client 1 stays online.
- **Return.** Start it again. Run `$B start client-2`, then
  `$B wait '.clients[] | select(.id == "'$C2'") | .online and .reported.applied == .desired.revision'`.
- **Pulled power.** Run `$B stop client-2 --kill`, then the same offline
  wait as for a clean disconnect. Then run `$B start client-2` and the same
  return wait.
- **Silent client.** Freeze client 2 so its socket stays open but nothing
  arrives. Run `$B freeze client-2`, then
  `$B wait '.clients[] | select(.id == "'$C2'") | .online == false' 60`.
  The heartbeat timeout, not the socket, takes it offline. Run
  `$B thaw client-2` and the return wait.
- **Server restart with an offline client.** Run `$B stop client-2`,
  `$B stop server`, `$B start server`. Then run
  `$B wait '(.clients | length == 2) and (.clients[] | select(.id == "'$C2'") | .online == false)'`
  and `$B wait '.clients[] | select(.id == "'$C1'") | .online' 60`.
  Client 2 is listed as offline from the persisted fleet. Client 1
  reconnects by itself.
- **Proof.** Run `$B doctor` after the last return, then `$B down`. The
  evidence is the `api.log` wait lines for each transition, the
  `client-2.log` reconnect lines, and `doctor.txt`.

## Gotchas

- `stop` and `freeze` make the doctor report NOT HEALTHY. That is expected
  until `start` or `thaw`.
- The silent-client wait depends on `Thresholds::heartbeat_timeout`.
  Raise the 60 s timeout if that threshold is longer.
- A thawed client may find the server already closed its socket. Either a
  resumed session or a reconnect passes, as long as it ends online and
  converged.
- Clients reconnect with backoff and jitter. Wait on state, never a fixed
  delay.
- Every local client reports the same hostname. Identify clients by id
  only.
- A second client with an already connected id should get
  `DuplicateClientId`. The harness cannot start one yet, so this map does
  not cover it.
