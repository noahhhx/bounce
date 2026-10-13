# Client logs

A speaker's own log lines show up on the server, so someone debugging it
does not need a console on the device. The speaker buffers its records in
memory and sends them to the server, which stores them and acknowledges
them. Records logged while the server is unreachable arrive after the
speaker reconnects. Records the server already acknowledged are not sent
again.

## Sub-features

- `logs-forward` shows a client's own records, such as `starting`, at `GET /api/logs`.
- `logs-outage` delivers records a client logged while the server was down once it reconnects.
- `logs-no-resend` keeps records acked before a server restart out of the restarted server's store.
- `logs-query` returns the newest `limit` records, newest last, and rejects a malformed query with 400.

## How to get to it (user POV)

- Open a speaker's logs (`GET /api/logs?client=<32 hex>&limit=<n>`).
- Restart the server while speakers keep running.

## Driving it with bounce-local

Preconditions:

- The baseline from the README holds: two clients online, no zones.
- `B=.claude/skills/verify/bin/bounce-local`, `C1=$($B client-id 1)`.
- `$B wait` polls only `/api/state`. For logs, poll `$B api GET "/api/logs?client=$C1"` in a loop with a deadline.

- **Forward.** Poll until the records arrive, then run
  `$B api GET "/api/logs?client=$C1" | jq -e 'any(.[]; .message == "starting")'`.
  Client 1's `starting`, `connected` and `handshake accepted` records are
  listed with `seq` 1, 2, 3, one hex `boot`, and a `wall_us` within a
  millisecond of the same line's timestamp in `evidence/client-1.log`.
  Note the highest `seq` as `H` and the `boot`.
- **Outage.** Run `$B stop server`. Poll `evidence/client-1.log` until it
  has two new `backing off` lines. Run `$B start server`, then
  `$B wait '.clients[] | select(.id == "'$C1'") | .online' 60`, then poll
  `/api/logs` until `handshake accepted` appears again.
- **No resend.** On the restarted server, `/api/logs?client=$C1` holds
  only records with `seq > H`, starting at `H + 1`, with no duplicate
  `seq`, the same `boot`, and the `backing off` records from the outage.
  Check with
  `jq -e --argjson h $H 'all(.[]; .seq > $h) and (map(.seq) | min == $h + 1) and (map(.seq) | unique | length) == length'`.
- **Query.** `$B api GET "/api/logs?client=$C1&limit=1"` returns one
  record, the newest. `$B api GET '/api/logs?client=nope'` fails with 400
  and the body `client must be 32 lowercase hex`.
- **Proof.** Run `$B doctor`, then `$B down`. The evidence is the
  `api.log` lines for each `/api/logs` read and the `backing off` lines in
  `client-1.log`.

## Gotchas

- The server keeps logs in memory, so a restart empties the store. That
  is what makes `logs-no-resend` checkable: any record with `seq <= H`
  after the restart was sent twice.
- A client sends a batch at most once a second, on its tick. Poll for
  records; a read right after an action can be one tick early.
- The hub sets every client's log level to `Info`, so `debug` lines such
  as time sync warnings never reach the server.
- `boot` is a hex string, not a number, because JSON numbers lose the low
  bits of a random 64-bit id. The client's own `starting` line prints it
  in decimal.
- `fields` is a list of `[name, value]` pairs, in the order the client
  logged them.
