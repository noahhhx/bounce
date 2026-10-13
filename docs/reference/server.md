# Server command line

```text
bounce-server [--listen <addr>] [--http <addr>] [--data-dir <path>] [--no-mdns] [--source <spec>]...
```

| Option | Default | Description |
|---|---|---|
| `--listen <addr>` | `0.0.0.0:7700` | The address speakers connect to. Port `0` picks a free port. |
| `--http <addr>` | `0.0.0.0:7701` | The address of the [HTTP API](http-api.md). Port `0` picks a free port. |
| `--data-dir <path>` | `/var/lib/bounce` | The directory for saved state. The server creates it if it does not exist. |
| `--no-mdns` | Off | Do not advertise the server to speakers over mDNS. Speakers then need `--server`. |
| `--source <spec>` | `spotify:Bounce` | A source. Repeat the option to add more. Source IDs follow the order of the options, from `0`. |

## Source specs

| Spec | Source |
|---|---|
| `spotify:<name>` | A Spotify Connect device named `<name>`, streaming at 320 kbit/s. |
| `test-tone` | A 440 Hz tone at 48 kHz with a click once per second. |

## Output

When both listeners are bound, the server prints one line to standard
output:

```text
bounce-server ready session=<addr> http=<addr>
```

With port `0`, this line is the only way to learn the chosen ports.

The server logs to standard error. Set `RUST_LOG` to change the level, for
example `RUST_LOG=debug`. The default is `info`.

## Files

| Path | Contents |
|---|---|
| `<data-dir>/fleet.json` | Speakers, zones, and speaker names. |
| `<data-dir>/spotify/` | Saved Spotify pairing credentials. |

## Exit status

The server exits with status 2 on an unknown option or a malformed value.
