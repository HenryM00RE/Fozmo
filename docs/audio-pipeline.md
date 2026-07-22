# Audio Pipeline

Playback follows five stages:

1. Fozmo resolves the song from your library or streaming service.
2. It decodes the source into PCM audio.
3. It applies volume, EQ and upsampling if you turned them on.
4. It prepares the result as PCM, DSD or a network-friendly stream.
5. It sends the audio to the chosen output.

## Outputs

Local DACs can receive PCM or DSD when their driver supports it. AirPlay,
Sonos and other network outputs receive a format they understand. A paired
remote agent does the final playback work on its own machine.

Fozmo checks what an output can handle before playback. When possible, it uses
a compatible format instead of forcing a mode the device does not support.

## DSP and DSD

EQ changes the tone. Upsampling changes the rate or format. PCM-to-DSD
conversion happens near the end of the pipeline. None of these steps adds
missing musical detail, and all of them are optional.

The playback details in the app show the source format, output format and any
active processing.
