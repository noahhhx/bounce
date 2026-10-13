# Volume

Bounce has two volume controls. Each zone has a volume, and each source has
a volume. A speaker plays at the source volume multiplied by the zone
volume. For example, a source at 80 and a zone at 50 play at 40.

Volumes go from 0 to 100. The server stores a higher value as 100.

## Set a zone's volume

```sh
curl -X POST http://localhost:7701/api/admin \
	-d '{"SetZoneVolume":{"zone":1,"volume":30}}'
```

Only the speakers in zone `1` change.

## Set a source's volume

```sh
curl -X POST http://localhost:7701/api/admin \
	-d '{"SetSourceVolume":{"source":0,"volume":80}}'
```

Every speaker that plays source `0` changes. For a Spotify source, the
volume slider in the Spotify app sets the same value.

A new source starts at 100. A new zone starts at 50.
