# Spotify Connect

The server shows up in the Spotify app as a Connect device, in the same
list as your phone and other speakers. When you pick it, Spotify streams to
the server, and the server sends the audio to every room routed to it.

## Cast from the Spotify app

1. Open the Spotify app on a device on the same network as the server.
2. Open the device picker.
3. Pick **Bounce**, or the name you gave with `--source spotify:<name>`.

Spotify Connect needs a Spotify Premium account. The first time you pick
the device, the app pairs with the server. The server saves the pairing in
`<data-dir>/spotify`, so a restart does not need the app to pair again.

Picking the device does not choose which rooms play. A room plays Spotify
only after you route its zone to the Spotify source. See
[Zones](zones.md).

## Add more than one Spotify device

Pass `--source` once for each device. Each one is a separate source with
its own name in the Spotify app:

```sh
bounce-server --source spotify:Downstairs --source spotify:Upstairs
```

Sources get IDs in the order of the flags, starting at `0`. In this example,
`Downstairs` is source `0` and `Upstairs` is source `1`.

## Use the test tone

The test tone source plays a 440 Hz tone with a click once per second. Use
it to check speakers and sync without a phone or a Spotify account:

```sh
bounce-server --source test-tone
```
