use crate::models::MetaAudio;

pub fn parse(data: &[u8], _ext: &str) -> MetaAudio {
    use lofty::prelude::*;
    use lofty::probe::Probe;

    let mut meta = MetaAudio::default();
    let cursor = std::io::Cursor::new(data);

    // guess_file_type() and read() have different error types,
    // so they cannot be chained with and_then directly.
    let probe = match Probe::new(cursor)
        .options(lofty::config::ParseOptions::new().read_properties(true))
        .guess_file_type()
    {
        Ok(p) => p,
        Err(_) => return meta,
    };

    let tagged_file: lofty::file::TaggedFile = match probe.read() {
        Ok(f) => f,
        Err(_) => return meta,
    };

    // ── Technical properties ──────────────────────────────────────────────
    let props = tagged_file.properties();
    meta.duration_secs = Some(props.duration().as_secs_f64());
    meta.bitrate_kbps = props.audio_bitrate();
    meta.sample_rate_hz = props.sample_rate();
    meta.channels = props.channels();

    // ── Tags (ID3v2, Vorbis Comments, APE, MP4 atoms…) ───────────────────
    if let Some(tag) = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())
    {
        meta.title = tag.title().map(|s: std::borrow::Cow<str>| s.into_owned());
        meta.artist = tag.artist().map(|s: std::borrow::Cow<str>| s.into_owned());
        meta.album = tag.album().map(|s: std::borrow::Cow<str>| s.into_owned());
        meta.genre = tag.genre().map(|s: std::borrow::Cow<str>| s.into_owned());
        meta.year = tag.year();
        meta.track_number = tag.track();
    }

    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_empty_doesnt_panic() {
        let meta = parse(b"", "mp3");
        assert!(meta.title.is_none());
    }
}
