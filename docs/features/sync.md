# Multi-room sync

Every speaker that plays the same source plays the same sample at the same
moment. This holds when the speakers are in different zones, when a speaker
joins in the middle of a song, and when a speaker reconnects. You can walk
from one room to the next and hear no echo.

You do not need to set anything up. To play two rooms in sync, route both
zones to the same source. See [Zones](zones.md).

## How it works

The server stamps each piece of audio with the time to play it, about one
second ahead. Each speaker keeps track of how far its own clock is from the
server's clock, and plays each piece at the stamped time. The one-second
delay gives every speaker time to receive the audio before it plays.
Because of this delay, a pause or a skip in the Spotify app takes about one
second to reach the speakers.
