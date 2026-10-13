# Set up Bounce

A Bounce system is one server and any number of speakers on the same
local network. Set up the server first, then add the speakers.

1. [Run the server](server.md) on a machine that stays on.
2. [Add a speaker](speakers.md) for each room.
3. [Build a Raspberry Pi speaker](raspberry-pi.md) to put a speaker in a
   room. This guide is not available yet.

## Requirements

- A Linux machine for the server, on the same network as the speakers.
- A Linux device for each speaker.
- A Rust toolchain to build Bounce from source. Bounce has no prebuilt
  packages yet.
- A Spotify Premium account to cast from the Spotify app.
- A network that passes multicast DNS (mDNS) traffic. Spotify uses mDNS to
  find the server, and speakers use it to find the server too. Without
  mDNS, give each speaker the server's address with `--server`.
