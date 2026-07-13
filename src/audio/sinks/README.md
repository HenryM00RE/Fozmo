# Sinks

Remote and network playback sinks live here.

Current shape:

- `airplay/`: MIT-side facade for the standalone GPL helper. It consumes coarse
  receiver state and opaque IDs from helper IPC, converts audio to documented
  standard PCM, and sends coarse control and metadata. DNS-SD discovery,
  receiver network targets, pairing, encryption, RTSP/RTP, and codec handling
  remain inside `airplay-helper/`.
- `sonos.rs`: Sonos discovery/control/playback service, stream proxy assets,
  DIDL metadata, transport status, and volume.

The playback router chooses when a zone uses one of these sinks; sink modules
own the MIT-side transport boundary. The standalone helper owns AirPlay network
protocol behavior.
