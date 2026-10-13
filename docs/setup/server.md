# Run the server

The server is the Spotify Connect device you cast to. It sends audio to
every speaker and keeps the list of speakers and rooms.

## Build the server

Clone the repository and build the `bounce-server` binary:

```sh
git clone https://github.com/noahhhx/bounce.git
cd bounce
cargo build --release -p bounce-server
```

The binary is `target/release/bounce-server`.

## Start the server

The server stores its state in `/var/lib/bounce` by default. To run it as
a user that cannot write there, pass a different directory:

```sh
target/release/bounce-server --data-dir ~/.local/share/bounce
```

When both ports are open, the server prints one line:

```text
bounce-server ready session=0.0.0.0:7700 http=0.0.0.0:7701
```

The server now shows up as a Spotify Connect device named **Bounce**. To
use a different name, pass `--source spotify:<name>`.

## Open the ports

If the server machine runs a firewall, allow these ports from your local
network:

| Port | Protocol | Used by |
|---|---|---|
| 7700 | TCP | Speakers, for the audio stream and control messages |
| 7701 | TCP | The HTTP API |
| 5353 | UDP | mDNS, so Spotify and the speakers can find the server |

Spotify Connect also listens on a random TCP port for pairing. If your
firewall blocks it, the device does not appear in the Spotify app.

## Check the server

Ask the server for its state:

```sh
curl http://localhost:7701/api/state
```

The response lists the speakers, the rooms, and the sources. A new server
has no speakers and no rooms, and one Spotify source.

## Next step

[Add a speaker](speakers.md).

For every server option, see the [server command line](../reference/server.md).
