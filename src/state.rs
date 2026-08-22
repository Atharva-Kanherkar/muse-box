//! Shared playback state, render caching, and meaningful-change detection.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Context;
use chrono::{DateTime, FixedOffset, Offset, Utc};
use tokio::sync::{Mutex, RwLock, broadcast, watch};

use crate::{
    error::AppError,
    idle, image,
    lyrics::LyricsIndex,
    render::{Art, DitherMode, PlaybackState, RENDER_DOCUMENT_VERSION, RenderDoc, VoiceLogEntry},
    spotify::{MAX_RATE_LIMIT_BACKOFF, PlaybackObservation},
};

const MIN_RENDER_DIMENSION: u32 = 16;
const MAX_RENDER_DIMENSION: u32 = 1024;
const DEFAULT_RENDER_DIMENSION: u32 = 400;
const SEEK_THRESHOLD_MS: u64 = 2_000;
const BROADCAST_CAPACITY: usize = 32;
const MAX_RENDER_VARIANTS: usize = 32;
const DEFAULT_PALETTE: [&str; 2] = ["#1a1a1a", "#e0e0e0"];
const NORMAL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// How often the SSE stream sends a comment to hold the socket open. Well under
/// the idle timeout of any proxy likely to sit in front of this.
pub const DEFAULT_SSE_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(15);
const MIN_ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);
const IDLE_THRESHOLD_SECONDS: i64 = 30;
const VOICE_LOG_CAPACITY: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderParams {
    pub width: u32,
    pub height: u32,
    pub dither: DitherMode,
}

impl Default for RenderParams {
    fn default() -> Self {
        Self {
            width: DEFAULT_RENDER_DIMENSION,
            height: DEFAULT_RENDER_DIMENSION,
            dither: DitherMode::Bayer,
        }
    }
}

impl RenderParams {
    pub fn from_query(query: &HashMap<String, String>) -> Result<Self, AppError> {
        let defaults = Self::default();
        let width = parse_dimension(query.get("w"), "w", defaults.width)?;
        let height = parse_dimension(query.get("h"), "h", defaults.height)?;
        let dither = match query.get("dither").map(String::as_str) {
            None | Some("bayer") => DitherMode::Bayer,
            Some("atkinson") => DitherMode::Atkinson,
            Some(value) => {
                return Err(AppError::BadRequest(format!(
                    "invalid dither mode: {value}"
                )));
            }
        };
        Ok(Self {
            width,
            height,
            dither,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub downloads: u64,
    pub dithers: u64,
}

/// Shared state for all SSE subscribers and the single Spotify poll loop.
pub struct StateHub {
    published: RwLock<PlaybackObservation>,
    documents: RwLock<HashMap<RenderParams, Arc<RenderDoc>>>,
    registered: RwLock<HashSet<RenderParams>>,
    render_cache: Mutex<RenderCache>,
    last_accent: RwLock<Option<String>>,
    idle_frame_at: RwLock<Option<DateTime<Utc>>>,
    operation_guard: Mutex<()>,
    voice_log: Mutex<VecDeque<VoiceLogEntry>>,
    playback_activity: watch::Sender<PlaybackActivity>,
    display_offset: FixedOffset,
    keep_alive: std::time::Duration,
    lyrics: Option<Arc<LyricsIndex>>,
    taste: RwLock<Option<Arc<crate::taste::TasteIndex>>>,
    /// Tempo and energy per track id, `None` cached for tracks Spotify will not
    /// describe (the endpoint is deprecated for newer API apps).
    beats: RwLock<HashMap<String, Option<(f32, f32)>>>,
    spotify_features: RwLock<Option<Arc<crate::spotify::SpotifyClient>>>,
    changes: broadcast::Sender<u64>,
    generation: AtomicU64,
    http: reqwest::Client,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlaybackActivity {
    has_track: bool,
    observed_at: DateTime<Utc>,
}

#[derive(Default)]
struct RenderCache {
    track_id: Option<String>,
    source_bytes: Option<Vec<u8>>,
    palette: Option<[String; 2]>,
    art: HashMap<RenderParams, Art>,
    stats: CacheStats,
}

impl StateHub {
    pub fn new() -> Self {
        Self::with_http(crate::spotify::http_client())
    }

    pub fn with_http(http: reqwest::Client) -> Self {
        let idle = PlaybackObservation::idle(Utc::now());
        let params = RenderParams::default();
        let document = Arc::new(document_from_observation(&idle, None, default_palette()));
        let (changes, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (playback_activity, _) = watch::channel(PlaybackActivity {
            has_track: false,
            observed_at: idle.observed_at,
        });
        Self {
            published: RwLock::new(idle),
            documents: RwLock::new(HashMap::from([(params, document)])),
            registered: RwLock::new(HashSet::from([params])),
            render_cache: Mutex::new(RenderCache::default()),
            last_accent: RwLock::new(None),
            idle_frame_at: RwLock::new(None),
            operation_guard: Mutex::new(()),
            voice_log: Mutex::new(VecDeque::with_capacity(VOICE_LOG_CAPACITY)),
            playback_activity,
            display_offset: Utc.fix(),
            keep_alive: DEFAULT_SSE_KEEP_ALIVE,
            lyrics: None,
            taste: RwLock::new(None),
            beats: RwLock::new(HashMap::new()),
            spotify_features: RwLock::new(None),
            changes,
            generation: AtomicU64::new(0),
            http,
        }
    }

    /// Attach a lyrics source. Absent in tests, which then publish no lyrics.
    #[must_use]
    pub fn with_lyrics(mut self, lyrics: Arc<LyricsIndex>) -> Self {
        self.lyrics = Some(lyrics);
        self
    }

    /// Attach the taste index, so documents can say whether the playing track
    /// is the listener's own.
    pub async fn attach_taste(&self, taste: Arc<crate::taste::TasteIndex>) {
        *self.taste.write().await = Some(taste);
    }

    /// Attach the Spotify client used to ask for tempo and energy. Absent in
    /// tests, which then publish documents without a beat.
    pub async fn attach_features(&self, spotify: Arc<crate::spotify::SpotifyClient>) {
        *self.spotify_features.write().await = Some(spotify);
    }

    /// Tempo and energy for the playing track, cached per track id, including
    /// the misses: the endpoint is deprecated for newer API apps, and asking
    /// again every poll would be pure waste.
    async fn beat_for(&self, observation: &PlaybackObservation) -> Option<(f32, f32)> {
        let track_id = observation.track_id.as_deref()?;
        if let Some(cached) = self.beats.read().await.get(track_id) {
            return *cached;
        }
        let spotify = self.spotify_features.read().await.clone()?;
        let features = spotify.audio_features(track_id).await;
        let mut beats = self.beats.write().await;
        if beats.len() >= 256 {
            beats.clear();
        }
        beats.insert(track_id.to_string(), features);
        features
    }

    /// How often the SSE stream should send a keep-alive comment.
    pub fn keep_alive(&self) -> std::time::Duration {
        self.keep_alive
    }

    /// Shorten the keep-alive. Tests use this so proving the comment arrives
    /// does not cost the real interval in wall-clock time.
    #[must_use]
    pub fn with_keep_alive(mut self, interval: std::time::Duration) -> Self {
        self.keep_alive = interval;
        self
    }

    /// Localize the idle clock's digits. Scheduling stays on UTC boundaries.
    #[must_use]
    pub fn with_display_offset(mut self, offset: FixedOffset) -> Self {
        self.display_offset = offset;
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.changes.receiver_count()
    }

    pub async fn current_document(&self, params: RenderParams) -> Result<Arc<RenderDoc>, AppError> {
        {
            let mut registered = self.registered.write().await;
            if !registered.contains(&params) && registered.len() >= MAX_RENDER_VARIANTS {
                registered.clear();
            }
            registered.insert(params);
        }
        if let Some(document) = self.documents.read().await.get(&params).cloned() {
            return Ok(document);
        }

        let _guard = self.operation_guard.lock().await;
        if let Some(document) = self.documents.read().await.get(&params).cloned() {
            return Ok(document);
        }
        let observation = self.published.read().await.clone();
        let document = Arc::new(self.build_document(&observation, params).await?);
        let mut documents = self.documents.write().await;
        if !documents.contains_key(&params) && documents.len() >= MAX_RENDER_VARIANTS {
            documents.clear();
        }
        documents.insert(params, document.clone());
        Ok(document)
    }

    pub async fn publish_if_meaningful(
        &self,
        observation: PlaybackObservation,
    ) -> Result<bool, AppError> {
        self.publish(observation, false).await
    }

    /// Hook for voice commands, which are meaningful even without a Spotify transition.
    pub async fn force_publish(&self, observation: PlaybackObservation) -> Result<bool, AppError> {
        self.publish(observation, true).await
    }

    /// Broadcast the current document in its transient voice-processing state.
    pub async fn publish_thinking(&self) {
        let _guard = self.operation_guard.lock().await;
        let now = Utc::now();
        let mut documents = self.documents.write().await;
        for document in documents.values_mut() {
            let mut thinking = document.as_ref().clone();
            thinking.state = PlaybackState::Thinking;
            thinking.server_ts = Some(now);
            *document = Arc::new(thinking);
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _send_result = self.changes.send(generation);
    }

    pub async fn append_voice_log(&self, entry: VoiceLogEntry) {
        let mut log = self.voice_log.lock().await;
        log.push_front(entry);
        log.truncate(VOICE_LOG_CAPACITY);
    }

    pub async fn published_observation(&self) -> PlaybackObservation {
        self.published.read().await.clone()
    }

    pub async fn cache_stats(&self) -> CacheStats {
        self.render_cache.lock().await.stats
    }

    /// Publish a minute-aligned clock if playback is still idle.
    pub async fn publish_idle_frame(&self, at: DateTime<Utc>) -> Result<bool, AppError> {
        let _guard = self.operation_guard.lock().await;
        if self.published.read().await.track_id.is_some() {
            return Ok(false);
        }
        *self.idle_frame_at.write().await = Some(at);
        if self.changes.receiver_count() == 0 {
            // Nobody is listening: skip the render and the broadcast, but stay
            // in clock mode so ticking continues. Clear the cached documents so
            // a later subscriber builds a frame for its own current minute
            // rather than being served this skipped one.
            let mut registered = self.registered.write().await;
            registered.clear();
            registered.insert(RenderParams::default());
            drop(registered);
            self.documents.write().await.clear();
            *self.published.write().await = PlaybackObservation::idle(at);
            return Ok(true);
        }
        let params: Vec<_> = self.registered.read().await.iter().copied().collect();
        let palette = self.idle_palette().await;
        let voice_log: Vec<_> = self.voice_log.lock().await.iter().cloned().collect();
        let observation = PlaybackObservation::idle(at);
        let documents = params
            .into_iter()
            .map(|params| {
                let art = idle::render(at, self.display_offset, params);
                let mut document =
                    document_from_observation(&observation, Some(art), palette.clone());
                document.voice_log.clone_from(&voice_log);
                (params, Arc::new(document))
            })
            .collect();

        *self.published.write().await = observation;
        *self.idle_frame_at.write().await = Some(at);
        *self.documents.write().await = documents;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _send_result = self.changes.send(generation);
        Ok(true)
    }

    async fn publish(
        &self,
        observation: PlaybackObservation,
        force: bool,
    ) -> Result<bool, AppError> {
        let _guard = self.operation_guard.lock().await;
        let previous = self.published.read().await.clone();
        if !force && !is_meaningful_change(&previous, &observation) {
            return Ok(false);
        }

        *self.idle_frame_at.write().await = None;

        if previous.track_id != observation.track_id {
            self.render_cache
                .lock()
                .await
                .reset(observation.track_id.clone());
        }
        // Registrations accumulate as clients connect and are never removed
        // individually, so with nobody listening drop back to the default
        // variant instead of re-dithering art for clients that have gone.
        if self.changes.receiver_count() == 0 {
            let mut registered = self.registered.write().await;
            registered.clear();
            registered.insert(RenderParams::default());
        }
        let params: Vec<_> = self.registered.read().await.iter().copied().collect();
        let mut documents = HashMap::with_capacity(params.len());
        for params in params {
            let document = Arc::new(self.build_document(&observation, params).await?);
            documents.insert(params, document);
        }

        *self.published.write().await = observation;
        *self.documents.write().await = documents;
        let published = self.published.read().await;
        self.playback_activity.send_replace(PlaybackActivity {
            has_track: published.track_id.is_some(),
            observed_at: published.observed_at,
        });
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _send_result = self.changes.send(generation);
        Ok(true)
    }

    async fn build_document(
        &self,
        observation: &PlaybackObservation,
        params: RenderParams,
    ) -> Result<RenderDoc, AppError> {
        let mut document = self
            .build_document_without_voice_log(observation, params)
            .await?;
        document.voice_log = self.voice_log.lock().await.iter().cloned().collect();
        // Two independent lookups; serializing them doubled the cold-track cost.
        let (lyrics, beat) = tokio::join!(self.lyrics_for(observation), self.beat_for(observation));
        document.lyrics = lyrics;
        if let Some((tempo, energy)) = beat {
            document.tempo_bpm = Some(tempo);
            document.energy = Some(energy);
        }
        if let (Some(taste), Some(track_id)) = (
            self.taste.read().await.clone(),
            observation.track_id.as_deref(),
        ) {
            document.in_library = taste.contains(track_id).await;
        }
        Ok(document)
    }

    /// Lyrics for whatever is playing, if a source is attached and has them.
    async fn lyrics_for(&self, observation: &PlaybackObservation) -> Option<crate::lyrics::Lyrics> {
        let index = self.lyrics.as_ref()?;
        let track_id = observation.track_id.as_deref()?;
        let track = observation.track.as_deref()?;
        let artist = observation.artist.as_deref()?;
        let album = observation.album.as_deref().unwrap_or_default();
        index
            .for_track(track_id, track, artist, album, observation.duration_ms)
            .await
    }

    async fn build_document_without_voice_log(
        &self,
        observation: &PlaybackObservation,
        params: RenderParams,
    ) -> Result<RenderDoc, AppError> {
        let Some(track_id) = observation.track_id.as_deref() else {
            // Render the minute that is current now, not the one last
            // published, so a client connecting after a skipped tick still
            // shows the right time.
            let art =
                self.idle_frame_at.read().await.map(|_| {
                    idle::render(floor_to_minute(Utc::now()), self.display_offset, params)
                });
            return Ok(document_from_observation(
                observation,
                art,
                self.idle_palette().await,
            ));
        };
        let Some(art_url) = observation.art_url.as_deref() else {
            return Ok(document_from_observation(
                observation,
                None,
                default_palette(),
            ));
        };

        let mut cache = self.render_cache.lock().await;
        if cache.track_id.as_deref() != Some(track_id) {
            cache.reset(Some(track_id.to_string()));
        }
        if let Some(art) = cache.art.get(&params).cloned() {
            return Ok(document_from_observation(
                observation,
                Some(art),
                cache.palette.clone().unwrap_or_else(default_palette),
            ));
        }
        match self.render_art(&mut cache, art_url, params).await {
            Ok(art) => Ok(document_from_observation(
                observation,
                Some(art),
                cache.palette.clone().unwrap_or_else(default_palette),
            )),
            Err(error) => {
                // Track, artist and play state need no cover. Failing the whole
                // publish would discard them too, leaving clients on a stale
                // document until the artwork happened to start working again.
                tracing::warn!(
                    %error,
                    track_id,
                    art_url,
                    "failed to render artwork; publishing without art"
                );
                Ok(document_from_observation(
                    observation,
                    None,
                    cache.palette.clone().unwrap_or_else(default_palette),
                ))
            }
        }
    }

    /// Download (once per track) and dither (once per variant) the cover art.
    async fn render_art(
        &self,
        cache: &mut RenderCache,
        art_url: &str,
        params: RenderParams,
    ) -> Result<Art, AppError> {
        if cache.source_bytes.is_none() {
            let bytes = self
                .http
                .get(art_url)
                .send()
                .await
                .context("failed to download Spotify artwork")?
                .error_for_status()
                .context("Spotify artwork returned an error")?
                .bytes()
                .await
                .context("failed to read Spotify artwork")?
                .to_vec();
            cache.source_bytes = Some(bytes);
            cache.stats.downloads += 1;
        }

        let source = cache.source_bytes.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("art cache lost downloaded source bytes"))
        })?;
        let (art, palette) = image::process(source, params.width, params.height, params.dither)
            .context("failed to render Spotify artwork")?;
        cache.stats.dithers += 1;
        if cache.palette.is_none() {
            cache.palette = Some(palette);
        }
        if let Some(accent) = cache.palette.as_ref().map(|palette| palette[1].clone()) {
            *self.last_accent.write().await = Some(accent);
        }
        if !cache.art.contains_key(&params) && cache.art.len() >= MAX_RENDER_VARIANTS {
            cache.art.clear();
        }
        cache.art.insert(params, art.clone());
        Ok(art)
    }

    async fn idle_palette(&self) -> [String; 2] {
        let mut palette = default_palette();
        if let Some(accent) = self.last_accent.read().await.as_ref() {
            palette[1] = accent.clone();
        }
        palette
    }
}

impl Default for StateHub {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderCache {
    fn reset(&mut self, track_id: Option<String>) {
        self.track_id = track_id;
        self.source_bytes = None;
        self.palette = None;
        self.art.clear();
    }
}

/// Run the one shared Spotify polling task until shutdown is requested.
pub async fn run_poll_loop<Fetch, FetchFuture>(
    hub: Arc<StateHub>,
    mut fetch: Fetch,
    mut shutdown: watch::Receiver<bool>,
) where
    Fetch: FnMut() -> FetchFuture,
    FetchFuture: Future<Output = Result<PlaybackObservation, crate::spotify::PlaybackFetchError>>,
{
    let mut error_backoff = MIN_ERROR_BACKOFF;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let delay = match fetch().await {
            Ok(observation) => {
                if let Err(error) = hub.publish_if_meaningful(observation).await {
                    tracing::warn!(%error, "failed to publish Spotify playback state");
                }
                error_backoff = MIN_ERROR_BACKOFF;
                NORMAL_POLL_INTERVAL
            }
            Err(crate::spotify::PlaybackFetchError::RateLimited(retry_after)) => {
                let delay = retry_after.clamp(MIN_ERROR_BACKOFF, MAX_RATE_LIMIT_BACKOFF);
                tracing::warn!(?delay, "Spotify playback polling rate limited");
                delay
            }
            Err(error) => {
                let delay = error_backoff;
                error_backoff = error_backoff.saturating_mul(2).min(MAX_ERROR_BACKOFF);
                tracing::warn!(%error, ?delay, "Spotify playback poll failed; backing off");
                delay
            }
        };

        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Render idle clocks after 30 seconds without a track, aligned to wall-clock minutes.
pub async fn run_idle_scheduler(hub: Arc<StateHub>, shutdown: watch::Receiver<bool>) {
    run_idle_scheduler_with_clock(hub, shutdown, Utc::now).await;
}

async fn run_idle_scheduler_with_clock<Now>(
    hub: Arc<StateHub>,
    mut shutdown: watch::Receiver<bool>,
    now: Now,
) where
    Now: Fn() -> DateTime<Utc>,
{
    let mut activity = hub.playback_activity.subscribe();
    let initial = *activity.borrow();
    let mut next_tick = (!initial.has_track).then(|| {
        ceil_to_minute(initial.observed_at + chrono::Duration::seconds(IDLE_THRESHOLD_SECONDS))
    });

    loop {
        if *shutdown.borrow() {
            return;
        }
        let Some(target) = next_tick else {
            tokio::select! {
                changed = activity.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let update = *activity.borrow();
                    next_tick = (!update.has_track).then(|| {
                        ceil_to_minute(
                            update.observed_at + chrono::Duration::seconds(IDLE_THRESHOLD_SECONDS),
                        )
                    });
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
            continue;
        };

        let delay = wall_delay(target, now());
        tokio::select! {
            () = tokio::time::sleep(delay) => {
                let current_minute = floor_to_minute(now());
                if current_minute < target {
                    next_tick = Some(target);
                    continue;
                }
                match hub.publish_idle_frame(current_minute).await {
                    Ok(true) => next_tick = Some(current_minute + chrono::Duration::minutes(1)),
                    Ok(false) => next_tick = None,
                    Err(error) => {
                        tracing::warn!(%error, "failed to publish idle frame");
                        next_tick = Some(current_minute + chrono::Duration::minutes(1));
                    }
                }
            }
            changed = activity.changed() => {
                if changed.is_err() {
                    return;
                }
                let update = *activity.borrow();
                next_tick = (!update.has_track).then(|| {
                    ceil_to_minute(
                        update.observed_at + chrono::Duration::seconds(IDLE_THRESHOLD_SECONDS),
                    )
                });
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

fn ceil_to_minute(at: DateTime<Utc>) -> DateTime<Utc> {
    let nanos = i64::from(at.timestamp_subsec_nanos());
    let without_nanos = at - chrono::Duration::nanoseconds(nanos);
    let remainder = without_nanos.timestamp().rem_euclid(60);
    if remainder == 0 && nanos == 0 {
        without_nanos
    } else {
        without_nanos + chrono::Duration::seconds(60 - remainder)
    }
}

fn floor_to_minute(at: DateTime<Utc>) -> DateTime<Utc> {
    at - chrono::Duration::nanoseconds(i64::from(at.timestamp_subsec_nanos()))
        - chrono::Duration::seconds(at.timestamp().rem_euclid(60))
}

fn wall_delay(target: DateTime<Utc>, now: DateTime<Utc>) -> std::time::Duration {
    (target - now).to_std().unwrap_or_default()
}

pub fn is_meaningful_change(
    published: &PlaybackObservation,
    observed: &PlaybackObservation,
) -> bool {
    if published.track_id != observed.track_id || published.is_playing != observed.is_playing {
        return true;
    }
    observed
        .progress_ms
        .abs_diff(interpolated_progress(published, observed.observed_at))
        > SEEK_THRESHOLD_MS
}

pub fn interpolated_progress(published: &PlaybackObservation, at: DateTime<Utc>) -> u64 {
    if !published.is_playing {
        return published.progress_ms;
    }
    let elapsed_ms = (at - published.observed_at).num_milliseconds().max(0) as u64;
    published
        .progress_ms
        .saturating_add(elapsed_ms)
        .min(published.duration_ms)
}

fn parse_dimension(value: Option<&String>, name: &str, default: u32) -> Result<u32, AppError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u32>()
        .map_err(|_| AppError::BadRequest(format!("invalid {name}: {value}")))?;
    if !(MIN_RENDER_DIMENSION..=MAX_RENDER_DIMENSION).contains(&parsed) {
        return Err(AppError::BadRequest(format!(
            "{name} must be between {MIN_RENDER_DIMENSION} and {MAX_RENDER_DIMENSION}"
        )));
    }
    Ok(parsed)
}

fn document_from_observation(
    observation: &PlaybackObservation,
    art: Option<Art>,
    palette: [String; 2],
) -> RenderDoc {
    let state = match (&observation.track_id, observation.is_playing) {
        (None, _) => PlaybackState::Idle,
        (Some(_), true) => PlaybackState::Playing,
        (Some(_), false) => PlaybackState::Paused,
    };
    RenderDoc {
        version: RENDER_DOCUMENT_VERSION,
        state,
        server_ts: Some(observation.observed_at),
        track_id: observation.track_id.clone(),
        track: observation.track.clone(),
        artist: observation.artist.clone(),
        album: observation.album.clone(),
        art,
        art_url: observation.art_url.clone(),
        palette: palette.into(),
        progress_ms: observation.progress_ms,
        duration_ms: observation.duration_ms,
        voice_log: Vec::new(),
        lyrics: None,
        tempo_bpm: None,
        energy: None,
        in_library: false,
    }
}

fn default_palette() -> [String; 2] {
    DEFAULT_PALETTE.map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::Cursor,
        sync::atomic::{AtomicI64, AtomicUsize, Ordering as AtomicOrdering},
    };

    use ::image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use axum::{Router, body::Body, response::IntoResponse, routing::get};
    use base64::{Engine as _, engine::general_purpose};
    use chrono::{Duration, TimeZone};

    use super::*;

    #[tokio::test]
    async fn voice_log_is_newest_first_and_evicts_the_sixth_oldest_entry() {
        let hub = StateHub::new();
        let started = Utc::now();
        for index in 0..6 {
            hub.append_voice_log(VoiceLogEntry {
                transcript: format!("command-{index}"),
                action: format!("spotify:test:{index}"),
                timestamp: started + Duration::seconds(index),
            })
            .await;
        }
        hub.force_publish(PlaybackObservation::idle(Utc::now()))
            .await
            .expect("publish voice log");

        let document = hub
            .current_document(RenderParams::default())
            .await
            .expect("current document");
        assert_eq!(document.voice_log.len(), VOICE_LOG_CAPACITY);
        assert_eq!(document.voice_log[0].transcript, "command-5");
        assert_eq!(document.voice_log[4].transcript, "command-1");
        assert!(
            document
                .voice_log
                .iter()
                .all(|entry| entry.transcript != "command-0")
        );
    }

    #[tokio::test]
    async fn thinking_is_broadcast_without_replacing_playback_observation() {
        let hub = StateHub::new();
        let before = hub.published_observation().await;
        let mut receiver = hub.subscribe();

        hub.publish_thinking().await;
        receiver.recv().await.expect("thinking generation");

        let document = hub
            .current_document(RenderParams::default())
            .await
            .expect("thinking document");
        assert_eq!(document.state, PlaybackState::Thinking);
        assert_eq!(hub.published_observation().await, before);
    }

    #[test]
    fn render_params_apply_defaults_and_validate_bounds() {
        assert_eq!(
            RenderParams::from_query(&HashMap::new()).unwrap(),
            RenderParams::default()
        );
        let valid = HashMap::from([
            ("w".to_string(), "16".to_string()),
            ("h".to_string(), "1024".to_string()),
            ("dither".to_string(), "atkinson".to_string()),
        ]);
        assert_eq!(
            RenderParams::from_query(&valid).unwrap(),
            RenderParams {
                width: 16,
                height: 1024,
                dither: DitherMode::Atkinson,
            }
        );
        for invalid in [
            HashMap::from([("w".to_string(), "15".to_string())]),
            HashMap::from([("h".to_string(), "1025".to_string())]),
            HashMap::from([("w".to_string(), "nope".to_string())]),
            HashMap::from([("dither".to_string(), "floyd".to_string())]),
        ] {
            assert!(RenderParams::from_query(&invalid).is_err());
        }
    }

    #[test]
    fn meaningful_change_detects_track_playback_and_seek_only() {
        let start = Utc::now();
        let published = observation(start, "track-a", true, 10_000, None);
        let steady = observation(start + Duration::seconds(2), "track-a", true, 12_000, None);
        assert!(!is_meaningful_change(&published, &steady));
        let boundary = observation(start + Duration::seconds(2), "track-a", true, 14_000, None);
        assert!(!is_meaningful_change(&published, &boundary));
        let seek = observation(start + Duration::seconds(2), "track-a", true, 14_001, None);
        assert!(is_meaningful_change(&published, &seek));
        let paused = observation(start + Duration::seconds(2), "track-a", false, 12_000, None);
        assert!(is_meaningful_change(&published, &paused));
        let changed = observation(start + Duration::seconds(2), "track-b", true, 12_000, None);
        assert!(is_meaningful_change(&published, &changed));
    }

    #[tokio::test]
    async fn document_preserves_poll_timestamp_and_progress() {
        let hub = StateHub::new();
        let at = Utc::now();
        let observation = observation(at, "track", true, 12_345, None);
        hub.force_publish(observation).await.unwrap();
        let document = hub.current_document(RenderParams::default()).await.unwrap();
        assert_eq!(document.server_ts, Some(at));
        assert_eq!(document.progress_ms, 12_345);
    }

    #[tokio::test]
    async fn steady_progress_is_silent_but_track_and_seek_publish_once() {
        let hub = StateHub::new();
        let mut receiver = hub.subscribe();
        let start = Utc::now();
        assert!(
            hub.publish_if_meaningful(observation(start, "track", true, 0, None))
                .await
                .unwrap()
        );
        receiver.recv().await.unwrap();

        assert!(
            !hub.publish_if_meaningful(observation(
                start + Duration::seconds(2),
                "track",
                true,
                2_000,
                None,
            ))
            .await
            .unwrap()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        assert!(
            hub.publish_if_meaningful(observation(
                start + Duration::seconds(2),
                "track",
                true,
                4_001,
                None,
            ))
            .await
            .unwrap()
        );
        receiver.recv().await.unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn render_cache_deduplicates_same_params_and_sizes_distinct_params() {
        let (art_url, downloads) = spawn_art_server().await;
        let hub = StateHub::new();
        hub.force_publish(observation(Utc::now(), "track", true, 0, Some(art_url)))
            .await
            .unwrap();
        let defaults = RenderParams::default();
        let first = hub.current_document(defaults).await.unwrap();
        let second = hub.current_document(defaults).await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            hub.cache_stats().await,
            CacheStats {
                downloads: 1,
                dithers: 1
            }
        );

        let small = RenderParams {
            width: 16,
            height: 16,
            dither: DitherMode::Atkinson,
        };
        let document = hub.current_document(small).await.unwrap();
        let packed = general_purpose::STANDARD
            .decode(document.art.as_ref().unwrap().bits.as_bytes())
            .unwrap();
        assert_eq!(packed.len(), 32);
        assert_eq!(
            hub.cache_stats().await,
            CacheStats {
                downloads: 1,
                dithers: 2
            }
        );
        assert_eq!(downloads.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    async fn broadcast_fans_out_to_two_subscribers() {
        let hub = StateHub::new();
        let mut first = hub.subscribe();
        let mut second = hub.subscribe();
        hub.force_publish(PlaybackObservation::idle(Utc::now()))
            .await
            .unwrap();
        assert_eq!(first.recv().await.unwrap(), second.recv().await.unwrap());
        let first_doc = hub.current_document(RenderParams::default()).await.unwrap();
        let second_doc = hub.current_document(RenderParams::default()).await.unwrap();
        assert!(Arc::ptr_eq(&first_doc, &second_doc));
    }

    #[tokio::test(start_paused = true)]
    async fn poll_loop_survives_errors_and_respects_retry_after() {
        let hub = Arc::new(StateHub::new());
        let results = Arc::new(Mutex::new(VecDeque::from([
            Err(crate::spotify::PlaybackFetchError::RateLimited(
                std::time::Duration::from_secs(7),
            )),
            Err(crate::spotify::PlaybackFetchError::Transient(
                "temporary".to_string(),
            )),
            Ok(PlaybackObservation::idle(Utc::now())),
        ])));
        let calls = Arc::new(AtomicUsize::new(0));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_poll_loop(
            hub,
            {
                let results = results.clone();
                let calls = calls.clone();
                move || {
                    let results = results.clone();
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, AtomicOrdering::SeqCst);
                        results
                            .lock()
                            .await
                            .pop_front()
                            .unwrap_or_else(|| Ok(PlaybackObservation::idle(Utc::now())))
                    }
                }
            },
            shutdown_rx,
        ));
        tokio::task::yield_now().await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        tokio::time::advance(std::time::Duration::from_secs(6)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 3);

        shutdown_tx.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn art_failure_publishes_document_without_art() {
        let art_url = spawn_failing_art_server(false).await;
        let hub = Arc::new(StateHub::new());
        let mut receiver = hub.subscribe();
        let start = Utc::now();

        let published = hub
            .publish_if_meaningful(observation(start, "track-a", true, 10_000, Some(art_url)))
            .await
            .expect("art failure must not fail the publish");
        assert!(published);
        assert!(receiver.try_recv().is_ok(), "the change must be broadcast");

        // Metadata still reaches clients; only the cover is missing.
        let document = hub
            .current_document(RenderParams::default())
            .await
            .expect("document");
        assert_eq!(document.track_id.as_deref(), Some("track-a"));
        assert_eq!(document.track.as_deref(), Some("Track"));
        assert!(matches!(document.state, PlaybackState::Playing));
        assert!(document.art.is_none());
        assert_eq!(
            hub.published_observation().await.track_id.as_deref(),
            Some("track-a")
        );
    }

    #[tokio::test]
    async fn art_download_timeout_does_not_hang_publish() {
        // A peer that accepts and never answers. The injected client mirrors the
        // production one from spotify::http_client, with a short budget so the
        // test does not wait out the real timeout.
        let art_url = spawn_failing_art_server(true).await;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(150))
            .connect_timeout(std::time::Duration::from_millis(150))
            .build()
            .expect("client");
        let hub = Arc::new(StateHub::with_http(http));
        let start = Utc::now();

        let published = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            hub.publish_if_meaningful(observation(start, "track-a", true, 10_000, Some(art_url))),
        )
        .await
        .expect("publish must not hang on a silent peer")
        .expect("publish");
        assert!(published);
        let document = hub
            .current_document(RenderParams::default())
            .await
            .expect("document");
        assert_eq!(document.track_id.as_deref(), Some("track-a"));
        assert!(document.art.is_none());
    }

    #[tokio::test]
    async fn registered_variants_are_pruned_when_no_subscribers_remain() {
        let (art_url, _downloads) = spawn_art_server().await;
        let hub = Arc::new(StateHub::new());

        {
            let _receiver = hub.subscribe();
            for dimension in [16, 32] {
                hub.current_document(RenderParams {
                    width: dimension,
                    height: dimension,
                    dither: DitherMode::Bayer,
                })
                .await
                .expect("document");
            }
        }
        assert_eq!(hub.subscriber_count(), 0);

        hub.publish_if_meaningful(observation(Utc::now(), "track-a", true, 0, Some(art_url)))
            .await
            .expect("publish");

        // Only the default variant is rebuilt; the two departed clients no
        // longer cost a dither on every publish.
        assert_eq!(hub.cache_stats().await.dithers, 1);
    }

    #[tokio::test]
    async fn idle_publish_without_subscribers_skips_broadcast_but_keeps_ticking() {
        let hub = StateHub::new();
        assert_eq!(hub.subscriber_count(), 0);
        let at = Utc.with_ymd_and_hms(2026, 8, 21, 9, 41, 0).unwrap();

        // Ok(true) keeps the scheduler ticking; a false would strand it waiting
        // for a playback change that never comes while idle.
        assert!(hub.publish_idle_frame(at).await.unwrap());

        // A client connecting later gets a frame for the minute that is current
        // now, not the skipped one.
        let document = hub.current_document(RenderParams::default()).await.unwrap();
        let art = document.art.as_ref().expect("idle art");
        assert_ne!(
            art.bits,
            idle::render(at, Utc.fix(), RenderParams::default()).bits
        );
    }

    #[tokio::test]
    async fn idle_frames_follow_registered_device_params() {
        let hub = StateHub::new();
        let _receiver = hub.subscribe();
        let wide = RenderParams {
            width: 296,
            height: 128,
            dither: DitherMode::Atkinson,
        };
        hub.current_document(wide).await.unwrap();
        let at = Utc.with_ymd_and_hms(2026, 8, 21, 9, 41, 0).unwrap();

        assert!(hub.publish_idle_frame(at).await.unwrap());

        let square = hub.current_document(RenderParams::default()).await.unwrap();
        let wide_document = hub.current_document(wide).await.unwrap();
        let square_art = square.art.as_ref().unwrap();
        let wide_art = wide_document.art.as_ref().unwrap();
        assert_eq!((square_art.w, square_art.h), (400, 400));
        assert_eq!(square_art.dither, DitherMode::Bayer);
        assert_eq!((wide_art.w, wide_art.h), (296, 128));
        assert_eq!(wide_art.dither, DitherMode::Atkinson);
    }

    #[tokio::test]
    async fn idle_palette_keeps_last_rendered_accent() {
        let (art_url, _downloads) = spawn_art_server().await;
        let hub = StateHub::new();
        let at = Utc.with_ymd_and_hms(2026, 8, 21, 9, 41, 0).unwrap();
        hub.force_publish(observation(at, "track", true, 0, Some(art_url)))
            .await
            .unwrap();
        let track_document = hub.current_document(RenderParams::default()).await.unwrap();
        let accent = track_document.palette[1].clone();

        hub.force_publish(PlaybackObservation::idle(at + Duration::seconds(1)))
            .await
            .unwrap();
        hub.publish_idle_frame(at + Duration::minutes(1))
            .await
            .unwrap();
        let idle_document = hub.current_document(RenderParams::default()).await.unwrap();

        assert_eq!(idle_document.palette, ["#1a1a1a", accent.as_str()]);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_scheduler_emits_once_per_minute_then_stops_on_play() {
        let hub = Arc::new(StateHub::new());
        let start = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 20).unwrap();
        hub.force_publish(PlaybackObservation::idle(start))
            .await
            .unwrap();
        let mut receiver = hub.subscribe();
        let wall_seconds = Arc::new(AtomicI64::new(start.timestamp()));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let scheduler = tokio::spawn(run_idle_scheduler_with_clock(hub.clone(), shutdown_rx, {
            let wall_seconds = wall_seconds.clone();
            move || {
                Utc.timestamp_opt(wall_seconds.load(AtomicOrdering::SeqCst), 0)
                    .unwrap()
            }
        }));
        tokio::task::yield_now().await;

        tokio::time::advance(std::time::Duration::from_secs(39)).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        wall_seconds.store(
            (start + Duration::seconds(40)).timestamp(),
            AtomicOrdering::SeqCst,
        );
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        receiver.try_recv().expect("first minute boundary");
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(
            hub.current_document(RenderParams::default())
                .await
                .unwrap()
                .server_ts,
            Some(start + Duration::seconds(40))
        );

        wall_seconds.store(
            (start + Duration::seconds(100)).timestamp(),
            AtomicOrdering::SeqCst,
        );
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        receiver.try_recv().expect("second minute boundary");
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        hub.force_publish(observation(
            start + Duration::seconds(101),
            "track",
            true,
            0,
            None,
        ))
        .await
        .unwrap();
        receiver.try_recv().expect("play event");
        tokio::task::yield_now().await;
        wall_seconds.store(
            (start + Duration::seconds(160)).timestamp(),
            AtomicOrdering::SeqCst,
        );
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        shutdown_tx.send(true).unwrap();
        scheduler.await.unwrap();
    }

    async fn spawn_failing_art_server(hang: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        if hang {
            tokio::spawn(async move {
                let mut accepted = Vec::new();
                while let Ok((socket, _)) = listener.accept().await {
                    accepted.push(socket);
                }
            });
        } else {
            let app = Router::new().route(
                "/art.png",
                get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
            );
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        }
        format!("http://{address}/art.png")
    }

    fn observation(
        observed_at: DateTime<Utc>,
        track_id: &str,
        is_playing: bool,
        progress_ms: u64,
        art_url: Option<String>,
    ) -> PlaybackObservation {
        PlaybackObservation {
            observed_at,
            track_id: Some(track_id.to_string()),
            track: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            art_url,
            is_playing,
            progress_ms,
            duration_ms: 300_000,
        }
    }

    async fn spawn_art_server() -> (String, Arc<AtomicUsize>) {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_fn(32, 32, |x, y| {
            Rgb([(x * 7) as u8, (y * 7) as u8, ((x + y) * 3) as u8])
        }))
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
        let bytes = Arc::new(encoded.into_inner());
        let downloads = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/art.png",
            get({
                let bytes = bytes.clone();
                let downloads = downloads.clone();
                move || {
                    let bytes = bytes.clone();
                    let downloads = downloads.clone();
                    async move {
                        downloads.fetch_add(1, AtomicOrdering::SeqCst);
                        Body::from(bytes.as_ref().clone()).into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/art.png"), downloads)
    }
}
