# AGENTS.md

This file orients AI agents (and humans) to working in the `bounce` repository.
Read this before doing anything else.

## What this project is

Bounce is a self-hosted, Sonos-like whole-home audio system, built as a Rust
workspace. The `server` crate acts as a Spotify Connect target (via
librespot) and broadcasts the audio stream to lightweight clients (e.g. on
Pi Zeros attached to speakers). The `web` crate is a Leptos UI that controls
which clients/zones play and at what volume. The `common` crate holds the
protocol and domain types shared across the workspace.
