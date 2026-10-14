# Roadmap

Outstanding work, grouped by area. Each item is stubbed, missing, or not
yet proven on real hardware.

## Playing audio on real speakers

1. **Sound card output.** Done. `--output alsa` plays through ALSA,
   corrects DAC drift by repeating or dropping single frames, and retries a
   missing or busy device every 2 s. It is proven through PipeWire on a
   laptop, not yet on a Pi, and a real unplug is only covered by a fake.
2. **Output latency per speaker.** The server always sends
   `output_latency: Duration::ZERO` (`server/src/hub.rs:526`). No admin
   request can set it, and nothing measures it. Without it, rooms with
   different sound cards play out of sync.
3. **Spotify tested end to end.** `server/src/source/spotify.rs` is written
   but has never been run against a real account. The verify harness only
   uses the test tone.
4. **Sync measured on hardware.** Measure two Pis on Wi-Fi and tune the
   playout delay to match. Through PipeWire, two ALSA clients on one card
   started 0.7 to 6.4 ms apart and took about 3.5 s to slew together.
   After that they stayed within 0.3 ms, including after a 300 ms stall.
   Check whether a Pi's driver places the first chunk better, and tune
   `client/src/player/drift.rs` if not.

## Deployment

5. **Cross-compiled builds** for the Pi Zero W (armv6) and the Zero 2 W
   (aarch64).
6. **systemd units** for the server and the client. The updater assumes
   systemd runs the `current` symlink.
7. **Pi setup guide.** `docs/setup/raspberry-pi.md` is a placeholder.
8. **Prebuilt packages.** The docs say you have to build from source.

## Control UI

9. **Web UI.** `web/src/main.rs` is `fn main() {}`. It should list speakers
   and zones, create zones, move speakers between them, choose what each
   zone plays, and set volume. Health alerts, logs and updates could go
   here too.
10. **Admin API gaps.** You can't delete a zone, remove a speaker the
    server no longer sees, or set output latency (see item 2).

## Speaker commands

11. **The client ignores every command.** `client/src/lib.rs:99` drops all
    `ServerMessage::Command`s, so these do nothing:
    - `Identify`, which should play a chime so you can find the speaker
    - `RestartProcess` and `RebootDevice`
    - `CollectDiagnostics` (`MetricsCollector::diagnostics` is also a
      `todo!()`)

## Health and metrics

12. **Client metrics.** `MetricsCollector::sample` in
    `client/src/telemetry.rs` is a `todo!()`.
13. **Server metrics storage.** `append_metrics` and `metrics` in
    `server/src/telemetry.rs` are `todo!()`s, and the telemetry task has no
    request type for metrics.
14. **Health rules.** All 12 rules in `server/src/health.rs` are
    `todo!()`s: offline, overheating, throttled, undervoltage, weak Wi-Fi,
    underruns, unstable clock, output unavailable, recent crash, failed
    update, rolled-back update, and not applying config. `Hub::reevaluate`
    is also a stub, and nothing calls it yet.
15. **Alerts in the API**, so the web UI can show them.

## Speaker updates

16. **Server side.** `ReleaseCatalog::publish` in `server/src/updates.rs` is
    a `todo!()`, and the `PublishRelease` admin request only logs a warning.
17. **Client side.** All four methods of the updater in
    `client/src/updater.rs` are `todo!()`s (startup check, fetch, switch,
    confirm healthy and roll back). None of them are wired into `main`.
18. **Release workflow.** Building an artifact, hashing it and publishing
    it to the server.
19. **Docs.** `docs/features/updates.md` is a placeholder.

## Bandwidth

20. **FLAC and Opus encoding.** `server/src/broadcast.rs:137` is a
    `todo!()`, and the client has no decoders. Uncompressed audio is fine
    for a few speakers. Compression starts to matter with more Pis on
    2.4 GHz Wi-Fi.

## Cleanup

21. **Lint suppressions.** Both `server/src/lib.rs` and `client/src/lib.rs`
    turn off the unused-variable and dead-code lints with
    `#![expect(…, reason = "sketch: bodies are todo!()")]`. Remove them once
    the stubs are filled in, or the lints will keep hiding real dead code.
22. **Leftover agent worktrees.** There are six under `.claude/worktrees/`.
23. **Verify skill coverage.** Each new feature needs its own recipe under
    `.claude/skills/verify/features/`.

## Speaker platforms

The speaker hardware isn't decided yet. Linux boards (Pi or others) and
the ESP32-S3 with an I2S amp are both candidates, and supporting both is
an option.

24. **Split the client into a portable core and platform shells.** The
    core keeps the code that never touches the OS: `Device`, `ClockSync`,
    the player's scheduling, and the log buffer. Each shell supplies the
    rest:
    - the transport and server discovery (`TcpStream` and `mdns-sd` in
      `client/src/transport.rs`)
    - the audio output. The `Output` and `Sink` traits in
      `client/src/player/sink.rs` are the seam: WAV and ALSA implement
      them, and I2S would too.
    - the client ID and state storage (`/etc/machine-id` and `--data-dir`
      in `client/src/config.rs`, or the MAC address and NVS)
    - system metrics (`/proc`, `/sys` and `vcgencmd` in
      `client/src/telemetry.rs`)
    - the updater (systemd and a `current` symlink in
      `client/src/updater.rs`, or ESP-IDF's A/B OTA partitions with
      rollback)

    The Linux shell stays the one the verify skill runs, whatever ships on
    speakers.
25. **ESP32 trial.** Build an esp-idf shell for an ESP32-S3 devkit and a
    MAX98357A that joins the server, syncs its clock and plays the test
    tone over I2S. Record it next to a WAV client and measure the offset
    over several minutes. Still to check: the forked Xtensa toolchain
    (`espup`), `mdns-sd` on lwIP (ESP-IDF's mDNS is the fallback), and Wi-Fi
    setup over BLE or SoftAP. The result decides whether 5–7 still matter
    or need ESP32 versions.

## Suggested order

- **25** before 5–7, since it decides which platforms those cover.
- **2–4** turn it into a working speaker system.
- **5–7** make it installable on Pis.
- **9** is what you'd use day to day.
- **11–15** make problems visible once speakers are mounted on walls.
- **16–19** make it maintainable without SSH.
- **20** can wait until bandwidth becomes a problem.
