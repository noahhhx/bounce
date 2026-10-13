# Speaker logs

Each speaker sends its own log records to the server. To debug a speaker,
read its logs from the server. You do not need a console on the device.

## Read a speaker's logs

```sh
curl 'http://localhost:7701/api/logs?client=<speaker id>&limit=50'
```

The response is the speaker's newest 50 records, oldest first. Without
`limit`, the server returns 100 records. For the fields of each record, see
the [HTTP API reference](../reference/http-api.md#get-apilogs).

## What the server keeps

- Records that a speaker logs while the server is unreachable arrive after
  the speaker reconnects.
- A record never arrives twice, even after a server restart.
- The server keeps the newest 10,000 records for each speaker, in memory.
  A server restart clears them.
