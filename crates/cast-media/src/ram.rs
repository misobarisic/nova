//! Bounded, session-owned packet-copy output. No full remux artifact is written.
use crate::{http::Resource, *};
use std::{
    collections::{BTreeMap, HashMap},
    net::TcpListener,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_FRAGMENT: usize = 16 * 1024 * 1024;
const MAX_STORED_SEGMENTS: usize = 128;
const MAX_AHEAD: usize = 3;
// Stable for the complete sliding presentation, including variable GOP lengths.
const TARGET_DURATION: u64 = 10;
const STARTUP_SECONDS: f64 = 3.0 * TARGET_DURATION as f64;
static PRODUCERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn acquire() -> Result<Self, MediaError> {
        let mut count = PRODUCERS.load(Ordering::Acquire);
        loop {
            if count >= 2 {
                return Err(MediaError::ResourceLimit);
            }
            match PRODUCERS.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self),
                Err(actual) => count = actual,
            }
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        PRODUCERS.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Segment {
    id: String,
    data: Arc<Vec<u8>>,
    start: f64,
    duration: f64,
    fetched: bool,
}
struct State {
    init: Option<Arc<Vec<u8>>>,
    offset: f64,
    segments: BTreeMap<u64, Segment>,
    next: u64,
    highest_fetched: Option<u64>,
    playhead: f64,
    bytes: usize,
    revision: u64,
    finished: bool,
    serving: bool,
    error: Option<MediaError>,
}
pub(crate) struct Store {
    state: Mutex<State>,
    changed: Condvar,
    cancel: Arc<AtomicBool>,
    cap: usize,
    max_fragment: usize,
    base: String,
    init_id: String,
    requested_start: f64,
}
impl Store {
    fn new(base: String, cancel: Arc<AtomicBool>, cap: usize, requested_start: f64) -> Self {
        let cap = cap.min(MAX_RAM_BUFFER_BYTES);
        Self {
            state: Mutex::new(State {
                init: None,
                offset: 0.0,
                segments: BTreeMap::new(),
                next: 0,
                highest_fetched: None,
                playhead: 0.0,
                bytes: 0,
                revision: 0,
                finished: false,
                serving: false,
                error: None,
            }),
            changed: Condvar::new(),
            cancel,
            cap,
            max_fragment: MAX_FRAGMENT.min(cap / 4),
            base,
            init_id: http::token(),
            requested_start,
        }
    }
    pub(crate) fn wake(&self) {
        self.changed.notify_all();
    }
    pub(crate) fn error(&self) -> Option<MediaError> {
        self.state.lock().ok().and_then(|s| s.error)
    }
    fn fail(&self, error: MediaError) {
        if let Ok(mut state) = self.state.lock() {
            state.error = Some(error);
        }
        self.wake();
    }
    fn checked(&self) -> Result<(), nova_remux::Error> {
        if self.cancel.load(Ordering::Acquire) {
            Err(nova_remux::Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn prune(state: &mut State, needed: usize, cap: usize) {
        while let Some((&sequence, segment)) = state.segments.first_key_value() {
            // Only a confirmed/requested playhead can prove an unfetched prefix
            // obsolete. Later concurrent GET completion does not establish that.
            let advanced = segment.start + segment.duration <= state.playhead;
            let outside_window = state
                .highest_fetched
                .is_some_and(|last| sequence.saturating_add(MAX_AHEAD as u64) <= last);
            let pressure = state.bytes.saturating_add(needed) > cap
                || state.segments.len() >= MAX_STORED_SEGMENTS;
            if (!segment.fetched && !advanced)
                || Arc::strong_count(&segment.data) != 1
                || (!outside_window && !pressure)
            {
                break;
            }
            let removed = state
                .segments
                .remove(&sequence)
                .expect("first segment remains under lock");
            state.bytes -= removed.data.len();
            state.revision += 1;
        }
    }
    fn emit(&self, event: nova_remux::StreamEvent) -> Result<(), nova_remux::Error> {
        self.checked()?;
        match event {
            nova_remux::StreamEvent::Init {
                bytes,
                timeline_offset,
            } => {
                if !timeline_offset.is_finite()
                    || bytes.is_empty()
                    || bytes.len() > self.cap / 4
                    || bytes.len() > 8 * 1024 * 1024
                {
                    return Err(nova_remux::Error::OutputLimit);
                }
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| nova_remux::Error::InvalidInput)?;
                if state.init.is_some() {
                    return Err(nova_remux::Error::InvalidInput);
                }
                state.bytes = bytes.len();
                state.offset = timeline_offset;
                state.playhead = (self.requested_start - timeline_offset).max(0.0);
                state.init = Some(Arc::new(bytes));
                state.revision += 1;
                self.wake();
            }
            nova_remux::StreamEvent::Fragment {
                bytes,
                start,
                duration,
            } => {
                if bytes.is_empty() || bytes.len() > self.max_fragment {
                    return Err(nova_remux::Error::OutputLimit);
                }
                if !start.is_finite()
                    || start < 0.0
                    || !duration.is_finite()
                    || duration <= 0.0
                    || duration > TARGET_DURATION as f64
                {
                    return Err(nova_remux::Error::InvalidInput);
                }
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| nova_remux::Error::InvalidInput)?;
                if state.init.is_none() || state.finished {
                    return Err(nova_remux::Error::InvalidInput);
                }
                if state
                    .segments
                    .last_key_value()
                    .is_some_and(|(_, last)| start + 0.001 < last.start)
                {
                    return Err(nova_remux::Error::InvalidInput);
                }
                loop {
                    self.checked()?;
                    Self::prune(&mut state, bytes.len(), self.cap);
                    let ahead: f64 = state
                        .segments
                        .values()
                        .filter(|s| !s.fetched)
                        .map(|s| (s.start + s.duration - s.start.max(state.playhead)).max(0.0))
                        .sum();
                    let fits = state.segments.len() < MAX_STORED_SEGMENTS
                        && state.bytes.saturating_add(bytes.len()) <= self.cap;
                    // No receiver can free bytes until prepare returns a route.
                    // Fail a too-small startup budget instead of deadlocking it.
                    if !state.serving && ahead < STARTUP_SECONDS && !fits {
                        return Err(nova_remux::Error::OutputLimit);
                    }
                    if ahead < STARTUP_SECONDS && fits {
                        break;
                    }
                    state = self
                        .changed
                        .wait_timeout(state, Duration::from_millis(100))
                        .map_err(|_| nova_remux::Error::InvalidInput)?
                        .0;
                }
                let sequence = state.next;
                state.next += 1;
                state.bytes += bytes.len();
                state.revision += 1;
                state.segments.insert(
                    sequence,
                    Segment {
                        id: http::token(),
                        data: Arc::new(bytes),
                        start,
                        duration,
                        fetched: false,
                    },
                );
                self.wake();
            }
            nova_remux::StreamEvent::Finished => {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| nova_remux::Error::InvalidInput)?;
                if state.init.is_none() || state.next == 0 {
                    return Err(nova_remux::Error::InvalidInput);
                }
                state.finished = true;
                state.revision += 1;
                self.wake();
            }
        }
        Ok(())
    }
    fn ready(&self) -> Result<(Arc<Vec<u8>>, f64), MediaError> {
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut state = self.state.lock().map_err(|_| MediaError::Native)?;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Err(MediaError::Cancelled);
            }
            if let Some(error) = state.error {
                return Err(error);
            }
            let buffered: f64 = state
                .segments
                .values()
                .map(|s| (s.start + s.duration - s.start.max(state.playhead)).max(0.0))
                .sum();
            if !state.segments.is_empty()
                && (state.finished || buffered >= STARTUP_SECONDS)
                && let Some(init) = state.init.clone()
            {
                state.serving = true;
                self.wake();
                return Ok((init, state.offset));
            }
            if Instant::now() > deadline {
                return Err(MediaError::ResourceLimit);
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .map_err(|_| MediaError::Native)?
                .0;
        }
    }
    pub(crate) fn playlist(&self) -> Result<(Vec<u8>, u64), MediaError> {
        let state = self.state.lock().map_err(|_| MediaError::Native)?;
        if let Some(error) = state.error {
            return Err(error);
        }
        let sequence = state
            .segments
            .first_key_value()
            .map_or(state.next, |(n, _)| *n);
        let target = TARGET_DURATION;
        let mut text = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{sequence}\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-START:TIME-OFFSET=0,PRECISE=YES\n#EXT-X-MAP:URI=\"{}/{}\"\n",
            self.base, self.init_id
        );
        for segment in state.segments.values() {
            text.push_str(&format!(
                "#EXTINF:{:.6},\n{}/{}\n",
                segment.duration, self.base, segment.id
            ));
        }
        if state.finished {
            text.push_str("#EXT-X-ENDLIST\n");
        }
        Ok((text.into_bytes(), state.revision))
    }
    pub(crate) fn segment(&self, id: &str) -> Option<(Arc<Vec<u8>>, u64)> {
        let state = self.state.lock().ok()?;
        state
            .segments
            .iter()
            .find(|(_, s)| s.id == id)
            .map(|(n, s)| (s.data.clone(), *n))
    }
    pub(crate) fn set_playhead(&self, position: f64) {
        if position.is_finite() && position >= 0.0 {
            if let Ok(mut state) = self.state.lock() {
                state.playhead = state.playhead.max(position);
            }
            self.wake();
        }
    }
    pub(crate) fn fetched(&self, sequence: u64) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(segment) = state.segments.get_mut(&sequence) {
                segment.fetched = true;
            }
            state.highest_fetched = Some(
                state
                    .highest_fetched
                    .map_or(sequence, |last| last.max(sequence)),
            );
        }
        self.wake();
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    mut reader: Box<dyn nova_remux::ReadSeek + Send>,
    probe: nova_remux::Probe,
    profile: OutputProfile,
    options: PrepareOptions,
    cancel: Arc<AtomicBool>,
    listener: TcpListener,
    base: String,
    prefix: String,
    main: String,
) -> Result<PreparedMedia, MediaError> {
    if !profile.hls && !(options.allow_unverified && !profile.codecs_verified) {
        return Err(if profile.codecs_verified {
            MediaError::UnsupportedContainer
        } else {
            MediaError::UnverifiedFormat
        });
    }
    if !options.start_position.is_finite()
        || options.start_position < 0.0
        || options.max_buffer_bytes < 4 * 1024 * 1024
    {
        return Err(MediaError::InvalidSource);
    }
    let permit = Permit::acquire()?;
    let store = Arc::new(Store::new(
        base.clone(),
        cancel.clone(),
        options.max_buffer_bytes,
        options.start_position,
    ));
    let producer = store.clone();
    let token = cancel.clone();
    let stream_options = nova_remux::StreamOptions {
        selected_audio_stream: options.selected_audio_stream,
        start_position: options.start_position,
        max_fragment_bytes: store.max_fragment,
    };
    thread::Builder::new()
        .name("cast-ram-packet-copy".into())
        .spawn(move || {
            let _permit = permit;
            let result = nova_remux::packet_copy_stream(
                reader.as_mut(),
                stream_options,
                &token,
                &mut |event| producer.emit(event),
            );
            match result {
                Err(error) if !token.load(Ordering::Acquire) => producer.fail(error.into()),
                Ok(()) if !producer.state.lock().is_ok_and(|s| s.finished) => {
                    producer.fail(MediaError::Native)
                }
                _ => {}
            }
        })
        .map_err(|_| MediaError::ResourceLimit)?;
    let (init, offset) = store.ready()?;
    let mut resources = HashMap::new();
    resources.insert(
        main.clone(),
        Resource::RamPlaylist {
            store: store.clone(),
        },
    );
    resources.insert(
        store.init_id.clone(),
        Resource::Shared {
            data: init,
            mime: "video/mp4".into(),
        },
    );
    let (text_tracks, text_track_warnings) = http::prepare_text_tracks(
        &options.text_tracks,
        &mut resources,
        &base,
        offset,
        cancel.clone(),
    );
    let mut prepared = http::start(
        listener,
        base,
        main,
        "application/vnd.apple.mpegurl".into(),
        probe.duration,
        false,
        DeliveryMode::PacketCopyHls,
        resources,
        prefix,
        cancel,
        options.max_readers,
        None,
    )?;
    prepared.timeline_offset = offset;
    prepared.text_tracks = text_tracks;
    prepared.text_track_warnings = text_track_warnings;
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, sync::atomic::AtomicBool};
    fn init(store: &Store) {
        store
            .emit(nova_remux::StreamEvent::Init {
                bytes: vec![0; 8],
                timeline_offset: 0.0,
            })
            .unwrap();
        store.state.lock().unwrap().serving = true;
    }
    fn fragment(store: &Store, start: f64) {
        store
            .emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![1; 24],
                start,
                duration: 1.0,
            })
            .unwrap();
    }
    #[test]
    fn active_http_arcs_remain_counted_and_consumed_prefix_is_evicted_under_pressure() {
        let store = Arc::new(Store::new(
            "http://192.168.1.1/private".into(),
            Arc::new(AtomicBool::new(false)),
            96,
            0.0,
        ));
        init(&store);
        fragment(&store, 0.0);
        fragment(&store, 1.0);
        fragment(&store, 2.0);
        let first_id = store.state.lock().unwrap().segments[&0].id.clone();
        let (active, _) = store.segment(&first_id).unwrap();
        store.fetched(0);
        let finished = Arc::new(AtomicBool::new(false));
        let result = finished.clone();
        let producer = store.clone();
        let worker = thread::spawn(move || {
            fragment(&producer, 3.0);
            result.store(true, Ordering::Release);
        });
        thread::sleep(Duration::from_millis(40));
        assert!(!finished.load(Ordering::Acquire));
        assert_eq!(store.state.lock().unwrap().bytes, 80);
        drop(active);
        store.wake();
        worker.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
        assert!(store.segment(&first_id).is_none());
        let state = store.state.lock().unwrap();
        assert!(state.bytes <= 80);
        assert_eq!(state.segments.first_key_value().unwrap().0, &1);
    }
    #[test]
    fn receiver_start_skipping_old_segments_does_not_deadlock_full_store() {
        let store = Store::new(
            "http://192.168.1.1/private".into(),
            Arc::new(AtomicBool::new(false)),
            96,
            0.0,
        );
        init(&store);
        fragment(&store, 0.0);
        fragment(&store, 1.0);
        fragment(&store, 2.0);
        store.set_playhead(2.0);
        store.fetched(2);
        fragment(&store, 3.0);
        let state = store.state.lock().unwrap();
        assert!(state.bytes <= 80);
        assert_eq!(state.segments.first_key_value().unwrap().0, &1);
    }
    #[test]
    fn out_of_order_later_get_keeps_unrequested_earlier_segment() {
        let store = Arc::new(Store::new(
            "http://192.168.1.1/private".into(),
            Arc::new(AtomicBool::new(false)),
            96,
            0.0,
        ));
        init(&store);
        fragment(&store, 0.0);
        fragment(&store, 1.0);
        fragment(&store, 2.0);
        let first_id = store.state.lock().unwrap().segments[&0].id.clone();
        store.fetched(2);
        let producer = store.clone();
        let worker = thread::spawn(move || {
            producer.emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![0; 24],
                start: 3.0,
                duration: 1.0,
            })
        });
        thread::sleep(Duration::from_millis(30));
        assert!(store.segment(&first_id).is_some());
        store.cancel.store(true, Ordering::Release);
        store.wake();
        assert_eq!(worker.join().unwrap(), Err(nova_remux::Error::Cancelled));
    }
    #[test]
    fn tail_range_does_not_mark_whole_segment_consumed_or_evict_prefix() {
        let cancel = Arc::new(AtomicBool::new(false));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}/aabb");
        let store = Arc::new(Store::new(base.clone(), cancel.clone(), 96, 0.0));
        init(&store);
        fragment(&store, 0.0);
        fragment(&store, 1.0);
        fragment(&store, 2.0);
        let first_id = store.state.lock().unwrap().segments[&0].id.clone();
        let mut resources = HashMap::new();
        resources.insert(
            "ccdd".into(),
            Resource::RamPlaylist {
                store: store.clone(),
            },
        );
        let prepared = http::start(
            listener,
            base.clone(),
            "ccdd".into(),
            "application/vnd.apple.mpegurl".into(),
            None,
            false,
            DeliveryMode::PacketCopyHls,
            resources,
            "aabb".into(),
            cancel,
            2,
            None,
        )
        .unwrap();
        let client = reqwest::blocking::Client::new();
        let url = format!("{base}/{first_id}");
        assert_eq!(
            client
                .get(&url)
                .header("range", "bytes=-1")
                .send()
                .unwrap()
                .bytes()
                .unwrap()
                .len(),
            1
        );
        assert!(!store.state.lock().unwrap().segments[&0].fetched);
        let producer = store.clone();
        let worker = thread::spawn(move || fragment(&producer, 3.0));
        thread::sleep(Duration::from_millis(30));
        assert_eq!(
            client
                .get(&url)
                .header("range", "bytes=0-3")
                .send()
                .unwrap()
                .bytes()
                .unwrap()
                .len(),
            4
        );
        assert!(!store.state.lock().unwrap().segments[&0].fetched);
        assert_eq!(client.get(&url).send().unwrap().bytes().unwrap().len(), 24);
        worker.join().unwrap();
        assert!(store.segment(&first_id).is_none());
        drop(prepared);
    }
    #[test]
    fn backpressure_is_cancelled_and_oversized_fragments_fail_before_waiting() {
        let cancel = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Store::new(
            "http://192.168.1.1/private".into(),
            cancel.clone(),
            96,
            0.0,
        ));
        init(&store);
        fragment(&store, 0.0);
        fragment(&store, 1.0);
        fragment(&store, 2.0);
        assert_eq!(
            store.emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![0; 41],
                start: 3.0,
                duration: 1.0
            }),
            Err(nova_remux::Error::OutputLimit)
        );
        let producer = store.clone();
        let worker = thread::spawn(move || {
            producer.emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![0; 24],
                start: 3.0,
                duration: 1.0,
            })
        });
        cancel.store(true, Ordering::Release);
        store.wake();
        assert_eq!(worker.join().unwrap(), Err(nova_remux::Error::Cancelled));
    }
    #[test]
    fn insufficient_startup_budget_fails_before_a_receiver_can_deadlock() {
        let store = Store::new(String::new(), Arc::new(AtomicBool::new(false)), 96, 0.0);
        init(&store);
        store.state.lock().unwrap().serving = false;
        fragment(&store, 0.0);
        fragment(&store, 1.0);
        fragment(&store, 2.0);
        assert_eq!(
            store.emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![1; 24],
                start: 3.0,
                duration: 1.0
            }),
            Err(nova_remux::Error::OutputLimit)
        );
    }
    #[test]
    fn configured_budget_can_exceed_default_but_fragment_and_safety_caps_stay_fixed() {
        let store = Store::new(
            String::new(),
            Arc::new(AtomicBool::new(false)),
            128 * 1024 * 1024,
            0.0,
        );
        assert_eq!(store.cap, 128 * 1024 * 1024);
        assert_eq!(store.max_fragment, 16 * 1024 * 1024);
        let store = Store::new(
            String::new(),
            Arc::new(AtomicBool::new(false)),
            usize::MAX,
            0.0,
        );
        assert_eq!(store.cap, 256 * 1024 * 1024);
        assert_eq!(store.max_fragment, 16 * 1024 * 1024);
        assert_eq!(PrepareOptions::default().max_buffer_bytes, 64 * 1024 * 1024);
    }
    #[test]
    fn fixed_target_accepts_short_first_and_longer_later_gop() {
        let store = Store::new(
            "http://192.168.1.1/private".into(),
            Arc::new(AtomicBool::new(false)),
            96,
            0.0,
        );
        init(&store);
        fragment(&store, 0.0);
        store
            .emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![1; 24],
                start: 1.0,
                duration: 10.0,
            })
            .unwrap();
        let playlist = String::from_utf8(store.playlist().unwrap().0).unwrap();
        assert!(playlist.contains("#EXT-X-TARGETDURATION:10\n"));
        assert!(playlist.contains("#EXTINF:1.000000"));
        assert!(playlist.contains("#EXTINF:10.000000"));
        assert_eq!(
            store.emit(nova_remux::StreamEvent::Fragment {
                bytes: vec![1; 24],
                start: 11.0,
                duration: 10.001
            }),
            Err(nova_remux::Error::InvalidInput)
        );
    }
    #[test]
    fn playlist_completion_and_terminal_errors_are_truthful() {
        let store = Store::new(
            "http://192.168.1.1/private".into(),
            Arc::new(AtomicBool::new(false)),
            96,
            0.0,
        );
        init(&store);
        fragment(&store, 0.0);
        let (first, revision) = store.playlist().unwrap();
        assert!(!String::from_utf8(first).unwrap().contains("ENDLIST"));
        store.emit(nova_remux::StreamEvent::Finished).unwrap();
        let (last, next_revision) = store.playlist().unwrap();
        assert!(String::from_utf8(last).unwrap().contains("#EXT-X-ENDLIST"));
        assert!(next_revision > revision);
        store.fail(MediaError::Native);
        assert_eq!(store.error(), Some(MediaError::Native));
        assert_eq!(store.playlist(), Err(MediaError::Native));
    }
    #[test]
    fn actual_packet_copy_is_served_as_ram_hls_without_creating_cache_files() {
        let fixture = include_bytes!("../../remux/tests/fixtures/h264-aac.mkv");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut reader = Cursor::new(fixture.as_slice());
        let probe = nova_remux::probe_reader(&mut reader, &cancel).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let forbidden = cache.path().join("no-remux-files");
        let options = PrepareOptions {
            cache_dir: forbidden.clone(),
            max_buffer_bytes: 4 * 1024 * 1024,
            ..Default::default()
        };
        let profile = OutputProfile {
            hls: true,
            ..Default::default()
        };
        let prepared = prepare(
            Box::new(reader),
            probe,
            profile,
            options,
            cancel,
            listener,
            format!("http://{address}/aabb"),
            "aabb".into(),
            "ccdd".into(),
        )
        .unwrap();
        assert!(!prepared.seekable);
        assert_eq!(prepared.mode, DeliveryMode::PacketCopyHls);
        assert!(!forbidden.exists());
        let client = reqwest::blocking::Client::new();
        let playlist = client
            .get(&prepared.receiver_url)
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(playlist.starts_with("#EXTM3U"));
        assert!(!playlist.contains(".mkv"));
        let init = playlist
            .lines()
            .find_map(|l| {
                l.strip_prefix("#EXT-X-MAP:URI=\"")
                    .and_then(|s| s.strip_suffix('"'))
            })
            .unwrap();
        let mut bytes = client.get(init).send().unwrap().bytes().unwrap().to_vec();
        for url in playlist
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
        {
            bytes.extend_from_slice(&client.get(url).send().unwrap().bytes().unwrap());
        }
        let probe =
            nova_remux::probe_reader(&mut Cursor::new(bytes), &AtomicBool::new(false)).unwrap();
        assert_eq!(probe.container, Container::Mp4);
        assert_eq!(probe.video, Some(Codec::H264));
        assert_eq!(probe.audio, Some(Codec::Aac));
        assert!(!forbidden.exists());
        assert_eq!(prepared.guard.delivery_error(), None);
        drop(prepared);
    }
}
