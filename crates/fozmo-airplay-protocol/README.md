# Fozmo AirPlay helper protocol

This MIT-licensed crate defines the complete interface between the MIT Fozmo
server and a separately built AirPlay helper. It deliberately contains no
AirPlay implementation and no Fozmo database, settings, playback, or UI types.

Control messages are one-line JSON documents sent over an owner-only Unix
domain socket. Each message carries `version: 1` and a caller-generated
`request_id`. Receiver records contain only an opaque ID, display name, coarse
service kind/support state; AirPlay addresses, ports, and TXT
records and connection targets remain private to the helper. Audio uses a second Unix socket: the client first writes one
`stream_attach` JSON line and then arbitrary chunks of stereo, 44.1 kHz,
signed 16-bit little-endian PCM. Commands refer only to receiver IDs returned
by `list_receivers`; callers cannot supply network hosts or ports.
