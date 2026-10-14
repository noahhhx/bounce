# Bounce verification map

This directory is the maintained source for verifying what bounce does for
a user. Read this index before driving, then use the matching feature file
as the recipe.

## Baseline preconditions

- Start a fresh run with `$B up --clients 2` from the repo root, where
  `B=.claude/skills/verify/bin/bounce-local`.
- Wait for both clients with
  `$B wait '[.clients[] | select(.online)] | length == 2'`.
- `$B doctor` exits 0 and prints `verdict  healthy`.
- The server has one source, the test tone, with id `0`. Confirm with
  `$B api GET /api/state | jq '.sources'`.
- No zones exist and both clients have `zone: null`.
- Never drive a run that this verification did not start.

## Driving conventions

- Make every change through `$B api POST /api/admin '<AdminRequest JSON>'`.
  This is the endpoint the web UI calls.
- Follow every change with `$B wait '<jq expr>'` on the resulting state.
  Never use a fixed sleep for state.
- Address clients by `$B client-id <n>`, never by a hostname. Every local
  client reports the same hostname.
- Enums in JSON are externally tagged. A unit variant is a string
  (`"Idle"`). A data variant is an object (`{"Playing":{"stream":3}}`).
- Run `$B down` when finished. It keeps `evidence/`.

## Proof and skip reporting

- Cite the `api.log` lines for the action and the `wait` that observed the
  result.
- Back audio claims with a WAV measurement (`$B peak`, `$B sync`), not
  only `reported.player`.
- Record which feature ID each artifact proves.
- If a step cannot run, report the command and the unmet precondition.
  Do not report a step as verified because a different step passed.
- State what a local run cannot cover (a Pi's DAC, separate sound cards, a lossy network)
  whenever it matters to the claim.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph on the
user-visible behavior. It then has exactly four H2 sections, in this order.

1. `Sub-features` lists short IDs, one line per behavior.
2. `How to get to it (user POV)` lists every user entry point.
3. `Driving it with bounce-local` starts with `Preconditions:`, then pairs
   each user action with an exact command and an observable result.
4. `Gotchas` lists traps that can waste or invalidate a run.

## Features

- [Join and liveness](./join.md) covers a speaker appearing, converging on its desired state, going offline, and coming back.
- [Zones and routing](./zones.md) covers creating zones, moving speakers between them, and choosing what each zone plays.
- [Volume](./volume.md) covers zone volume, source volume, and how they reach each speaker's output.
- [Multi-room sync](./sync.md) covers speakers in zones on the same source playing the same audio at the same moment.
- [Client logs](./logs.md) covers a speaker's log records reaching the server, surviving a server outage, and never arriving twice.
- [Sound card output](./sound-card.md) covers a speaker playing through ALSA, staying aligned with another sound-card speaker, and recovering from a busy device.
