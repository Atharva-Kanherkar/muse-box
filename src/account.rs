//! One Spotify login, one isolated runtime.
//!
//! muse-box started as a single-tenant appliance: one `SpotifyClient`, one
//! `StateHub`, one `TasteIndex`, all constructed once at boot. Opening it to
//! strangers means each of those becomes per-account instead of per-process,
//! resolved lazily so an account nobody is currently looking at costs nothing
//! but the disk space its token and taste index occupy.
//!
//! `AccountId` is the Spotify user id, learned once via `GET /v1/me` right
//! after OAuth exchanges its code — Spotify already hands out a stable,
//! unique identity, so nothing here invents its own.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, FixedOffset, Utc};
use tokio::sync::{Mutex, RwLock, watch};

use crate::{
    error::AppError,
    lyrics::LyricsIndex,
    spotify::{SpotifyClient, SpotifyConfig},
    state::{StateHub, run_idle_scheduler, run_poll_loop},
    taste::TasteIndex,
};

/// How often the reaper sweeps for accounts nobody is using.
const REAP_SWEEP_INTERVAL: StdDuration = StdDuration::from_secs(60);
/// How long an account may sit with no SSE subscriber before its runtime is
/// torn down. On-disk token and taste index survive; the next visit rebuilds
/// a fresh poll loop from them.
const IDLE_GRACE: Duration = Duration::minutes(10);

/// A Spotify user id. Stable and unique, so it is the whole identity scheme.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct AccountId(String);

impl AccountId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything one signed-in person needs, isolated from every other account.
pub struct AccountRuntime {
    pub(crate) id: AccountId,
    pub(crate) spotify: SpotifyClient,
    pub(crate) hub: Arc<StateHub>,
    pub(crate) taste: Arc<TasteIndex>,
    /// Serializes this account's voice/control commands. One per account, so
    /// one person's in-flight command never blocks another's. `Arc`-wrapped
    /// so a handler can hold it alongside its own cheap clones of the other
    /// fields without borrowing the whole runtime.
    pub(crate) voice_guard: Arc<Mutex<()>>,
    last_active: RwLock<DateTime<Utc>>,
    shutdown: watch::Sender<bool>,
}

impl AccountRuntime {
    async fn spawn(id: AccountId, config: &RegistryConfig) -> Arc<Self> {
        let spotify = SpotifyClient::new(SpotifyConfig {
            client_id: config.spotify_client_id.clone(),
            client_secret: config.spotify_client_secret.clone(),
            redirect_uri: config.spotify_redirect_uri.clone(),
            token_store_path: token_path(&config.accounts_root, &id),
        });
        // Whether a token was already on disk matters below: a brand new
        // account (created by the OAuth callback for the very first time)
        // has none yet, and starting the one-shot taste build before the
        // callback adopts its tokens would race it — see
        // `kick_off_taste_build`'s doc comment.
        let had_stored_token = match spotify.initialize_from_store().await {
            Ok(had_token) => {
                if had_token {
                    tracing::info!(account = %id, "restored Spotify authorization");
                } else {
                    tracing::warn!(account = %id, "no persisted Spotify authorization for account");
                }
                had_token
            }
            Err(error) => {
                tracing::warn!(%error, account = %id, "could not restore Spotify authorization");
                false
            }
        };

        let hub = Arc::new(
            StateHub::new()
                .with_display_offset(config.idle_display_offset)
                .with_lyrics(config.lyrics.clone()),
        );
        hub.attach_features(Arc::new(spotify.clone())).await;

        let taste = Arc::new(TasteIndex::new(
            config.openai_api_key.clone(),
            taste_path(&config.accounts_root, &id),
        ));
        hub.attach_taste(taste.clone()).await;

        let (shutdown, poll_shutdown_rx) = watch::channel(false);
        let idle_shutdown_rx = poll_shutdown_rx.clone();
        let poll_hub = hub.clone();
        let poll_spotify = spotify.clone();
        tokio::spawn(run_poll_loop(
            poll_hub,
            move || {
                let spotify = poll_spotify.clone();
                async move { spotify.currently_playing().await }
            },
            poll_shutdown_rx,
        ));
        tokio::spawn(run_idle_scheduler(hub.clone(), idle_shutdown_rx));

        let runtime = Arc::new(Self {
            id,
            spotify,
            hub,
            taste,
            voice_guard: Arc::new(Mutex::new(())),
            last_active: RwLock::new(Utc::now()),
            shutdown,
        });
        // A brand new account has nothing to embed yet: the OAuth callback
        // that is about to adopt its tokens calls `kick_off_taste_build`
        // itself once that finishes. Only an account that already had a
        // persisted token gets this for free here.
        if had_stored_token {
            runtime.kick_off_taste_build();
        }
        runtime
    }

    /// Build (or rebuild) the taste index from this account's Spotify
    /// library, in the background.
    ///
    /// Called automatically by `spawn` for an account that already had a
    /// persisted token. A brand new account has none at construction time —
    /// the OAuth callback derives the account id, resolves (constructs) its
    /// runtime, *then* adopts its tokens — so `spawn` cannot safely start
    /// this for a new account: it would race the callback's token adoption,
    /// almost always lose (library_tracks needs a token that does not exist
    /// yet), and, since this only ever runs once, leave the taste index
    /// permanently empty. The callback calls this explicitly right after
    /// adopting a new account's tokens instead.
    pub(crate) fn kick_off_taste_build(&self) {
        let taste_builder = self.taste.clone();
        let taste_spotify = self.spotify.clone();
        let taste_id = self.id.clone();
        tokio::spawn(async move {
            match taste_builder.load().await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, account = %taste_id, "could not load taste index");
                }
            }
            match taste_spotify
                .library_tracks(crate::taste::MAX_INDEXED_TRACKS)
                .await
            {
                Ok(library) if library.is_empty() => {
                    tracing::info!(account = %taste_id, "Spotify returned no library to index");
                }
                Ok(library) => {
                    tracing::info!(account = %taste_id, tracks = library.len(), "embedding music library");
                    if let Err(error) = taste_builder.rebuild(library).await {
                        tracing::warn!(%error, account = %taste_id, "could not build taste index");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, account = %taste_id, "could not read Spotify library");
                }
            }
        });
    }

    async fn touch(&self, now: DateTime<Utc>) {
        *self.last_active.write().await = now;
    }

    #[cfg(test)]
    pub(crate) async fn backdate_for_test(&self, when: DateTime<Utc>) {
        *self.last_active.write().await = when;
    }

    /// Assemble an already-built runtime directly, bypassing
    /// [`AccountRuntime::spawn`]'s disk reads and background tasks — what
    /// tests want, since they hand in fixtures already pointed at mock
    /// servers.
    #[cfg(test)]
    pub(crate) fn for_test(
        id: AccountId,
        spotify: SpotifyClient,
        hub: Arc<StateHub>,
        taste: Arc<TasteIndex>,
    ) -> Arc<Self> {
        let (shutdown, _unwatched) = watch::channel(false);
        Arc::new(Self {
            id,
            spotify,
            hub,
            taste,
            voice_guard: Arc::new(Mutex::new(())),
            last_active: RwLock::new(Utc::now()),
            shutdown,
        })
    }
}

fn account_dir(accounts_root: &Path, id: &AccountId) -> PathBuf {
    accounts_root.join(id.as_str())
}

fn token_path(accounts_root: &Path, id: &AccountId) -> PathBuf {
    account_dir(accounts_root, id).join("spotify_token.json")
}

fn taste_path(accounts_root: &Path, id: &AccountId) -> PathBuf {
    account_dir(accounts_root, id).join("taste_index.json")
}

/// Shared dependencies every account's runtime is built from.
#[derive(Clone)]
pub struct RegistryConfig {
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    pub spotify_redirect_uri: String,
    pub openai_api_key: String,
    pub lyrics: Arc<LyricsIndex>,
    pub idle_display_offset: FixedOffset,
    pub accounts_root: PathBuf,
}

#[cfg(test)]
impl RegistryConfig {
    /// Paths nobody in a test actually reads from or writes to: tests that
    /// need real disk state pre-seed an `AccountRuntime` directly instead of
    /// exercising [`AccountRuntime::spawn`].
    pub(crate) fn for_test() -> Self {
        Self {
            spotify_client_id: "client-id".to_string(),
            spotify_client_secret: "client-secret".to_string(),
            spotify_redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
            openai_api_key: "test-key".to_string(),
            lyrics: Arc::new(LyricsIndex::new(PathBuf::from("unused"))),
            idle_display_offset: FixedOffset::east_opt(0).expect("UTC offset"),
            accounts_root: PathBuf::from("unused"),
        }
    }
}

/// Resolves an account's runtime lazily, and reaps the ones nobody is using.
pub struct AccountRegistry {
    config: RegistryConfig,
    accounts: RwLock<HashMap<AccountId, Arc<AccountRuntime>>>,
}

impl AccountRegistry {
    pub fn new(config: RegistryConfig) -> Self {
        Self {
            config,
            accounts: RwLock::new(HashMap::new()),
        }
    }

    /// Resolve an account's runtime, building it on first use. Cheap on every
    /// call after the first: a read-lock hit and a timestamp write.
    pub async fn get_or_create(&self, id: &AccountId) -> Arc<AccountRuntime> {
        if let Some(existing) = self.accounts.read().await.get(id) {
            existing.touch(Utc::now()).await;
            return existing.clone();
        }

        // Built with no lock held: this does real disk I/O and, when a
        // stored token is near expiry, a Spotify token-refresh network call.
        // Holding the registry's write lock across that would stall every
        // other account's credential resolution — including ones with an
        // already-warm runtime, since a queued writer blocks new readers too
        // — for up to the HTTP timeout.
        tracing::info!(account = %id, "warming account runtime");
        let runtime = AccountRuntime::spawn(id.clone(), &self.config).await;

        let mut accounts = self.accounts.write().await;
        // Another request may have built and inserted the same account while
        // this one was still resolving. Keep whichever got there first and
        // shut down the loser's just-spawned poll/idle loops rather than
        // leaking a second, orphaned pair of them for the same account.
        if let Some(existing) = accounts.get(id) {
            existing.touch(Utc::now()).await;
            let _ = runtime.shutdown.send(true);
            return existing.clone();
        }
        accounts.insert(runtime.id.clone(), runtime.clone());
        runtime
    }

    pub async fn len(&self) -> usize {
        self.accounts.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.accounts.read().await.is_empty()
    }

    /// Preload an already-built runtime, so a test can name exactly which
    /// account a mock-pointed fixture resolves to.
    #[cfg(test)]
    pub(crate) async fn insert_for_test(&self, runtime: Arc<AccountRuntime>) {
        self.accounts
            .write()
            .await
            .insert(runtime.id.clone(), runtime);
    }

    /// Tear down any account with no SSE subscriber and no activity for
    /// `idle_after`. Their on-disk token and taste index are untouched, so the
    /// next visit rebuilds cleanly. Returns the ids reaped, for logging.
    pub async fn reap_idle_at(&self, now: DateTime<Utc>, idle_after: Duration) -> Vec<AccountId> {
        let candidates: Vec<AccountId> = {
            let accounts = self.accounts.read().await;
            let mut candidates = Vec::new();
            for (id, runtime) in accounts.iter() {
                if runtime.hub.subscriber_count() > 0 {
                    continue;
                }
                let idle_for = now - *runtime.last_active.read().await;
                if idle_for >= idle_after {
                    candidates.push(id.clone());
                }
            }
            candidates
        };
        if candidates.is_empty() {
            return candidates;
        }

        let mut reaped = Vec::new();
        let mut accounts = self.accounts.write().await;
        for id in candidates {
            // Re-check under the write lock: a request may have touched or
            // resubscribed to this account between the scan above and here.
            let Some(runtime) = accounts.get(&id) else {
                continue;
            };
            if runtime.hub.subscriber_count() > 0 {
                continue;
            }
            let idle_for = now - *runtime.last_active.read().await;
            if idle_for < idle_after {
                continue;
            }
            if let Some(runtime) = accounts.remove(&id) {
                let _ = runtime.shutdown.send(true);
                reaped.push(id);
            }
        }
        reaped
    }

    /// Spawn the periodic reaper. Fire-and-forget, like the poll loops it
    /// eventually shuts down: nothing joins it, and the process exiting is
    /// the only way it stops.
    pub fn spawn_reaper(registry: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_SWEEP_INTERVAL).await;
                for id in registry.reap_idle_at(Utc::now(), IDLE_GRACE).await {
                    tracing::info!(account = %id, "reaped idle account runtime");
                }
            }
        });
    }
}

/// Tracks which account the hardware bearer token resolves to. Whoever
/// completes OAuth first on a fresh install keeps this permanently — later
/// logins from other people never displace them.
pub struct OwnerMarker {
    path: PathBuf,
    current: RwLock<Option<AccountId>>,
}

impl OwnerMarker {
    /// Load the owner marker. A missing file means a fresh install with no
    /// owner yet — the only condition that may read as "unclaimed". Any
    /// other read failure (permissions, a transient volume glitch) must not
    /// collapse to the same thing: that would let the very next login
    /// permanently take the hardware bearer token away from whoever the real
    /// owner already is, on nothing more than a hiccup.
    pub async fn load(path: PathBuf) -> Result<Self, AppError> {
        let current = match tokio::fs::read_to_string(&path).await {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(AccountId::new(trimmed))
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "failed to read owner marker at {}: {error}",
                    path.display()
                )));
            }
        };
        Ok(Self {
            path,
            current: RwLock::new(current),
        })
    }

    pub async fn get(&self) -> Option<AccountId> {
        self.current.read().await.clone()
    }

    /// Start already owned by `id`, with no marker file behind it — what
    /// every test wants, since bearer-authenticated requests need an owner to
    /// resolve to and none of them restart the process to prove persistence.
    #[cfg(test)]
    pub(crate) fn for_test(id: AccountId) -> Self {
        Self {
            path: PathBuf::from("unused"),
            current: RwLock::new(Some(id)),
        }
    }

    /// No owner claimed yet — a fresh install nobody has ever signed into.
    #[cfg(test)]
    pub(crate) fn for_test_unclaimed() -> Self {
        Self {
            path: PathBuf::from("unused"),
            current: RwLock::new(None),
        }
    }

    /// Record `id` as the owner if no owner is set yet. A no-op otherwise.
    pub async fn claim(&self, id: &AccountId) -> Result<(), AppError> {
        let mut current = self.current.write().await;
        if current.is_some() {
            return Ok(());
        }
        write_owner_marker(&self.path, id).await?;
        *current = Some(id.clone());
        Ok(())
    }
}

/// Same temp-file-and-rename discipline as the token and session stores: a
/// half-written marker must never read back as "unclaimed" — that is
/// exactly the state that lets the next login take over permanently.
async fn write_owner_marker(path: &Path, id: &AccountId) -> Result<(), AppError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "failed to create owner marker directory: {error}"
            ))
        })?;
    }
    let mut temporary = path.file_name().unwrap_or_default().to_os_string();
    temporary.push(".tmp");
    let temporary = path.with_file_name(temporary);
    tokio::fs::write(&temporary, id.as_str().as_bytes())
        .await
        .map_err(|error| {
            AppError::Internal(anyhow::anyhow!("failed to write owner marker: {error}"))
        })?;
    tokio::fs::rename(&temporary, path).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!("failed to commit owner marker: {error}"))
    })
}

/// One-time move from the single-tenant layout to `data/accounts/<id>/`.
///
/// A no-op once the legacy token is gone — moved by an earlier successful
/// run, or never there to begin with — which is what actually marks this
/// done. `accounts_root` existing is deliberately *not* that signal: a
/// volume mount can pre-create its mount point, and a prior attempt that
/// created the directory but died before the rename below would otherwise
/// look "migrated" forever while the legacy token sits unmoved. Retrying on
/// every boot until the rename actually succeeds is the safe default; the
/// rename is what stops it, by making the legacy path disappear. `legacy`
/// must already be pointed at `legacy_token_path` — the caller owns
/// constructing it, which is what makes this testable against a mock
/// Spotify server instead of the real one.
pub async fn migrate_legacy_install(
    legacy: &SpotifyClient,
    legacy_token_path: &Path,
    legacy_taste_path: &Path,
    accounts_root: &Path,
    owner: &OwnerMarker,
) -> Result<Option<AccountId>, AppError> {
    if !legacy_token_path.exists() {
        return Ok(None);
    }

    if !legacy.initialize_from_store().await? {
        return Ok(None);
    }
    let id = AccountId::new(legacy.current_user_id().await?);

    let dir = account_dir(accounts_root, &id);
    tokio::fs::create_dir_all(&dir).await.map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "failed to create account directory: {error}"
        ))
    })?;
    tokio::fs::rename(legacy_token_path, token_path(accounts_root, &id))
        .await
        .map_err(|error| {
            AppError::Internal(anyhow::anyhow!("failed to migrate Spotify token: {error}"))
        })?;
    if legacy_taste_path.exists() {
        // Not load-bearing: a taste index rebuilds from Spotify on next boot
        // if this particular move fails, so it does not abort the migration.
        if let Err(error) =
            tokio::fs::rename(legacy_taste_path, taste_path(accounts_root, &id)).await
        {
            tracing::warn!(%error, "could not migrate legacy taste index; it will rebuild");
        }
    }
    owner.claim(&id).await?;
    tracing::info!(account = %id, "migrated legacy single-tenant install");
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry_config(accounts_root: PathBuf) -> RegistryConfig {
        RegistryConfig {
            spotify_client_id: "client-id".to_string(),
            spotify_client_secret: "client-secret".to_string(),
            spotify_redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
            openai_api_key: "test-key".to_string(),
            lyrics: Arc::new(LyricsIndex::new(accounts_root.join("lyrics"))),
            idle_display_offset: FixedOffset::east_opt(0).expect("UTC offset"),
            accounts_root,
        }
    }

    #[tokio::test]
    async fn the_same_account_resolves_to_the_same_runtime() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = AccountRegistry::new(test_registry_config(directory.path().to_path_buf()));
        let id = AccountId::new("listener-1");

        let first = registry.get_or_create(&id).await;
        let second = registry.get_or_create(&id).await;

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn two_accounts_get_two_isolated_runtimes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = AccountRegistry::new(test_registry_config(directory.path().to_path_buf()));

        let a = registry.get_or_create(&AccountId::new("listener-a")).await;
        let b = registry.get_or_create(&AccountId::new("listener-b")).await;

        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(registry.len().await, 2);
    }

    #[tokio::test]
    async fn concurrent_first_resolution_of_the_same_account_converges_on_one_runtime() {
        // get_or_create builds a runtime with no lock held (so one cold
        // account can't stall everyone else's credential resolution), then
        // re-checks under the write lock before inserting. This is what
        // proves that double-check-then-discard path is actually correct,
        // not just fast: every concurrent caller for a never-before-seen
        // account must still converge on exactly one runtime.
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = Arc::new(AccountRegistry::new(test_registry_config(
            directory.path().to_path_buf(),
        )));
        let id = AccountId::new("listener-1");

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            let id = id.clone();
            tasks.push(tokio::spawn(
                async move { registry.get_or_create(&id).await },
            ));
        }
        let mut runtimes = Vec::new();
        for task in tasks {
            runtimes.push(task.await.expect("task"));
        }

        let first = &runtimes[0];
        assert!(
            runtimes.iter().all(|runtime| Arc::ptr_eq(runtime, first)),
            "concurrent first resolutions produced more than one runtime"
        );
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn an_idle_account_with_no_subscriber_is_reaped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = AccountRegistry::new(test_registry_config(directory.path().to_path_buf()));
        let id = AccountId::new("listener-1");
        let runtime = registry.get_or_create(&id).await;

        let long_ago = Utc::now() - Duration::hours(1);
        runtime.backdate_for_test(long_ago).await;

        let reaped = registry.reap_idle_at(Utc::now(), IDLE_GRACE).await;
        assert_eq!(reaped, vec![id]);
        assert_eq!(registry.len().await, 0);
    }

    #[tokio::test]
    async fn an_account_with_a_live_subscriber_is_never_reaped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = AccountRegistry::new(test_registry_config(directory.path().to_path_buf()));
        let id = AccountId::new("listener-1");
        let runtime = registry.get_or_create(&id).await;
        let _subscriber = runtime.hub.subscribe();

        let long_ago = Utc::now() - Duration::hours(1);
        runtime.backdate_for_test(long_ago).await;

        let reaped = registry.reap_idle_at(Utc::now(), IDLE_GRACE).await;
        assert!(reaped.is_empty());
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn an_account_still_inside_the_grace_period_is_not_reaped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = AccountRegistry::new(test_registry_config(directory.path().to_path_buf()));
        let id = AccountId::new("listener-1");
        registry.get_or_create(&id).await;

        let reaped = registry.reap_idle_at(Utc::now(), IDLE_GRACE).await;
        assert!(reaped.is_empty());
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn reaping_and_then_revisiting_builds_a_fresh_runtime() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let registry = AccountRegistry::new(test_registry_config(directory.path().to_path_buf()));
        let id = AccountId::new("listener-1");
        let first = registry.get_or_create(&id).await;
        first
            .backdate_for_test(Utc::now() - Duration::hours(1))
            .await;
        registry.reap_idle_at(Utc::now(), IDLE_GRACE).await;

        let second = registry.get_or_create(&id).await;
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(registry.len().await, 1);
    }

    #[tokio::test]
    async fn an_owner_marker_starts_empty_and_keeps_the_first_claim() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("owner_account_id.txt");
        let owner = OwnerMarker::load(path.clone())
            .await
            .expect("load owner marker");
        assert_eq!(owner.get().await, None);

        let first = AccountId::new("listener-1");
        owner.claim(&first).await.expect("claim owner");
        assert_eq!(owner.get().await, Some(first.clone()));

        // A later login never displaces the first owner.
        owner
            .claim(&AccountId::new("listener-2"))
            .await
            .expect("claim is a no-op");
        assert_eq!(owner.get().await, Some(first));

        let reloaded = OwnerMarker::load(path).await.expect("reload owner marker");
        assert_eq!(reloaded.get().await, Some(AccountId::new("listener-1")));
    }

    #[tokio::test]
    async fn a_missing_owner_marker_file_loads_as_no_owner() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let owner = OwnerMarker::load(directory.path().join("does-not-exist.txt"))
            .await
            .expect("a missing marker file is not a read error");
        assert_eq!(owner.get().await, None);
    }

    #[tokio::test]
    async fn migration_is_a_no_op_when_there_is_no_legacy_token() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let accounts_root = directory.path().join("accounts");
        let owner = OwnerMarker::load(directory.path().join("owner_account_id.txt"))
            .await
            .expect("load owner marker");
        let legacy_token_path = directory.path().join("spotify_token.json");
        let legacy = SpotifyClient::new(SpotifyConfig {
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
            token_store_path: legacy_token_path.clone(),
        });

        let migrated = migrate_legacy_install(
            &legacy,
            &legacy_token_path,
            &directory.path().join("taste_index.json"),
            &accounts_root,
            &owner,
        )
        .await
        .expect("migration does not fail with nothing to migrate");

        assert_eq!(migrated, None);
        assert_eq!(owner.get().await, None);
        assert!(!accounts_root.exists());
    }

    #[tokio::test]
    async fn migration_still_succeeds_even_if_the_accounts_directory_already_exists() {
        // A volume mount can pre-create its mount point, and a prior
        // migration attempt could have created this directory and then died
        // before the rename. Either way, the legacy token is still sitting
        // there unmoved, and migration must still pick it up rather than
        // treating the directory's mere existence as "already done."
        let directory = tempfile::tempdir().expect("temporary directory");
        let accounts_root = directory.path().join("accounts");
        tokio::fs::create_dir_all(&accounts_root)
            .await
            .expect("seed accounts root");
        let legacy_token_path = directory.path().join("spotify_token.json");
        let legacy_taste_path = directory.path().join("taste_index.json");
        tokio::fs::write(&legacy_taste_path, b"{}")
            .await
            .expect("seed legacy taste index");
        let owner = OwnerMarker::load(directory.path().join("owner_account_id.txt"))
            .await
            .expect("load owner marker");

        // initialize_from_store always force-refreshes, so the mock needs a
        // /token route too, not just /me.
        let app = axum::Router::new()
            .route(
                "/me",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({ "id": "legacy-owner" }))
                }),
            )
            .route(
                "/token",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "access_token": "refreshed-access-token",
                        "refresh_token": "refreshed-refresh-token",
                        "expires_in": 3600
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock Spotify");
        let address = listener.local_addr().expect("mock Spotify address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock Spotify");
        });
        let legacy = SpotifyClient::with_all_test_endpoints(
            SpotifyConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: "http://localhost/auth/spotify/callback".to_string(),
                token_store_path: legacy_token_path.clone(),
            },
            format!("http://{address}/authorize"),
            format!("http://{address}/token"),
            format!("http://{address}"),
        );
        // Seed a real, valid token at the legacy path: adopt_tokens persists
        // through this exact client, which is also the `legacy` handed to
        // migrate_legacy_install below.
        legacy
            .adopt_tokens(crate::spotify::ExchangedTokens {
                access_token: "access-token".to_string(),
                refresh_token: "refresh-token".to_string(),
                expires_at: Utc::now() + Duration::hours(1),
            })
            .await
            .expect("seed a valid legacy token");

        let migrated = migrate_legacy_install(
            &legacy,
            &legacy_token_path,
            &legacy_taste_path,
            &accounts_root,
            &owner,
        )
        .await
        .expect("migration succeeds despite the pre-existing accounts directory");

        let id = AccountId::new("legacy-owner");
        assert_eq!(migrated, Some(id.clone()));
        assert!(
            !legacy_token_path.exists(),
            "the legacy token must be moved, not left behind"
        );
        assert!(token_path(&accounts_root, &id).exists());
        assert!(taste_path(&accounts_root, &id).exists());
        assert_eq!(owner.get().await, Some(id));
    }
}
