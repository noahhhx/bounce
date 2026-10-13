# HTTP API

The server answers HTTP on the `--http` address, port 7701 by default. The
API has no authentication. Anyone on your network can use it.

## GET /api/state

Returns the current state as JSON:

```json
{
	"clients": [
		{
			"id": "b0c00000000000000000000000000001",
			"hostname": "kitchen",
			"version": { "major": 0, "minor": 1, "patch": 0 },
			"online": true,
			"zone": 1,
			"desired": { "name": "kitchen", "volume": 50, "...": "..." },
			"reported": { "player": { "Playing": { "stream": 3 } }, "...": "..." }
		}
	],
	"zones": [
		{ "id": 1, "name": "Living room", "members": ["b0c00000000000000000000000000001"], "volume": 50, "source": 0 }
	],
	"sources": [
		{ "id": 0, "kind": "Spotify", "name": "Bounce", "volume": 100, "stream": 3 }
	]
}
```

| Field | Description |
|---|---|
| `clients[].online` | `true` while the speaker is connected. |
| `clients[].zone` | The speaker's zone ID, or `null`. |
| `clients[].desired` | What the server wants the speaker to do, including the speaker's `name` and its combined `volume`. |
| `clients[].reported` | What the speaker last reported, or `null`. `reported.player` is `"Idle"`, `{"Buffering":{"stream":<id>}}`, `{"Playing":{"stream":<id>}}`, or `{"OutputUnavailable":{"device":"<name>"}}`. |
| `zones[].source` | The source the zone plays, or `null` for silent. |
| `sources[].stream` | The current stream ID, or `null` while the source is idle. |

## POST /api/admin

Takes one request as JSON and returns `202 Accepted` when the server has
queued it. The change shows in the next `GET /api/state`, not in the
response. A malformed request returns `400 Bad Request`. A body larger than
64 KiB returns `413 Payload Too Large`.

| Request | Effect |
|---|---|
| `{"UpsertZone":{"zone":<id>,"name":"<name>"}}` | Creates the zone, or renames it if it exists. |
| `{"MoveClient":{"client":"<id>","zone":<id or null>}}` | Moves the speaker into the zone, or out of every zone with `null`. |
| `{"RouteZone":{"zone":<id>,"source":<id or null>}}` | Makes the zone play the source, or silences it with `null`. |
| `{"SetZoneVolume":{"zone":<id>,"volume":<0-100>}}` | Sets the zone's volume. |
| `{"SetSourceVolume":{"source":<id>,"volume":<0-100>}}` | Sets the source's volume. |
| `{"RenameClient":{"client":"<id>","name":"<name>"}}` | Renames the speaker. |

## GET /api/logs

```text
GET /api/logs?client=<id>&limit=<n>
```

Returns the speaker's newest `n` log records as a JSON array, oldest first.
`limit` defaults to 100. A malformed query returns `400 Bad Request`.

| Field | Description |
|---|---|
| `boot` | The speaker's boot ID, as 16 hex digits. It changes each time `bounce-client` starts. |
| `seq` | The record's sequence number within the boot. |
| `wall_us` | When the speaker logged the record, on the server's clock, in microseconds since the Unix epoch. |
| `level` | The log level, for example `"Info"`. |
| `target` | The module that logged the record. |
| `message` | The log message. |
| `fields` | Extra key and value pairs, as an array of `[key, value]` arrays. |
