//! A deliberately small packet-copy boundary. No decoder, encoder or filtergraph is opened.
use std::{
    ffi::{CString, c_char, c_int, c_void},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Vp8,
    Vp9,
    Av1,
    Aac,
    Mp3,
    Opus,
    Vorbis,
    Ac3,
    Eac3,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Webm,
    Matroska,
    MpegTs,
    Mp3,
    Ogg,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct Probe {
    pub video: Option<Codec>,
    pub audio: Option<Codec>,
    pub container: Container,
    pub duration: Option<f64>,
    pub seekable: bool,
    pub width: u32,
    pub height: u32,
    pub audio_stream: Option<usize>,
    pub default_audio_stream: Option<usize>,
    pub audio_stream_count: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    InvalidInput,
    Native(i32),
    OutputLimit,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Media preparation failed ({self:?})")
    }
}
impl std::error::Error for Error {}
#[repr(C)]
#[derive(Default)]
struct NativeProbe {
    video: c_int,
    audio: c_int,
    container: c_int,
    duration: f64,
    seekable: c_int,
    width: c_int,
    height: c_int,
    audio_stream: c_int,
    default_audio_stream: c_int,
    audio_stream_count: c_int,
}
unsafe extern "C" {
    fn nova_media_probe(
        path: *const c_char,
        cancel: extern "C" fn(*mut c_void) -> c_int,
        opaque: *mut c_void,
        probe: *mut NativeProbe,
        read: Option<ReadFn>,
        seek: Option<SeekFn>,
        io: *mut c_void,
        selected_audio_index: c_int,
    ) -> c_int;
    fn nova_media_remux(
        source: *const c_char,
        destination: *const c_char,
        cancel: extern "C" fn(*mut c_void) -> c_int,
        opaque: *mut c_void,
        limit: i64,
        read: Option<ReadFn>,
        seek: Option<SeekFn>,
        io: *mut c_void,
        selected_audio_index: c_int,
    ) -> c_int;
}
extern "C" fn cancelled(ptr: *mut c_void) -> c_int {
    i32::from(unsafe { &*ptr.cast::<AtomicBool>() }.load(Ordering::Relaxed))
}
fn path(path: &Path) -> Result<CString, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::InvalidInput)
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_str().ok_or(Error::InvalidInput)?).map_err(|_| Error::InvalidInput)
    }
}
fn checked(code: i32, cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else if code == -28 {
        Err(Error::OutputLimit)
    } else if code < 0 {
        Err(Error::Native(code))
    } else {
        Ok(())
    }
}
fn codec(id: i32) -> Option<Codec> {
    Some(match id {
        0 => return None,
        1 => Codec::H264,
        2 => Codec::Hevc,
        3 => Codec::Vp8,
        4 => Codec::Vp9,
        5 => Codec::Av1,
        10 => Codec::Aac,
        11 => Codec::Mp3,
        12 => Codec::Opus,
        13 => Codec::Vorbis,
        14 => Codec::Ac3,
        15 => Codec::Eac3,
        _ => Codec::Unknown,
    })
}
pub fn probe(source: &Path, cancel: &AtomicBool) -> Result<Probe, Error> {
    let source = path(source)?;
    let mut p = NativeProbe::default();
    checked(
        unsafe {
            nova_media_probe(
                source.as_ptr(),
                cancelled,
                (cancel as *const AtomicBool).cast_mut().cast(),
                &mut p,
                None,
                None,
                std::ptr::null_mut(),
                -1,
            )
        },
        cancel,
    )?;
    Ok(convert(p))
}
fn convert(p: NativeProbe) -> Probe {
    Probe {
        video: codec(p.video),
        audio: codec(p.audio),
        container: match p.container {
            1 => Container::Mp4,
            2 => Container::Webm,
            3 => Container::Matroska,
            4 => Container::MpegTs,
            5 => Container::Mp3,
            6 => Container::Ogg,
            _ => Container::Unknown,
        },
        duration: if p.duration.is_finite() && p.duration >= 0.0 {
            Some(p.duration)
        } else {
            None
        },
        seekable: p.seekable != 0,
        width: p.width.max(0) as u32,
        height: p.height.max(0) as u32,
        audio_stream: usize::try_from(p.audio_stream).ok(),
        default_audio_stream: usize::try_from(p.default_audio_stream).ok(),
        audio_stream_count: p.audio_stream_count.max(0) as usize,
    }
}
/// Available bytes on the filesystem containing an existing directory.
pub fn available_space(directory: &Path) -> Result<u64, Error> {
    unsafe extern "C" {
        fn nova_media_available_space(path: *const c_char) -> i64;
    }
    let directory = path(directory)?;
    let result = unsafe { nova_media_available_space(directory.as_ptr()) };
    u64::try_from(result).map_err(|_| Error::InvalidInput)
}
/// Writes a completed seekable MP4. The caller owns its private destination directory.
/// Any failure removes the partial output before returning.
pub fn packet_copy_mp4(
    source: &Path,
    destination: &Path,
    max_bytes: u64,
    cancel: &AtomicBool,
) -> Result<Probe, Error> {
    if source == destination || destination.exists() {
        return Err(Error::InvalidInput);
    }
    let src = path(source)?;
    let dst = path(destination)?;
    let result = checked(
        unsafe {
            nova_media_remux(
                src.as_ptr(),
                dst.as_ptr(),
                cancelled,
                (cancel as *const AtomicBool).cast_mut().cast(),
                max_bytes.min(i64::MAX as u64) as i64,
                None,
                None,
                std::ptr::null_mut(),
                -1,
            )
        },
        cancel,
    )
    .and_then(|()| probe(destination, cancel));
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

type ReadFn = extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;
type SeekFn = extern "C" fn(*mut c_void, i64, c_int) -> i64;
fn selection(value: Option<usize>) -> Result<i32, Error> {
    value.map_or(Ok(-1), |n| {
        i32::try_from(n).map_err(|_| Error::InvalidInput)
    })
}
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
struct Reader<'a> {
    inner: &'a mut dyn ReadSeek,
    cancel: &'a AtomicBool,
    started: Option<std::time::Instant>,
}
extern "C" fn read_callback(opaque: *mut c_void, buffer: *mut u8, len: c_int) -> c_int {
    // FFmpeg provides a writable buffer of exactly len bytes. No panic may cross FFI.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = unsafe { &mut *opaque.cast::<Reader<'_>>() };
        if reader.cancel.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if reader
            .started
            .is_some_and(|started| started.elapsed() > std::time::Duration::from_secs(300))
        {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        reader
            .inner
            .read(unsafe { std::slice::from_raw_parts_mut(buffer, len as usize) })
    }));
    match result {
        Ok(Ok(0)) => -541478725,
        Ok(Ok(n)) => n as c_int,
        _ => -5,
    }
}
extern "C" fn seek_callback(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = unsafe { &mut *opaque.cast::<Reader<'_>>() };
        if reader.cancel.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if reader
            .started
            .is_some_and(|started| started.elapsed() > std::time::Duration::from_secs(300))
        {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        if whence & 0x10000 != 0 {
            let current = reader.inner.stream_position()?;
            let size = reader.inner.seek(SeekFrom::End(0))?;
            reader.inner.seek(SeekFrom::Start(current))?;
            return Ok(size);
        }
        reader.inner.seek(match whence & 3 {
            0 if offset >= 0 => SeekFrom::Start(offset as u64),
            1 => SeekFrom::Current(offset),
            2 => SeekFrom::End(offset),
            _ => return Err(std::io::ErrorKind::InvalidInput.into()),
        })
    }));
    match result {
        Ok(Ok(n)) => i64::try_from(n).unwrap_or(-5),
        _ => -5,
    }
}
pub fn probe_reader(reader: &mut dyn ReadSeek, cancel: &AtomicBool) -> Result<Probe, Error> {
    probe_reader_selected(reader, cancel, None)
}
pub fn probe_reader_selected(
    reader: &mut dyn ReadSeek,
    cancel: &AtomicBool,
    selected_audio: Option<usize>,
) -> Result<Probe, Error> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::InvalidInput)?;
    let mut input = Reader {
        inner: reader,
        cancel,
        started: Some(std::time::Instant::now()),
    };
    let mut p = NativeProbe::default();
    checked(
        unsafe {
            nova_media_probe(
                std::ptr::null(),
                cancelled,
                (cancel as *const AtomicBool).cast_mut().cast(),
                &mut p,
                Some(read_callback),
                Some(seek_callback),
                (&mut input as *mut Reader<'_>).cast(),
                selection(selected_audio)?,
            )
        },
        cancel,
    )?;
    Ok(convert(p))
}
pub fn packet_copy_mp4_reader(
    reader: &mut dyn ReadSeek,
    destination: &Path,
    max_bytes: u64,
    cancel: &AtomicBool,
) -> Result<Probe, Error> {
    packet_copy_mp4_reader_selected(reader, destination, max_bytes, cancel, None)
}
pub fn packet_copy_mp4_reader_selected(
    reader: &mut dyn ReadSeek,
    destination: &Path,
    max_bytes: u64,
    cancel: &AtomicBool,
    selected_audio: Option<usize>,
) -> Result<Probe, Error> {
    if destination.exists() {
        return Err(Error::InvalidInput);
    }
    let dst = path(destination)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::InvalidInput)?;
    let mut input = Reader {
        inner: reader,
        cancel,
        started: Some(std::time::Instant::now()),
    };
    let result = checked(
        unsafe {
            nova_media_remux(
                std::ptr::null(),
                dst.as_ptr(),
                cancelled,
                (cancel as *const AtomicBool).cast_mut().cast(),
                max_bytes.min(i64::MAX as u64) as i64,
                Some(read_callback),
                Some(seek_callback),
                (&mut input as *mut Reader<'_>).cast(),
                selection(selected_audio)?,
            )
        },
        cancel,
    )
    .and_then(|()| probe(destination, cancel));
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, sync::atomic::AtomicUsize};
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/h264-aac.mkv");
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "nova-remux-test-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn digest(path: &Path) -> (u64, u64, i32) {
        digest_selected(path, -1)
    }
    fn digest_selected(path: &Path, selected_audio: i32) -> (u64, u64, i32) {
        unsafe extern "C" {
            fn nova_media_packet_fingerprint(
                path: *const c_char,
                video: *mut u64,
                audio: *mut u64,
                packets: *mut i32,
                selected_audio: i32,
            ) -> c_int;
        }
        let path = super::path(path).unwrap();
        let (mut video, mut audio, mut count) = (0, 0, 0);
        assert_eq!(
            unsafe {
                nova_media_packet_fingerprint(
                    path.as_ptr(),
                    &mut video,
                    &mut audio,
                    &mut count,
                    selected_audio,
                )
            },
            0
        );
        (video, audio, count)
    }
    fn stream_fixture(fixture: &[u8], start: f64, cap: usize) -> Result<Vec<StreamEvent>, Error> {
        let mut events = Vec::new();
        packet_copy_stream(
            &mut Cursor::new(fixture),
            StreamOptions {
                selected_audio_stream: None,
                start_position: start,
                max_fragment_bytes: cap,
            },
            &AtomicBool::new(false),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )?;
        Ok(events)
    }
    fn boxes(bytes: &[u8]) -> Vec<[u8; 4]> {
        let mut at = 0;
        let mut result = Vec::new();
        while at < bytes.len() {
            assert!(bytes.len() - at >= 8);
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            assert!(size >= 8 && size <= bytes.len() - at);
            result.push(bytes[at + 4..at + 8].try_into().unwrap());
            at += size;
        }
        result
    }
    #[test]
    fn ram_stream_emits_complete_boxes_and_unchanged_packets() {
        for fixture in [
            FIXTURE,
            include_bytes!("../tests/fixtures/h264-aac-bframes.mp4").as_slice(),
        ] {
            let dir = Directory::new();
            let events = stream_fixture(fixture, 0.0, 1024 * 1024).unwrap();
            // The production call receives only an in-memory reader and callback.
            assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
            assert!(matches!(events.last(), Some(StreamEvent::Finished)));
            let mut bytes = Vec::new();
            for event in events {
                match event {
                    StreamEvent::Init { bytes: b, .. } => {
                        assert!(boxes(&b).contains(b"moov"));
                        bytes.extend(b);
                    }
                    StreamEvent::Fragment {
                        bytes: b,
                        start,
                        duration,
                    } => {
                        let kinds = boxes(&b);
                        assert!(kinds.contains(b"moof"));
                        assert!(kinds.contains(b"mdat"));
                        assert!(start >= 0.0 && duration > 0.0);
                        bytes.extend(b);
                    }
                    StreamEvent::Finished => {}
                }
            }
            let source = dir.0.join("source");
            let output = dir.0.join("out.mp4");
            std::fs::write(&source, fixture).unwrap();
            std::fs::write(&output, bytes).unwrap();
            assert_eq!(digest(&source), digest(&output));
        }
    }
    fn times(path: &Path) -> Vec<[f64; 3]> {
        unsafe extern "C" {
            fn nova_media_packet_times(
                path: *const c_char,
                times: *mut f64,
                capacity: c_int,
            ) -> c_int;
        }
        let mut times = vec![[0.0; 3]; 1024];
        let count = unsafe {
            nova_media_packet_times(
                super::path(path).unwrap().as_ptr(),
                times.as_mut_ptr().cast(),
                1024,
            )
        };
        assert!(count > 0);
        times.truncate(count as usize);
        times
    }
    #[test]
    fn ram_stream_preserves_shared_av_timeline_and_bframe_offsets() {
        let fixture = include_bytes!("../tests/fixtures/h264-aac-bframes.mp4");
        let events = stream_fixture(fixture, 0.0, 1024 * 1024).unwrap();
        let mut bytes = Vec::new();
        let mut origin = 0.0;
        for event in events {
            match event {
                StreamEvent::Init {
                    bytes: b,
                    timeline_offset,
                } => {
                    bytes.extend(b);
                    origin = timeline_offset
                }
                StreamEvent::Fragment { bytes: b, .. } => bytes.extend(b),
                _ => {}
            }
        }
        let dir = Directory::new();
        let source = dir.0.join("source.mp4");
        let output = dir.0.join("out.mp4");
        std::fs::write(&source, fixture).unwrap();
        std::fs::write(&output, bytes).unwrap();
        let before = times(&source);
        let after = times(&output);
        for track in [0.0, 1.0] {
            let a: Vec<_> = before.iter().filter(|t| t[0] == track).collect();
            let b: Vec<_> = after.iter().filter(|t| t[0] == track).collect();
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                assert!(
                    (a[1] - origin - b[1]).abs() < 0.001,
                    "PTS {a:?} => {b:?} origin {origin}"
                );
                assert!(
                    (a[2] - origin - b[2]).abs() < 0.001,
                    "DTS {a:?} => {b:?} origin {origin}"
                );
            }
        }
    }
    #[test]
    fn ram_stream_transport_stream_and_resume() {
        let fixture = include_bytes!("../tests/fixtures/h264-aac.ts");
        assert!(stream_fixture(fixture, 0.0, 1024 * 1024).is_ok());
        let events = stream_fixture(
            include_bytes!("../tests/fixtures/h264-aac-bframes.mp4"),
            0.5,
            1024 * 1024,
        )
        .unwrap();
        assert!(
            matches!(&events[0],StreamEvent::Init{timeline_offset,..} if *timeline_offset<=0.5)
        );
    }
    #[test]
    fn ram_stream_multiple_keyframe_fragments_and_nonzero_source_resume() {
        let fixture = include_bytes!("../tests/fixtures/h264-aac-offset.mp4");
        for requested in [0.0, 3.5] {
            let events = stream_fixture(fixture, requested, 1024 * 1024).unwrap();
            let mut bytes = Vec::new();
            let mut origin = 0.0;
            let mut starts = Vec::new();
            for event in events {
                match event {
                    StreamEvent::Init {
                        bytes: b,
                        timeline_offset,
                    } => {
                        bytes.extend(b);
                        origin = timeline_offset
                    }
                    StreamEvent::Fragment {
                        bytes: b,
                        start,
                        duration,
                    } => {
                        assert!(boxes(&b).contains(b"moof"));
                        assert!(duration > 0.0 && duration < 1.3);
                        starts.push(start);
                        bytes.extend(b)
                    }
                    _ => {}
                }
            }
            assert!(starts.len() >= 3);
            assert_eq!(starts[0], 0.0);
            assert!(starts.windows(2).all(|p| p[1] > p[0]));
            assert!(origin <= requested);
            if requested > 0.0 {
                assert!(origin > 2.0, "resume unexpectedly restarted: {origin}");
            }
            let dir = Directory::new();
            let source = dir.0.join("source.mp4");
            let output = dir.0.join("out.mp4");
            std::fs::write(&source, fixture).unwrap();
            std::fs::write(&output, bytes).unwrap();
            if requested == 0.0 {
                assert_eq!(digest(&source), digest(&output));
            }
            let before = times(&source);
            let after = times(&output);
            // Fixture has a nonzero container start, including AAC encoder priming.
            let source_zero = 6.976009;
            for b in &after {
                assert!(
                    before.iter().any(|a| a[0] == b[0]
                        && (a[1] - source_zero - origin - b[1]).abs() < 0.001
                        && (a[2] - source_zero - origin - b[2]).abs() < 0.001),
                    "unmapped packet {b:?}, offset {origin}"
                );
            }
        }
    }
    #[test]
    fn ram_stream_audio_only_uses_bounded_duration_fragments() {
        let fixture = include_bytes!("../tests/fixtures/aac-only.m4a");
        let events = stream_fixture(fixture, 0.0, 1024 * 1024).unwrap();
        let fragments: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Fragment { duration, .. } => Some(*duration),
                _ => None,
            })
            .collect();
        assert!(fragments.len() >= 3);
        assert!(fragments.iter().all(|d| *d > 0.0 && *d < 2.1));
    }
    #[test]
    fn ram_stream_blocked_consumer_can_cancel_without_finished_event() {
        let cancel = AtomicBool::new(false);
        let mut finished = false;
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::sync_channel(0);
            let cancellation = &cancel;
            scope.spawn(move || {
                rx.recv().unwrap();
                cancellation.store(true, Ordering::Relaxed);
            });
            let result = packet_copy_stream(
                &mut Cursor::new(FIXTURE),
                StreamOptions {
                    selected_audio_stream: None,
                    start_position: 0.0,
                    max_fragment_bytes: 1024 * 1024,
                },
                &cancel,
                &mut |event| {
                    if matches!(event, StreamEvent::Finished) {
                        finished = true;
                    }
                    tx.send(()).unwrap();
                    while !cancel.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                    Err(Error::Cancelled)
                },
            );
            assert_eq!(result, Err(Error::Cancelled));
        });
        assert!(!finished);
    }
    #[test]
    fn ram_stream_limit_cancel_and_callback_errors_are_propagated() {
        assert_eq!(
            stream_fixture(FIXTURE, 0.0, 1024).unwrap_err(),
            Error::OutputLimit
        );
        let cancel = AtomicBool::new(false);
        let result = packet_copy_stream(
            &mut Cursor::new(FIXTURE),
            StreamOptions {
                selected_audio_stream: None,
                start_position: 0.0,
                max_fragment_bytes: 1024 * 1024,
            },
            &cancel,
            &mut |_| {
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            },
        );
        assert_eq!(result, Err(Error::Cancelled));
        let result = packet_copy_stream(
            &mut Cursor::new(FIXTURE),
            StreamOptions {
                selected_audio_stream: None,
                start_position: 0.0,
                max_fragment_bytes: 1024 * 1024,
            },
            &AtomicBool::new(false),
            &mut |_| Err(Error::OutputLimit),
        );
        assert_eq!(result, Err(Error::OutputLimit));
    }
    #[test]
    fn copy_preserves_every_packet_and_finalizes_seekable_mp4() {
        let dir = Directory::new();
        let source = dir.0.join("source.mkv");
        std::fs::write(&source, FIXTURE).unwrap();
        let output = dir.0.join("output.mp4");
        let cancel = AtomicBool::new(false);
        let input = probe(&source, &cancel).unwrap();
        assert_eq!(input.video, Some(Codec::H264));
        assert_eq!(input.audio, Some(Codec::Aac));
        assert_eq!(input.container, Container::Matroska);
        let result = packet_copy_mp4(&source, &output, 1024 * 1024, &cancel).unwrap();
        assert_eq!(result.container, Container::Mp4);
        assert!(result.seekable);
        assert!(result.duration.unwrap() > 0.9);
        assert_eq!(digest(&source), digest(&output));
        let bytes = std::fs::read(output).unwrap();
        let moov = bytes.windows(4).position(|w| w == b"moov").unwrap();
        let mdat = bytes.windows(4).position(|w| w == b"mdat").unwrap();
        assert!(moov < mdat);
    }
    #[test]
    fn explicit_audio_stream_is_the_only_audio_copied() {
        let dir = Directory::new();
        let source = dir.0.join("two.mkv");
        std::fs::write(&source, include_bytes!("../tests/fixtures/two-audio.mkv")).unwrap();
        let mut reader = std::fs::File::open(&source).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            probe_reader_selected(&mut reader, &cancel, Some(2))
                .unwrap()
                .audio_stream,
            Some(2)
        );
        let output = dir.0.join("selected.mp4");
        packet_copy_mp4_reader_selected(&mut reader, &output, 1024 * 1024, &cancel, Some(2))
            .unwrap();
        assert_eq!(digest_selected(&source, 2), digest(&output));
        let invalid = dir.0.join("invalid.mp4");
        assert!(
            packet_copy_mp4_reader_selected(&mut reader, &invalid, 1024 * 1024, &cancel, Some(0))
                .is_err()
        );
        assert!(!invalid.exists());
    }
    #[test]
    fn transport_stream_uses_packet_bitstream_filter_for_mp4() {
        let dir = Directory::new();
        let mut reader = Cursor::new(include_bytes!("../tests/fixtures/h264-aac.ts").as_slice());
        let cancel = AtomicBool::new(false);
        let output = dir.0.join("from-ts.mp4");
        let result = packet_copy_mp4_reader(&mut reader, &output, 1024 * 1024, &cancel).unwrap();
        assert_eq!(result.video, Some(Codec::H264));
        assert_eq!(result.audio, Some(Codec::Aac));
        assert_eq!(result.container, Container::Mp4);
        assert!(result.duration.unwrap() > 0.9);
    }
    #[test]
    fn matroska_missing_initial_bframe_dts_is_rejected_without_guessing() {
        let dir = Directory::new();
        let mut reader =
            Cursor::new(include_bytes!("../tests/fixtures/h264-aac-missing-dts.mkv").as_slice());
        let output = dir.0.join("output.mp4");
        assert!(matches!(
            packet_copy_mp4_reader(&mut reader, &output, 1024 * 1024, &AtomicBool::new(false)),
            Err(Error::Native(-1094995529))
        ));
        assert!(!output.exists());
    }
    #[test]
    fn bframe_mp4_keeps_payloads_and_reordered_timestamps() {
        let dir = Directory::new();
        let source = dir.0.join("source.mp4");
        std::fs::write(
            &source,
            include_bytes!("../tests/fixtures/h264-aac-bframes.mp4"),
        )
        .unwrap();
        let output = dir.0.join("output.mp4");
        packet_copy_mp4(&source, &output, 1024 * 1024, &AtomicBool::new(false)).unwrap();
        assert_eq!(digest(&source), digest(&output));
    }
    #[test]
    fn custom_io_resets_after_probe_and_copies_without_decoding() {
        let dir = Directory::new();
        let mut reader = Cursor::new(FIXTURE);
        let cancel = AtomicBool::new(false);
        assert_eq!(
            probe_reader(&mut reader, &cancel).unwrap().video,
            Some(Codec::H264)
        );
        let output = dir.0.join("output.mp4");
        assert!(packet_copy_mp4_reader(&mut reader, &output, 1024 * 1024, &cancel).is_ok());
    }
    #[test]
    fn cancellation_quota_and_bad_input_leave_no_partial_artifact() {
        let dir = Directory::new();
        let output = dir.0.join("output.mp4");
        let mut reader = Cursor::new(FIXTURE);
        assert_eq!(
            packet_copy_mp4_reader(&mut reader, &output, 1024 * 1024, &AtomicBool::new(true))
                .unwrap_err(),
            Error::Cancelled
        );
        assert!(!output.exists());
        assert_eq!(
            packet_copy_mp4_reader(&mut reader, &output, 1, &AtomicBool::new(false)).unwrap_err(),
            Error::OutputLimit
        );
        assert!(!output.exists());
        let mut invalid = Cursor::new(b"this is not media");
        assert!(
            packet_copy_mp4_reader(&mut invalid, &output, 1024, &AtomicBool::new(false)).is_err()
        );
        assert!(!output.exists());
    }
}

/// A synchronous packet-copy producer. The consumer owns bounded retention and
/// must wake blocked callbacks when `cancel` is set. No output file is created.
#[derive(Clone, Copy, Debug)]
pub struct StreamOptions {
    pub selected_audio_stream: Option<usize>,
    /// Seconds on the source presentation timeline (relative to its start time).
    pub start_position: f64,
    /// Hard cap on an emitted fragment, including its MP4 box headers.
    pub max_fragment_bytes: usize,
}
#[derive(Debug)]
pub enum StreamEvent {
    Init {
        bytes: Vec<u8>,
        timeline_offset: f64,
    },
    /// A complete moof/mdat group; times are on the rebased receiver timeline.
    Fragment {
        bytes: Vec<u8>,
        start: f64,
        duration: f64,
    },
    Finished,
}
type EmitFn = extern "C" fn(*mut c_void, c_int, *const u8, c_int, f64, f64, f64) -> c_int;
struct Emitter<'a> {
    emit: &'a mut dyn FnMut(StreamEvent) -> Result<(), Error>,
    cancel: &'a AtomicBool,
    error: Option<Error>,
}
extern "C" fn stream_callback(
    opaque: *mut c_void,
    kind: c_int,
    data: *const u8,
    size: c_int,
    timeline_offset: f64,
    start: f64,
    duration: f64,
) -> c_int {
    let emitter = unsafe { &mut *opaque.cast::<Emitter<'_>>() };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if emitter.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let bytes = if size > 0 {
            unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec()
        } else {
            Vec::new()
        };
        let event = match kind {
            0 => StreamEvent::Init {
                bytes,
                timeline_offset,
            },
            1 if start.is_finite() && start >= 0.0 && duration.is_finite() && duration > 0.0 => {
                StreamEvent::Fragment {
                    bytes,
                    start,
                    duration,
                }
            }
            2 => StreamEvent::Finished,
            _ => return Err(Error::InvalidInput),
        };
        (emitter.emit)(event)
    }))
    .unwrap_or(Err(Error::InvalidInput));
    match result {
        Ok(()) => 0,
        Err(error) => {
            emitter.error = Some(error);
            -5
        }
    }
}
/// Emits bounded, keyframe-aligned fragmented MP4 using a nonseekable custom
/// writer. A single shared DTS origin preserves A/V offsets and B-frame CTS.
/// `source_position = receiver_position + timeline_offset`. Initial input seek
/// selects a preceding keyframe; subsequent arbitrary seeking is unsupported.
/// The reader must implement interruptible I/O (its own network timeout/cancel).
/// Callbacks may block indefinitely for pause/backpressure, but must honor cancel.
pub fn packet_copy_stream(
    reader: &mut dyn ReadSeek,
    options: StreamOptions,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(StreamEvent) -> Result<(), Error>,
) -> Result<(), Error> {
    unsafe extern "C" {
        fn nova_media_stream(
            cancel: extern "C" fn(*mut c_void) -> c_int,
            opaque: *mut c_void,
            read: ReadFn,
            seek: SeekFn,
            io: *mut c_void,
            selected_audio: c_int,
            start: f64,
            limit: i64,
            emit: EmitFn,
            emitter: *mut c_void,
        ) -> c_int;
    }
    if !options.start_position.is_finite()
        || options.start_position < 0.0
        || options.start_position > (i64::MAX / 1_000_000) as f64
        || !(1024..=64 * 1024 * 1024).contains(&options.max_fragment_bytes)
    {
        return Err(Error::InvalidInput);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::InvalidInput)?;
    let mut input = Reader {
        inner: reader,
        cancel,
        started: None,
    };
    let mut emitter = Emitter {
        emit,
        cancel,
        error: None,
    };
    let code = unsafe {
        nova_media_stream(
            cancelled,
            (cancel as *const AtomicBool).cast_mut().cast(),
            read_callback,
            seek_callback,
            (&mut input as *mut Reader<'_>).cast(),
            selection(options.selected_audio_stream)?,
            options.start_position,
            options.max_fragment_bytes as i64,
            stream_callback,
            (&mut emitter as *mut Emitter<'_>).cast(),
        )
    };
    if let Some(error) = emitter.error {
        Err(error)
    } else {
        checked(code, cancel)
    }
}
