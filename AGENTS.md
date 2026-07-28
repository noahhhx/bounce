# AGENTS.md

This file orients AI agents (and humans) to working in the `bounce` repository.
Read this before doing anything else.

## What this project is

Bounce is a self-hosted, Sonos-like whole-home audio system. A Spring Boot
server acts as a Spotify Connect target (via librespot) and broadcasts the
audio stream to lightweight GraalVM-native Java clients (e.g. on Pi Zeros
attached to speakers). An Astro web UI controls which clients/zones play.
