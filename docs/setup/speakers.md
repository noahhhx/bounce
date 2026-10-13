# Add a speaker

Each speaker runs `bounce-client`. The client finds the server, joins it,
and plays what the server sends.

!!! warning "No sound card output yet"
    The client cannot play through a sound card yet. With the default
    `--output alsa`, the speaker joins the server and reports
    `OutputUnavailable`. To check that a speaker receives audio, write it
    to WAV files with `--output wav:<dir>`.

## Build the client

On the speaker device, or on a machine that builds for it, run:

```sh
cargo build --release -p bounce-client
```

The binary is `target/release/bounce-client`.

## Start the client

The client needs a unique ID. By default it reads `/etc/machine-id`, which
most Linux systems already have. The client also stores its state in
`/var/lib/bounce` by default.

```sh
target/release/bounce-client --data-dir ~/.local/share/bounce-client --output wav:/tmp/bounce-wav
```

The client looks for the server with mDNS. If mDNS does not work on your
network, pass the server's session address:

```sh
target/release/bounce-client --server 192.168.1.10:7700
```

## Check that the speaker joined

Ask the server for its state:

```sh
curl http://<server>:7701/api/state
```

The new speaker is in `clients`, with `"online": true` and `"zone": null`.
A speaker with no zone stays silent. To make it play, put it in a zone and
route the zone to a source. See [Zones](../features/zones.md).

If a speaker loses power or network, the server marks it offline. When it
comes back, it rejoins without help. The server remembers its speakers
across restarts.

For every client option, see the
[speaker command line](../reference/client.md).
