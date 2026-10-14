# Speaker command line

```text
bounce-client [--client-id <id>] [--server <addr>] [--data-dir <path>] [--output <spec>]
```

| Option | Default | Description |
|---|---|---|
| `--client-id <id>` | The contents of `/etc/machine-id` | The speaker's ID, as 32 lowercase hex digits. Each speaker needs a unique ID. |
| `--server <addr>` | Find the server with mDNS | The server's session address, as `<ip>:<port>`, for example `192.168.1.10:7700`. Host names are not accepted. |
| `--data-dir <path>` | `/var/lib/bounce` | The directory for saved state. |
| `--output <spec>` | `alsa` | Where the speaker plays audio. |

## Output specs

| Spec | Output |
|---|---|
| `alsa` | The device's sound card, through ALSA. The speaker offers the server its playback devices, and the server picks the first one, `default`. If the device is missing or busy, the speaker reports `OutputUnavailable` and tries again every 2 seconds. Linux only. |
| `wav:<dir>` | WAV files in `<dir>`, one per stream, named `stream-<id>.wav`. Each WAV file has a `stream-<id>.timing` file next to it with the time each chunk would play. |
