---
name: verify
description: Run bounce-server and several bounce-clients together on this one machine (loopback, WAV files instead of speakers, a test tone instead of Spotify), drive them through the server's HTTP API, and capture proof. Use to prove any change to the server, client, protocol or common crate end to end without Pi hardware, or when asked to verify, run, or demo bounce.
---

# Verify bounce on one machine

Bounce is a server that broadcasts audio to clients wired to speakers. This
skill runs the real `bounce-server` and N real `bounce-client` processes on
this machine and checks what they do. Every process gets its own id, data
dir and ports. Clients write what they would play into WAV files. The
server plays a test tone, so no phone or Spotify account is needed.

All commands go through one script, run from the repo root:

```sh
B=.claude/skills/verify/bin/bounce-local
```

It needs `curl` and `jq`, which the devenv shell provides. Outside it, run
`devenv shell -- $B ...`.

## What the binaries must support

The harness depends on these flags and outputs. They are documented in
`server/src/config.rs`, `client/src/config.rs`, `server/src/http.rs` and
`client/src/player.rs`. If one changes, update the harness in the same
change.

- Server: `--listen 127.0.0.1:0 --http 127.0.0.1:0 --data-dir <dir> --no-mdns --source test-tone`.
  Once bound, it prints `bounce-server ready session=<addr> http=<addr>`
  to stdout.
- Client: `--client-id <32 hex> --server <addr> --data-dir <dir> --output wav:<dir>`.
- HTTP: `GET /api/state` returns `StateView` JSON. `POST /api/admin` takes
  one `AdminRequest` as serde's externally tagged JSON, such as
  `{"RouteZone":{"zone":1,"source":0}}`, and returns 202.
- WAV sink: `stream-<id>.wav` (16-bit PCM, 44-byte header) next to
  `stream-<id>.timing` (`<first frame> <play_at us>` per chunk).

**Current status (2026-10-13).** `$B up --clients 2` gives a healthy
doctor. Clients connect, converge on their desired state, stay online, and
reconnect by themselves. Every step of [join](features/join.md) passes
except the last part of a server restart: the server does not persist its
fleet yet, so a client that is off during the restart is not listed
afterwards. Routing a zone sends its clients `Playing` and subscribes them
to the source's audio, but the client has no player yet. It reports
`player=Idle` and writes no WAVs, so `$B peak` and `$B sync` have nothing
to measure, and the audio steps of [sync](features/sync.md) and
[volume](features/volume.md) cannot pass yet.

## Launch

```sh
$B up --clients 2        # builds, starts server + 2 clients, prints the run dir
```

The command builds both crates in debug, creates
`target/verify/<timestamp>-<pid>/`, and points `target/verify/current` at
it. It starts the server and waits up to 30 s for the ready line, then
starts the clients. Client `n` gets id `$B client-id n`
(`b0c0…000n`) and writes WAVs to `evidence/wav/client-n/`. `up` returning
means the server is ready, not that the clients are. Wait for them:

```sh
$B wait '[.clients[] | select(.online)] | length == 2'
```

To test prebuilt binaries, such as a release build, set
`BOUNCE_BIN_DIR=<dir>`. The harness then skips `cargo build`.

The server never advertises over mDNS (`--no-mdns`) and only listens on
127.0.0.1. Spotify is not started, so no Connect device appears on the LAN
and no real speaker in the house can join this server.

## Doctor

Run this first, and again whenever anything looks off:

```sh
$B doctor                # exit 0 and "verdict  healthy", or a named failure
```

It checks four things:

- Each process the run started is alive, matched by the run dir in its
  command line rather than by name.
- No `.rs` file is newer than the binaries. If one is, the build is stale,
  so rerun `up`.
- Both ports are owned by this run's server session.
- `/api/state` answers. It then lists each client's online status, zone,
  desired and applied revision, player state, and every source.

The output is also saved to `evidence/doctor.txt`. A `DOWN` process line
names its exit status. Its stderr is in `evidence/<name>.log`.

## Drive

Everything a user does happens through the web UI, which calls the same
`/api/admin` endpoint the harness uses. Drive only through `$B api` and
`$B wait`, which record every request, response and wait in
`evidence/api.log`.

```sh
$B api GET /api/state | jq .
$B api POST /api/admin '{"UpsertZone":{"zone":1,"name":"Living room"}}'
$B api POST /api/admin "{\"MoveClient\":{\"client\":\"$($B client-id 1)\",\"zone\":1}}"
$B wait '.clients[] | select(.id=="'"$($B client-id 1)"'") | .zone == 1'
```

`api` exits non-zero on any non-2xx response. `wait <jq expr> [secs]` polls
`/api/state` until the expression is true, 20 s by default. Use it after
every admin request, because the request returns before the hub applies
it. Never use a fixed `sleep` to wait for state.

Process controls stand in for physical events. `<name>` is `server` or
`client-<n>`.

```sh
$B stop client-2           # clean shutdown (SIGTERM)
$B stop client-2 --kill    # pulled power (SIGKILL)
$B freeze client-2         # Wi-Fi gone: socket stays open, nothing is sent (SIGSTOP)
$B thaw client-2
$B start client-2          # same arguments as before; a server keeps its ports
```

The doctor reports a stopped or frozen process as not healthy, which is
expected in the middle of these recipes.

Audio checks read the WAV files:

```sh
$B peak target/verify/current/evidence/wav/client-1/stream-<id>.wav 1   # peak sample, last 1 s
$B sync                                                                  # click spread across clients
```

`sync` finds the test tone's once-a-second click in every client's WAV and
converts each click to server-clock time using the `.timing` file. It
prints `second <s> clients=<n> spread_us=<µs>` and writes `clicks.txt` and
`sync.txt` to evidence.

Per-feature recipes are in [features/README.md](features/README.md).
Check that index before driving. A proof is incomplete if it skips an entry
point that the matching feature file lists.

## Evidence

Each run keeps its proof in `target/verify/<run>/evidence/`. `down` never
deletes it.

| File | What it proves |
|---|---|
| `api.log` | Every admin request and its status, every state read, and every `wait` with its result. This is the action trail. |
| `doctor.txt` | That the instance was healthy, built from current source, and owned by this run. |
| `server.log`, `client-n.log` | Process stdout and stderr, including panics. |
| `wav/client-n/stream-*.wav` and `.timing` | What each client would have played and when. |
| `clicks.txt`, `sync.txt` | The measured cross-client offset. |

Proof standards:

- Exercise the user path. Change state through `POST /api/admin` only.
  Never edit files under `state/` or call internal code to set state.
- Capture both the action and the result. The `api.log` entry for the
  request and the `wait` that saw the new state belong together.
- Check side effects as well as API state. A client that reports
  `Playing` but writes a silent WAV has not played anything. Read the WAV
  with `peak` or `sync`.
- Know what this setup cannot prove. The WAV sink replaces ALSA, and
  loopback has no Wi-Fi loss or jitter. A local pass proves protocol,
  scheduling, convergence and audio content. It does not prove DAC timing,
  ALSA device handling, or behavior on a lossy network. Say so when you
  report a result.

## Cleanup

```sh
$B down                  # stops this run's processes, deletes state/, keeps evidence/
$B down target/verify/<run>
```

`down` sends SIGTERM, then SIGKILL after 5 s, to each process group the
run started. It finds them through `pids/`, and a pid counts only if its
command line names the run dir. It never kills by process name. Run it
after every attempt, including failed ones. Old runs stay under
`target/verify/` until you delete them. `$B runs` lists them, newest
first.

## Isolation

Runs are fully separate. Ports are chosen by the OS, and each run has its
own data dirs and client ids. Two agents can therefore run `up` at the
same time. `target/verify/current` points at the latest `up`, so when
another agent may be running, pass the run explicitly. Set
`BOUNCE_RUN=<run dir>` for `api`, `wait`, `sync`, `stop`, `start`,
`freeze` and `thaw`, and pass the run dir as the argument to `doctor` and
`down`. Never drive or tear down a run you did not start.
