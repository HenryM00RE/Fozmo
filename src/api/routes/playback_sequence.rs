use crate::playback::sequencer::{
    PLAYBACK_CLIENT_HEADER, PLAYBACK_SEQUENCE_HEADER, PlaybackRequestSequence,
};
use axum::http::HeaderMap;

pub(super) fn playback_request_sequence_from_headers(
    headers: &HeaderMap,
) -> Option<PlaybackRequestSequence> {
    let client = headers.get(PLAYBACK_CLIENT_HEADER)?.to_str().ok()?.trim();
    if client.is_empty() {
        return None;
    }
    let sequence = headers
        .get(PLAYBACK_SEQUENCE_HEADER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    Some(PlaybackRequestSequence::new(client, sequence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parses_playback_request_sequence_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            PLAYBACK_CLIENT_HEADER,
            HeaderValue::from_static(" client-a "),
        );
        headers.insert(PLAYBACK_SEQUENCE_HEADER, HeaderValue::from_static("42"));

        let sequence = playback_request_sequence_from_headers(&headers).unwrap();

        assert_eq!(sequence.client_id, "client-a");
        assert_eq!(sequence.sequence, 42);
    }

    #[test]
    fn ignores_missing_or_invalid_playback_request_sequence_headers() {
        assert!(playback_request_sequence_from_headers(&HeaderMap::new()).is_none());

        let mut headers = HeaderMap::new();
        headers.insert(PLAYBACK_CLIENT_HEADER, HeaderValue::from_static("client-a"));
        headers.insert(
            PLAYBACK_SEQUENCE_HEADER,
            HeaderValue::from_static("not-a-number"),
        );

        assert!(playback_request_sequence_from_headers(&headers).is_none());
    }
}
