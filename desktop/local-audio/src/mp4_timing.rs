//! Recover simple MP4 encoder priming/padding that Symphonia 0.6.1's MP4/AAC
//! path does not apply. Mozilla's parser owns metadata interpretation; a small
//! box-shape guard rejects edit forms that its permissive reader would ignore.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

/// Opaque, validated container timing. Read this before a decoder/demuxer takes
/// the file or a cloned descriptor: clones share the underlying seek position.
pub struct ContainerTimeline {
    movie: Option<mp4parse::MediaContext>,
}

impl ContainerTimeline {
    /// Starts and finishes at offset zero, including on metadata rejection.
    /// The supplied name is only a format hint; no path is opened here.
    pub fn read(file: &mut File, name: &str) -> Result<Self, &'static str> {
        file.rewind().map_err(|_| "decode_failed")?;
        let result = read_movie(file, name);
        file.rewind().map_err(|_| "decode_failed")?;
        result.map(|movie| Self { movie })
    }

    pub fn for_track(
        &self,
        track_id: u32,
        sample_rate: u32,
    ) -> Result<DecodedFrameTimeline, &'static str> {
        if sample_rate == 0 {
            return Err("audio_format_unsupported");
        }
        let edit = self
            .movie
            .as_ref()
            .map(|movie| EditWindow::from_context(movie, track_id, sample_rate))
            .transpose()?
            .flatten();
        Ok(DecodedFrameTimeline {
            edit,
            sample_rate,
            decoded_frames: 0,
            effective_frames: 0,
            packet_trim_applied: false,
            maximum_decoded_frames: u64::from(sample_rate)
                .checked_mul(18_004)
                .ok_or("decode_limit")?,
        })
    }
}

/// Select frames from *untrimmed* decoder output. Decoder-side gapless trimming
/// must be disabled: AAC 0.6.1 ignores that option, whereas MP3 honors it.
/// Supported MP4 edits override packet trims; otherwise packet trims apply once.
pub struct DecodedFrameTimeline {
    edit: Option<EditWindow>,
    sample_rate: u32,
    decoded_frames: u64,
    effective_frames: u64,
    packet_trim_applied: bool,
    maximum_decoded_frames: u64,
}

impl DecodedFrameTimeline {
    pub fn select_packet(
        &mut self,
        frames: usize,
        trim_start: u64,
        trim_end: u64,
    ) -> Result<std::ops::Range<usize>, &'static str> {
        let decoded = self
            .decoded_frames
            .checked_add(u64::try_from(frames).map_err(|_| "decode_limit")?)
            .ok_or("decode_limit")?;
        if decoded > self.maximum_decoded_frames {
            return Err("decode_limit");
        }
        let selected = if let Some(edit) = &mut self.edit {
            edit.select(frames)?
        } else {
            let start = usize::try_from(trim_start).map_err(|_| "decode_failed")?;
            let end = frames
                .checked_sub(usize::try_from(trim_end).map_err(|_| "decode_failed")?)
                .ok_or("decode_failed")?;
            if start > end {
                return Err("decode_failed");
            }
            self.packet_trim_applied |= trim_start != 0 || trim_end != 0;
            start..end
        };
        self.effective_frames = self
            .effective_frames
            .checked_add(u64::try_from(selected.len()).map_err(|_| "decode_limit")?)
            .ok_or("decode_limit")?;
        self.decoded_frames = decoded;
        Ok(selected)
    }

    pub fn finish(self) -> Result<TimelineSummary, &'static str> {
        if let Some(edit) = &self.edit {
            edit.finish()?;
        }
        if self.effective_frames == 0 {
            return Err("duration_mismatch");
        }
        let duration = u128::from(self.effective_frames)
            .checked_mul(1000)
            .ok_or("decode_limit")?
            .div_ceil(u128::from(self.sample_rate));
        Ok(TimelineSummary {
            sample_rate: self.sample_rate,
            decoded_frames: self.decoded_frames,
            effective_frames: self.effective_frames,
            duration_ms_ceil: u64::try_from(duration).map_err(|_| "decode_limit")?,
            container_edit_applied: self.edit.is_some(),
            packet_trim_applied: self.packet_trim_applied,
        })
    }

    pub fn effective_frames(&self) -> u64 {
        self.effective_frames
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineSummary {
    sample_rate: u32,
    decoded_frames: u64,
    effective_frames: u64,
    duration_ms_ceil: u64,
    container_edit_applied: bool,
    packet_trim_applied: bool,
}

impl TimelineSummary {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn decoded_frames(&self) -> u64 {
        self.decoded_frames
    }
    pub fn effective_frames(&self) -> u64 {
        self.effective_frames
    }
    pub fn duration_ms_ceil(&self) -> u64 {
        self.duration_ms_ceil
    }
    pub fn container_edit_applied(&self) -> bool {
        self.container_edit_applied
    }
    pub fn packet_trim_applied(&self) -> bool {
        self.packet_trim_applied
    }
}

fn read_movie(file: &mut File, name: &str) -> Result<Option<mp4parse::MediaContext>, &'static str> {
    let mut head = [0_u8; 12];
    let count = file.read(&mut head).map_err(|_| "decode_failed")?;
    file.rewind().map_err(|_| "decode_failed")?;
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "m4a" | "mp4") && (count < 8 || &head[4..8] != b"ftyp") {
        return Ok(None);
    }
    let size = file.metadata().map_err(|_| "decode_failed")?.len();
    let mut boxes = 0;
    check_boxes(file, 0, size, 0, &mut boxes)?;
    file.rewind().map_err(|_| "decode_failed")?;
    let context = mp4parse::read_mp4(&mut file.take(512 * 1024 * 1024))
        .map_err(|_| "mp4_metadata_invalid")?;
    file.rewind().map_err(|_| "decode_failed")?;
    Ok(Some(context))
}

fn check_boxes(
    file: &mut File,
    mut cursor: u64,
    end: u64,
    depth: usize,
    boxes: &mut usize,
) -> Result<(), &'static str> {
    let mut seen_edts = false;
    let mut seen_elst = false;
    while cursor < end {
        *boxes += 1;
        if *boxes > 4096 || end - cursor < 8 {
            return Err("mp4_metadata_invalid");
        }
        file.seek(SeekFrom::Start(cursor))
            .map_err(|_| "decode_failed")?;
        let mut header = [0_u8; 8];
        file.read_exact(&mut header)
            .map_err(|_| "mp4_metadata_invalid")?;
        let short = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (size, width) = match short {
            0 => (end - cursor, 8),
            1 => {
                let mut long = [0_u8; 8];
                if end - cursor < 16 {
                    return Err("mp4_metadata_invalid");
                }
                file.read_exact(&mut long)
                    .map_err(|_| "mp4_metadata_invalid")?;
                (u64::from_be_bytes(long), 16)
            }
            n => (u64::from(n), 8),
        };
        if size < width || size > end - cursor {
            return Err("mp4_metadata_invalid");
        }
        let body = cursor + width;
        let next = cursor + size;
        // The metadata reader overwrites prior edits on duplicates. Reject
        // ambiguous sibling containers/lists before that information is lost.
        let seen = match &header[4..8] {
            b"edts" => Some(&mut seen_edts),
            b"elst" => Some(&mut seen_elst),
            _ => None,
        };
        if let Some(seen) = seen {
            if *seen {
                return Err("audio_edits_unsupported");
            }
            *seen = true;
        }
        if &header[4..8] == b"elst" {
            let length = usize::try_from(size - width).map_err(|_| "mp4_metadata_invalid")?;
            if !(8..=28).contains(&length) {
                return Err("audio_edits_unsupported");
            }
            let mut edit = vec![0_u8; length];
            file.read_exact(&mut edit)
                .map_err(|_| "mp4_metadata_invalid")?;
            if edit[1..4] != [0, 0, 0] {
                return Err("audio_edits_unsupported");
            }
            let count = u32::from_be_bytes(edit[4..8].try_into().unwrap());
            let valid = match (edit[0], count, length) {
                (0 | 1, 0, 8) => true,
                (0, 1, 20) => {
                    i32::from_be_bytes(edit[12..16].try_into().unwrap()) >= 0
                        && edit[16..20] == [0, 1, 0, 0]
                }
                (1, 1, 28) => {
                    i64::from_be_bytes(edit[16..24].try_into().unwrap()) >= 0
                        && edit[24..28] == [0, 1, 0, 0]
                }
                _ => false,
            };
            if !valid {
                return Err("audio_edits_unsupported");
            }
        } else if matches!(
            (depth, &header[4..8]),
            (0, b"moov") | (1, b"trak") | (2, b"edts")
        ) {
            check_boxes(file, body, next, depth + 1, boxes)?;
        }
        cursor = next;
    }
    Ok(())
}

struct EditWindow {
    start: usize,
    end: usize,
    consumed: usize,
    maximum: usize,
}

impl EditWindow {
    pub fn from_context(
        context: &mp4parse::MediaContext,
        track_id: u32,
        rate: u32,
    ) -> Result<Option<Self>, &'static str> {
        let mut matches = context
            .tracks
            .iter()
            .filter(|t| t.track_id == Some(track_id));
        let track = matches.next().ok_or("mp4_metadata_invalid")?;
        if matches.next().is_some() {
            return Err("mp4_metadata_invalid");
        }
        let Some(start) = track.media_time else {
            return Ok(None);
        };
        if track.looped == Some(true) || track.empty_duration.is_some_and(|n| n.0 != 0) {
            return Err("audio_edits_unsupported");
        }
        let scale = track.timescale.ok_or("mp4_metadata_invalid")?.0;
        let movie_scale = context.timescale.ok_or("mp4_metadata_invalid")?.0;
        let duration = track.edited_duration.ok_or("mp4_metadata_invalid")?.0;
        if scale == 0 || movie_scale == 0 || duration == 0 {
            return Err("audio_edits_unsupported");
        }
        let convert = |value: u64, scale: u64| {
            usize::try_from(u128::from(value) * u128::from(rate) / u128::from(scale))
                .map_err(|_| "decode_limit")
        };
        let start = convert(start.0, scale)?;
        let keep = convert(duration, movie_scale)?;
        // This path supports codec priming, not arbitrary movie timeline edits.
        let priming_limit = usize::try_from(u64::from(rate).checked_mul(2).ok_or("decode_limit")?)
            .map_err(|_| "decode_limit")?;
        let duration_limit =
            usize::try_from(u64::from(rate).checked_mul(18_000).ok_or("decode_limit")?)
                .map_err(|_| "decode_limit")?;
        if start > priming_limit || keep == 0 || keep >= duration_limit {
            return Err("audio_edits_unsupported");
        }
        let end = start.checked_add(keep).ok_or("decode_limit")?;
        Ok(Some(Self {
            start,
            end,
            consumed: 0,
            maximum: end.checked_add(priming_limit).ok_or("decode_limit")?,
        }))
    }

    pub fn select(&mut self, frames: usize) -> Result<std::ops::Range<usize>, &'static str> {
        let consumed = self.consumed.checked_add(frames).ok_or("decode_limit")?;
        if consumed > self.maximum {
            return Err("decode_limit");
        }
        let start = self.start.saturating_sub(self.consumed).min(frames);
        let end = self.end.saturating_sub(self.consumed).min(frames);
        self.consumed = consumed;
        Ok(start..end.max(start))
    }

    pub fn finish(&self) -> Result<(), &'static str> {
        if self.consumed < self.end {
            return Err("duration_mismatch");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn explicit_edit_keeps_fractional_aac_frames_and_ignores_packet_trims() {
        let mut timeline = ContainerTimeline { movie: None }
            .for_track(1, 16_000)
            .unwrap();
        timeline.edit = Some(EditWindow {
            start: 1024,
            end: 1024 + 16_401,
            consumed: 0,
            maximum: 1024 + 16_401 + 32_000,
        });
        let mut selected = 0;
        for packet in 0..18 {
            selected += timeline
                .select_packet(
                    1024,
                    if packet == 0 { 1024 } else { 0 },
                    if packet == 17 { 1007 } else { 0 },
                )
                .unwrap()
                .len();
        }
        let result = timeline.finish().unwrap();
        assert_eq!(selected, 16_401);
        assert_eq!(result.decoded_frames(), 18_432);
        assert_eq!(result.effective_frames(), 16_401);
        assert_eq!(result.duration_ms_ceil(), 1026);
        assert!(result.decoded_frames() * 1000 / 16_000 - result.duration_ms_ceil() > 100);
        assert!(result.container_edit_applied());
        assert!(!result.packet_trim_applied());
    }

    #[test]
    fn packet_trims_apply_once_when_no_container_edit_exists() {
        let mut timeline = ContainerTimeline { movie: None }
            .for_track(0, 16_000)
            .unwrap();
        assert_eq!(timeline.select_packet(1024, 100, 0).unwrap(), 100..1024);
        assert_eq!(timeline.select_packet(1024, 0, 50).unwrap(), 0..974);
        let summary = timeline.finish().unwrap();
        assert_eq!(summary.decoded_frames(), 2048);
        assert_eq!(summary.effective_frames(), 1898);
        assert_eq!(summary.duration_ms_ceil(), 119);
        assert!(!summary.container_edit_applied());
        assert!(summary.packet_trim_applied());
        let mut invalid = ContainerTimeline { movie: None }
            .for_track(0, 16_000)
            .unwrap();
        assert!(invalid.select_packet(100, 60, 41).is_err());
        assert!(ContainerTimeline { movie: None }.for_track(0, 0).is_err());
    }

    #[test]
    fn raw_padding_can_cross_five_hours_while_effective_timeline_remains_below_it() {
        // Counter-only regression: no five-hour signal is allocated or decoded.
        let keep = 16_000 * 18_000 - 16;
        let start = 1024;
        let end = start + keep;
        let mut timeline = ContainerTimeline { movie: None }
            .for_track(1, 16_000)
            .unwrap();
        timeline.edit = Some(EditWindow {
            start,
            end,
            consumed: 0,
            maximum: end + 32_000,
        });
        timeline.select_packet(end + 1000, 0, 0).unwrap();
        let summary = timeline.finish().unwrap();
        assert!(summary.decoded_frames() * 1000 / 16_000 >= 18_000_000);
        assert_eq!(summary.duration_ms_ceil(), 17_999_999);
        assert_eq!(summary.effective_frames(), keep as u64);
    }

    #[test]
    fn metadata_is_read_and_rewound_before_a_shared_descriptor_consumer() {
        let mut original = tempfile::tempfile().unwrap();
        original.write_all(b"RIFFxxxxWAVEadditional data").unwrap();
        let mut cloned = original.try_clone().unwrap();
        cloned.seek(SeekFrom::Start(5)).unwrap();
        let metadata = ContainerTimeline::read(&mut cloned, "source.wav").unwrap();
        assert_eq!(original.stream_position().unwrap(), 0);
        assert!(metadata.for_track(0, 1000).unwrap().finish().is_err());
        original.set_len(0).unwrap();
        original
            .write_all(&[0, 0, 0, 8, b'e', b'l', b's', b't'])
            .unwrap();
        assert!(ContainerTimeline::read(&mut original, "invalid.m4a").is_err());
        assert_eq!(original.stream_position().unwrap(), 0);
    }

    #[test]
    fn duplicate_edit_lists_or_containers_cannot_silently_replace_the_timeline() {
        let wrap = |kind: &[u8; 4], body: &[u8]| {
            let mut boxed = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            boxed.extend(kind);
            boxed.extend(body);
            boxed
        };
        let edit = wrap(
            b"elst",
            &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 7, 208, 0, 0, 4, 0, 0, 1, 0, 0],
        );
        let container = wrap(b"edts", &edit);
        for track in [
            wrap(b"trak", &wrap(b"edts", &[edit.clone(), edit].concat())),
            wrap(b"trak", &[container.clone(), container].concat()),
        ] {
            let movie = wrap(b"moov", &track);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&movie).unwrap();
            assert_eq!(
                check_boxes(&mut file, 0, movie.len() as u64, 0, &mut 0),
                Err("audio_edits_unsupported")
            );
        }
    }

    #[test]
    fn window_removes_priming_and_padding_without_filling_missing_input() {
        let mut window = EditWindow {
            start: 2,
            end: 7,
            consumed: 0,
            maximum: 10,
        };
        assert_eq!(window.select(4).unwrap(), 2..4);
        assert!(window.finish().is_err());
        assert_eq!(window.select(4).unwrap(), 0..3);
        window.finish().unwrap();
        assert!(window.select(3).is_err());
    }

    #[test]
    fn edit_shape_guard_rejects_complex_rates_and_bad_lengths() {
        let mut valid = vec![0, 0, 0, 28];
        valid.extend(b"elst");
        valid.extend([0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 7, 208, 0, 0, 4, 0, 0, 1, 0, 0]);
        for bad in [false, true] {
            let mut data = valid.clone();
            if bad {
                data[25] = 2;
            }
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&data).unwrap();
            let result = check_boxes(&mut file, 0, data.len() as u64, 3, &mut 0);
            assert_eq!(result.is_err(), bad);
        }
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&valid[..10]).unwrap();
        assert!(check_boxes(&mut file, 0, 10, 0, &mut 0).is_err());
    }
}
