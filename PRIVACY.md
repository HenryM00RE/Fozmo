# Privacy and network behavior

_Last updated: 13 July 2026_  
_Applies to Fozmo 0.0.1 public pre-alpha._

Fozmo is a self-hosted music player. It does not require a Fozmo account and does not use a project-operated cloud service for your library, playback history, settings, or playlists.

Fozmo does not include project-operated analytics, telemetry, advertising, or automatic crash-report uploads. Crash information and application logs remain on the computer running Fozmo unless you choose to share them.

Fozmo does make direct connections to third-party services and devices when the relevant functionality is used, as described below.

## Data stored on your device

Fozmo stores its persistent application data locally. This can include:

- local-library metadata and file paths;
- playlists, favorites, queues, profiles, zones, and listening history;
- edited metadata and downloaded artwork;
- user-created EQ presets and an uploaded display font;
- Qobuz catalog, artwork, stream, and transcode caches;
- application settings, logs, and local backups;
- browser-pairing and Remote Access session information.

On packaged macOS installations, persistent data is normally stored under:

```text
~/Library/Application Support/Fozmo
~/Library/Caches/Fozmo
~/Library/Logs/Fozmo
```

Sensitive material, including Qobuz session information, Last.fm API keys, pairing secrets, and generated Remote Access private keys, is stored using the macOS Keychain rather than in the ordinary settings file.

Replacing or deleting `Fozmo.app` does not automatically delete this data.

## Internet connections

### Qobuz

Fozmo contacts Qobuz to:

- initialize the unofficial Qobuz integration;
- connect and authenticate a Qobuz account;
- refresh the Qobuz home page and account-related catalog metadata;
- search and browse albums, artists, playlists, and tracks;
- obtain playback URLs and stream audio;
- retrieve Qobuz artwork and artist images.

In the current 0.0.1 build, Fozmo attempts an initial Qobuz home-cache refresh when the server starts and refreshes that cache approximately once per hour while the server remains running. This may contact Qobuz even before you open a Qobuz page.

Requests can contain Qobuz authentication or session information, search terms, catalog identifiers, playback requests, and the normal network information associated with an HTTPS connection. Qobuz responses and temporary playback data may be cached locally.

This application uses the Qobuz API but is not certified by Qobuz. Fozmo is not affiliated with, endorsed by, sponsored by, or certified by Qobuz.

Use of Qobuz through Fozmo remains subject to the [Qobuz Terms of Service](https://www.qobuz.com/us-en/legal/terms).

### MusicBrainz

Fozmo contacts MusicBrainz when you run manual or automatic metadata matching. It may send search queries derived from local album titles, artist names, track titles, track counts, and existing MusicBrainz identifiers.

Fozmo also uses MusicBrainz artist searches while resolving popular tracks for some Qobuz artist pages.

Audio-file contents are not uploaded to MusicBrainz.

### ListenBrainz

Fozmo may contact ListenBrainz while loading popular tracks for a Qobuz artist. It sends MusicBrainz artist or recording identifiers and uses public, site-wide popularity and radio data.

Fozmo does not send your Fozmo listening history or a ListenBrainz user account token for this process.

### Last.fm

Last.fm integration is inactive until you provide a Last.fm API key and enable or test Last.fm Radio.

When active, Fozmo sends the configured API key together with a seed track title and artist, or a MusicBrainz track identifier, to request similar tracks.

Fozmo may use your local library and locally stored playback history to rank, match, or exclude the returned recommendations. That local history is not included in the Last.fm request.

### Cover Art Archive

When MusicBrainz metadata is applied and cover replacement is enabled, Fozmo may send the selected MusicBrainz release identifier to the Cover Art Archive and download its front cover.

### Apple iTunes Search and artwork CDN

Fozmo may use Apple’s public iTunes Search API to improve album artwork. A lookup can contain:

- an album barcode or UPC; or
- an artist name, album title, and track count.

Matching artwork is downloaded from Apple’s `mzstatic.com` artwork CDN and stored locally.

This happens after some metadata-matching operations or when you explicitly run a bulk artwork refresh.

### Google Fonts

The current browser interface imports DM Sans and JetBrains Mono from Google Fonts. When the interface loads and those fonts are not already cached, your browser may connect directly to Google to retrieve the stylesheet and font files.

Google receives the normal connection information associated with that browser request. Fozmo does not proxy this request through a Fozmo-operated service.

## Local-network connections

Fozmo listens only on the local computer by default.

When you explicitly enable LAN access, Fozmo can advertise itself using Bonjour/mDNS and accept connections from browsers on the same network. The advertisement can expose the computer’s local hostname, Fozmo version, server port, pairing mode, and local access URL to devices on that network.

When LAN authentication is disabled, other devices on the trusted local network can browse and control Fozmo. Enable LAN access only on a network you trust.

AirPlay, Sonos, UPnP, Hegel, and remote-agent functionality can use local-network discovery and direct connections to compatible devices. Depending on the integration, Fozmo may send audio, playback metadata, device-control commands, volume changes, and stream URLs to the selected device.

## Remote Access

Remote Access is disabled by default.

When enabled, it creates a direct TLS connection between your Fozmo server and linked browsers through the network configuration and port forwarding you control. Fozmo does not route Remote Access traffic through a relay operated by the Fozmo project.

Enabling Remote Access can expose the configured port to the public internet. Read [the Remote Access documentation](docs/remote-access.md) before enabling it.

## Data sent to the Fozmo project

Fozmo does not automatically send the project maintainer:

- your library database;
- music files;
- Qobuz account information;
- listening history;
- playlists or queues;
- device names or network addresses;
- logs or crash information.

Information is sent to the project only when you deliberately submit it, such as in a GitHub issue or private security report. Remove account details, file paths, network addresses, tokens, and other private information before sharing diagnostics.

## Clearing local data

Individual service connections and caches can be cleared from Fozmo’s settings where supported.

To remove all packaged macOS data:

1. Quit Fozmo.
2. Delete `~/Library/Application Support/Fozmo`.
3. Delete `~/Library/Caches/Fozmo`.
4. Delete `~/Library/Logs/Fozmo`.
5. Remove the Fozmo `com.fozmo.secrets` entry from Keychain Access if you also want to remove stored secrets.

Deleting this data permanently removes local settings, history, playlists, managed uploads, artwork, presets, and backups. Back up anything you want to keep first.

Third-party services may retain information according to their own terms and privacy policies. Deleting Fozmo’s local data does not delete data held by Qobuz, Last.fm, Apple, Google, MetaBrainz, or another external service.

## Changes and security reports

This document should be updated whenever Fozmo adds a new external service, automatic network request, telemetry mechanism, remote-access path, or materially different data-retention behavior.

Report suspected security vulnerabilities using the private process described in [SECURITY.md](SECURITY.md).
