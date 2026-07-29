import type { JsonRecord } from '../../../shared/types';

/**
 * Fozmo plays Apple Music by capturing Music.app on the Mac running the core,
 * which means taking over that Mac's default output. There is one such stream
 * in existence, so the zone that started Apple Music holds it until playback
 * there ends, and a request from anywhere else is refused rather than silently
 * taking the music away from whoever is listening.
 */
export const APPLE_MUSIC_STREAM_IN_USE = 'apple_music_stream_in_use';

/** The chosen output cannot consume a live capture at all. */
export const APPLE_MUSIC_OUTPUT_UNSUPPORTED = 'apple_music_output_unsupported';

/**
 * This browser is running on the Mac Fozmo is capturing. A page plays through
 * the system default output, which is the device Apple Music capture takes
 * over, so the stream would be captured again as it played.
 */
export const APPLE_MUSIC_HOST_BROWSER_LOOP = 'apple_music_host_browser_loop';

/** Name of the zone currently holding the Apple Music stream, if any. */
export function appleMusicStreamZoneName(status: JsonRecord | null | undefined): string {
  const name = status?.apple_music_stream_zone_name;
  return typeof name === 'string' ? name.trim() : '';
}

/**
 * The sentence to show for an Apple Music request the server refused, or null
 * when the failure is an ordinary one that belongs in the transient notice.
 *
 * The server sends a stable code rather than prose so the explanation can name
 * the output actually holding the stream, which only the client knows how to
 * spell for this listener.
 */
export function appleMusicBlockedMessage(
  message: string,
  status?: JsonRecord | null
): string | null {
  const code = message.trim();
  if (code === APPLE_MUSIC_STREAM_IN_USE) {
    const zoneName = appleMusicStreamZoneName(status);
    const where = zoneName ? `on ${zoneName}` : 'on another output';
    return `Apple Music is already playing ${where}. Fozmo can play Apple Music through one output at a time, so stop it there before starting it here.`;
  }
  if (code === APPLE_MUSIC_OUTPUT_UNSUPPORTED) {
    return 'This output cannot play Apple Music. Apple Music streams live from the Mac running Fozmo, so it plays through a Fozmo output, an agent, or this browser — not a Sonos or UPnP renderer.';
  }
  if (code === APPLE_MUSIC_HOST_BROWSER_LOOP) {
    return 'This browser is running on the Mac Fozmo captures Apple Music from, so playing here would feed the capture its own audio. Use the Mac’s own Fozmo output, or open Fozmo in a browser on another device.';
  }
  return null;
}
