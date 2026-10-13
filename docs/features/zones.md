# Zones

A zone is a room. It has a name, a volume, a list of speakers, and the
source it plays. A speaker is in at most one zone. A speaker plays only
when its zone is routed to a source. A speaker with no zone, or in a zone
with no source, stays silent.

Zone IDs are numbers that you choose. Speaker IDs are the 32 hex digits the
server lists in `clients[].id` from `GET /api/state`.

Each request below returns `202 Accepted` when the server has queued it.
The change shows in the next `GET /api/state`.

## Create or rename a zone

```sh
curl -X POST http://localhost:7701/api/admin \
	-d '{"UpsertZone":{"zone":1,"name":"Living room"}}'
```

If zone `1` exists, this request renames it. A new zone starts at volume
50 and with no source.

## Move a speaker into a zone

```sh
curl -X POST http://localhost:7701/api/admin \
	-d '{"MoveClient":{"client":"<speaker id>","zone":1}}'
```

Moving a speaker into a zone takes it out of its previous zone. To take a
speaker out of every zone, send `"zone":null`.

## Pick what a zone plays

```sh
curl -X POST http://localhost:7701/api/admin \
	-d '{"RouteZone":{"zone":1,"source":0}}'
```

Every speaker in the zone plays source `0`. To silence the zone, send
`"source":null`. Source IDs are in `sources[].id` from `GET /api/state`.

## Rename a speaker

```sh
curl -X POST http://localhost:7701/api/admin \
	-d '{"RenameClient":{"client":"<speaker id>","name":"Kitchen left"}}'
```

The server keeps zones, their speakers, and speaker names in
`<data-dir>/fleet.json`. They survive a server restart.
