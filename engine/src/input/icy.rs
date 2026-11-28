//! ICY (Shoutcast/Icecast) in-band metadata: strips metadata blocks from the audio
//! byte stream and reports `StreamTitle` changes.

use std::io::{self, Read};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamMeta {
    pub station: String,
    /// Full StreamTitle, usually "Artist - Title".
    pub title: String,
}

impl StreamMeta {
    pub fn artist_title(&self) -> (String, String) {
        match self.title.split_once(" - ") {
            Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
            None => (String::new(), self.title.trim().to_string()),
        }
    }

    /// Fill a RadioText template: {song}, {artist}, {title}, {station}.
    pub fn render(&self, template: &str) -> String {
        let (artist, title) = self.artist_title();
        template
            .replace("{song}", self.title.trim())
            .replace("{artist}", &artist)
            .replace("{title}", &title)
            .replace("{station}", &self.station)
            .trim()
            .to_string()
    }
}

pub fn parse_stream_title(block: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(block);
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    let end = rest.find("';").unwrap_or_else(|| rest.trim_end_matches('\0').trim_end_matches('\'').len());
    Some(rest[..end].to_string())
}

pub struct IcyReader<R: Read, F: FnMut(String)> {
    inner: R,
    metaint: usize,
    until_meta: usize,
    on_title: F,
}

impl<R: Read, F: FnMut(String)> IcyReader<R, F> {
    pub fn new(inner: R, metaint: usize, on_title: F) -> Self {
        Self { inner, metaint, until_meta: metaint, on_title }
    }
}

impl<R: Read, F: FnMut(String)> Read for IcyReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.metaint == 0 {
            return self.inner.read(buf);
        }
        if self.until_meta == 0 {
            let mut len = [0u8; 1];
            self.inner.read_exact(&mut len)?;
            let n = len[0] as usize * 16;
            if n > 0 {
                let mut block = vec![0u8; n];
                self.inner.read_exact(&mut block)?;
                if let Some(t) = parse_stream_title(&block) {
                    (self.on_title)(t);
                }
            }
            self.until_meta = self.metaint;
        }
        let want = buf.len().min(self.until_meta);
        let n = self.inner.read(&mut buf[..want])?;
        self.until_meta -= n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_metadata_and_reports_title() {
        let meta = b"StreamTitle='Artist - Song';";
        let pad = meta.len().div_ceil(16) * 16;
        let mut data = Vec::new();
        data.extend_from_slice(&[1u8; 8]);
        data.push((pad / 16) as u8);
        data.extend_from_slice(meta);
        data.resize(data.len() + pad - meta.len(), 0);
        data.extend_from_slice(&[2u8; 8]);
        data.push(0);
        let mut titles = vec![];
        let mut out = vec![];
        IcyReader::new(&data[..], 8, |t| titles.push(t)).read_to_end(&mut out).unwrap();
        assert_eq!(out, [[1u8; 8], [2u8; 8]].concat());
        assert_eq!(titles, vec!["Artist - Song".to_string()]);
        let m = StreamMeta { station: "X".into(), title: "Artist - Song".into() };
        assert_eq!(m.render("{title} by {artist}"), "Song by Artist");
    }
}
