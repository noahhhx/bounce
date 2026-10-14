# Sound card output

A speaker started with `--output alsa` plays through the device's sound
card. It advertises the card's ALSA playback devices, plays the one the
server picks, keeps in step with other speakers although its DAC runs on
its own crystal, and comes back by itself when a busy device frees up.

## Sub-features

- `card-play` plays the stream through ALSA with no dropouts.
- `card-sync` keeps two sound-card speakers aligned, including after a stall.
- `card-busy` reports a busy device unavailable and recovers when it frees.

## How to get to it (user POV)

- Start a speaker with no `--output` flag, or with `--output alsa`, and
  route its zone.

## Driving it with bounce-local

Preconditions:

- A Linux host with PipeWire, so `pw-record` can capture the default
  sink's monitor. The monitor is captured before the sink's volume, so a
  muted sink keeps the run silent.
- `B=.claude/skills/verify/bin/bounce-local`. Run inside `devenv shell`,
  which provides libasound for the build.

Steps:

- **Play and sync.** Run `.claude/skills/verify/bin/alsa-pair 20`. It
  starts its own run with two ALSA clients and tears it down. Both lines in
  `players.json` show `Playing`. `offsets.txt` has one line per second, and
  from about 4 s on every `offset_us` is under 1000.
- **Stall.** Run `.claude/skills/verify/bin/alsa-pair 30 15 300`, which
  freezes client 2 for 300 ms at 15 s. Within 5 s of the freeze the
  offsets are back under 1000.
- **Busy device.** Write `pcm.!default { type hw card <card> device 0 }`
  and `ctl.!default { type hw card <card> }` to a file, with `<card>` from
  `aplay -l`. Hold the card with another player, such as
  `aplay -D hw:CARD=<card>,DEV=0 -f S16_LE -r 48000 -c 2 /dev/zero`.
  Start one client with `ALSA_CONFIG_PATH=<file>` and `--output alsa`,
  then route its zone. `reported.player` is
  `{"OutputUnavailable":{"device":"default"}}`. Stop the other player.
  Within 5 s the client logs `the output is back` and reports `Playing`.
- **Proof.** Keep `evidence/alsa-pair/` (`rec.wav`, `offsets.txt`,
  `players.json`, the client logs) and `api.log`.

## Gotchas

- ALSA's `null` and `file` plugins are not real time. They take 2 s of
  audio in 5 ms, so they cannot prove pacing.
- PipeWire keeps a card open for a few seconds after its last stream ends.
  Wait before you try to hold the card for the busy test.
- `ALSA_CONFIG_PATH` replaces the whole configuration. If the file
  includes the system `alsa.conf`, PipeWire's `conf.d` entry overrides
  `default` again.
- The two clients share one DAC, so `alsa-pair` proves they agree with
  each other, not that they follow the server clock. Two speakers on
  separate cards (roadmap item 4) are the real test.
- Starts measured 0.7 to 6.4 ms apart and took about 3.5 s to slew
  together. Read the offsets after that.
