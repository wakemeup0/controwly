use crate::controller::{DeviceMutationGuard, SharedController};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
#[cfg(not(windows))]
use tauri::Manager;
use tauri::{AppHandle, Emitter, Runtime, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use tokio::sync::Mutex as AsyncMutex;

pub(crate) const UPDATE_STATE_EVENT: &str = "updater-state";

const CANONICAL_UPDATE_ENDPOINT: &str =
    "https://github.com/wakemeup0/controwly/releases/latest/download/latest.json";
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PROGRESS_EVENT_INTERVAL: Duration = Duration::from_millis(100);
const MAX_PUBLIC_KEY_BYTES: usize = 16 * 1024;
const MAX_ERROR_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum UpdateStatus {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Installing,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateState {
    pub(crate) status: UpdateStatus,
    pub(crate) version: Option<String>,
    pub(crate) downloaded: u64,
    pub(crate) total: Option<u64>,
    pub(crate) error: Option<String>,
}

impl UpdateState {
    fn idle() -> Self {
        Self {
            status: UpdateStatus::Idle,
            version: None,
            downloaded: 0,
            total: None,
            error: None,
        }
    }

    fn checking() -> Self {
        Self {
            status: UpdateStatus::Checking,
            version: None,
            downloaded: 0,
            total: None,
            error: None,
        }
    }

    fn current() -> Self {
        Self {
            status: UpdateStatus::Current,
            version: None,
            downloaded: 0,
            total: None,
            error: None,
        }
    }

    fn available(version: String) -> Self {
        Self {
            status: UpdateStatus::Available,
            version: Some(version),
            downloaded: 0,
            total: None,
            error: None,
        }
    }

    fn downloading(version: String, downloaded: u64, total: Option<u64>) -> Self {
        Self {
            status: UpdateStatus::Downloading,
            version: Some(version),
            downloaded,
            total,
            error: None,
        }
    }

    fn installing(version: String, downloaded: u64, total: Option<u64>) -> Self {
        Self {
            status: UpdateStatus::Installing,
            version: Some(version),
            downloaded,
            total,
            error: None,
        }
    }

    fn error(message: String) -> Self {
        Self {
            status: UpdateStatus::Error,
            version: None,
            downloaded: 0,
            total: None,
            error: Some(message),
        }
    }
}

struct UpdaterRuntime {
    state: UpdateState,
    pending: Option<Update>,
}

pub(crate) struct UpdaterState {
    controller: SharedController,
    operation: AsyncMutex<()>,
    runtime: Mutex<UpdaterRuntime>,
}

impl UpdaterState {
    pub(crate) fn new(controller: SharedController) -> Self {
        Self {
            controller,
            operation: AsyncMutex::new(()),
            runtime: Mutex::new(UpdaterRuntime {
                state: UpdateState::idle(),
                pending: None,
            }),
        }
    }

    fn lock_runtime(&self) -> MutexGuard<'_, UpdaterRuntime> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn snapshot(&self) -> UpdateState {
        self.lock_runtime().state.clone()
    }

    fn pending(&self) -> Option<Update> {
        self.lock_runtime().pending.clone()
    }

    fn clear_pending(&self) {
        self.lock_runtime().pending = None;
    }

    fn set_pending(&self, update: Option<Update>) {
        self.lock_runtime().pending = update;
    }

    fn store_state(&self, next: UpdateState) {
        self.lock_runtime().state = next;
    }

    fn emit_state<R: Runtime>(&self, app: &AppHandle<R>, next: UpdateState) -> UpdateState {
        self.store_state(next.clone());
        let _ = app.emit(UPDATE_STATE_EVENT, &next);
        next
    }

    fn set_error<R: Runtime>(&self, app: &AppHandle<R>, message: String) -> String {
        self.clear_pending();
        let message = bounded_text(&message, MAX_ERROR_BYTES);
        self.emit_state(app, UpdateState::error(message.clone()));
        message
    }
}

#[derive(Clone, Copy)]
enum ErrorPhase {
    Check,
    Download,
    Install,
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }

    let mut end = max_bytes.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn busy_error(operation: &str) -> String {
    format!("Another updater operation is already in progress; {operation} was not started")
}

fn plugin_error_message(phase: ErrorPhase, error: &UpdaterError) -> String {
    match error {
        UpdaterError::Minisign(_)
        | UpdaterError::Base64(_)
        | UpdaterError::SignatureUtf8(_) => {
            "Update signature verification failed; the package was not installed. Contact the release maintainer.".to_owned()
        }
        UpdaterError::InsecureTransportProtocol => {
            "Update configuration rejected: the updater endpoint must use HTTPS.".to_owned()
        }
        UpdaterError::Reqwest(_) | UpdaterError::Network(_) => {
            "Update service is unavailable or offline. Check the connection and try again.".to_owned()
        }
        UpdaterError::ReleaseNotFound if matches!(phase, ErrorPhase::Check) => {
            "Update service is unavailable or offline. Check the connection and try again.".to_owned()
        }
        UpdaterError::TargetNotFound(_)
        | UpdaterError::TargetsNotFound(_)
        | UpdaterError::Serialization(_)
        | UpdaterError::Semver(_) => {
            "Update metadata is invalid or has no signed artifact for this platform. Contact the release maintainer.".to_owned()
        }
        UpdaterError::EmptyEndpoints
        | UpdaterError::UnsupportedArch
        | UpdaterError::UnsupportedOs => {
            "Updater is not configured for this build or platform. Use a signed release build.".to_owned()
        }
        UpdaterError::PackageInstallFailed
        | UpdaterError::DebInstallFailed
        | UpdaterError::InvalidUpdaterFormat
        | UpdaterError::AuthenticationFailed
            if matches!(phase, ErrorPhase::Install | ErrorPhase::Download) =>
        {
            "Update installation failed; the current version was kept. Try again later or contact support.".to_owned()
        }
        _ if matches!(phase, ErrorPhase::Install) => {
            "Update installation failed; the current version was kept. Try again later or contact support.".to_owned()
        }
        _ if matches!(phase, ErrorPhase::Download) => {
            "Update download failed; the current version was kept. Check the connection and try again.".to_owned()
        }
        _ => "Update check failed. Check the connection and try again.".to_owned(),
    }
}

fn validate_updater_config<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let updater = app
        .config()
        .plugins
        .0
        .get("updater")
        .ok_or_else(|| "Updater is not configured for this build".to_owned())?;

    let endpoints = updater
        .get("endpoints")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Updater has no configured HTTPS endpoint".to_owned())?;
    if endpoints.len() != 1 {
        return Err("Updater must use exactly one canonical HTTPS endpoint".to_owned());
    }
    let endpoint = endpoints[0]
        .as_str()
        .ok_or_else(|| "Updater endpoint configuration is invalid".to_owned())?;
    if endpoint != CANONICAL_UPDATE_ENDPOINT {
        return Err("Updater endpoint is not the Controwly GitHub release endpoint".to_owned());
    }
    if !endpoint.starts_with("https://") {
        return Err("Updater endpoint must use HTTPS".to_owned());
    }

    let public_key = updater
        .get("pubkey")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Updater signing public key is not configured".to_owned())?;
    if public_key.trim().is_empty() {
        return Err("Updater signing public key is not configured".to_owned());
    }
    if public_key.len() > MAX_PUBLIC_KEY_BYTES {
        return Err("Updater signing public key is too large".to_owned());
    }

    Ok(())
}

fn validate_update_artifact(update: &Update) -> Result<(), String> {
    if update.download_url.scheme() != "https" {
        return Err("Update artifact URL is not HTTPS; refusing to download it".to_owned());
    }
    if update.signature.trim().is_empty() {
        return Err("Update has no signature; refusing to install an unsigned artifact".to_owned());
    }
    if update.signature.len() > MAX_PUBLIC_KEY_BYTES {
        return Err("Update signature is too large; refusing to process it".to_owned());
    }
    Ok(())
}

async fn check_for_updates_inner<R: Runtime>(
    app: &AppHandle<R>,
    state: &UpdaterState,
) -> Result<UpdateState, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| busy_error("the update check"))?;

    state.clear_pending();
    state.emit_state(app, UpdateState::checking());

    if let Err(message) = validate_updater_config(app) {
        let message = state.set_error(app, message);
        return Err(message);
    }

    let updater = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(updater) => updater,
        Err(error) => {
            let message = plugin_error_message(ErrorPhase::Check, &error);
            let message = state.set_error(app, message);
            return Err(message);
        }
    };

    match updater.check().await {
        Ok(None) => {
            state.clear_pending();
            Ok(state.emit_state(app, UpdateState::current()))
        }
        Ok(Some(update)) => {
            if let Err(message) = validate_update_artifact(&update) {
                let message = state.set_error(app, message);
                return Err(message);
            }

            let version = bounded_text(&update.version, MAX_ERROR_BYTES);
            state.set_pending(Some(update));
            Ok(state.emit_state(app, UpdateState::available(version)))
        }
        Err(error) => {
            let message = plugin_error_message(ErrorPhase::Check, &error);
            let message = state.set_error(app, message);
            Err(message)
        }
    }
}

#[tauri::command]
pub(crate) fn get_update_state(state: State<'_, UpdaterState>) -> UpdateState {
    state.snapshot()
}

#[tauri::command]
pub(crate) async fn check_for_updates<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> Result<UpdateState, String> {
    check_for_updates_inner(&app, state.inner()).await
}

#[tauri::command]
pub(crate) async fn install_update<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> Result<(), String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| busy_error("the update installation"))?;

    let mut update = state.pending().ok_or_else(|| {
        "No verified update is ready to install; check for updates first".to_owned()
    })?;
    let available = state.snapshot();
    if available.status != UpdateStatus::Available {
        return Err("No verified update is ready to install; check for updates first".to_owned());
    }
    validate_update_artifact(&update).map_err(|message| state.set_error(&app, message))?;
    update.timeout = Some(DOWNLOAD_TIMEOUT);

    let version = bounded_text(&update.version, MAX_ERROR_BYTES);
    state.emit_state(&app, UpdateState::downloading(version.clone(), 0, None));

    let mut downloaded = 0_u64;
    let mut total = None;
    let mut last_event = Instant::now()
        .checked_sub(PROGRESS_EVENT_INTERVAL)
        .unwrap_or_else(Instant::now);
    let progress_version = version.clone();
    let progress_state = state.inner();
    let progress_app = app.clone();

    let download_result = tokio::time::timeout(
        DOWNLOAD_TIMEOUT,
        update.download(
            |chunk_length, content_length| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                if let Some(length) = content_length {
                    total = Some(length);
                }

                let next = UpdateState::downloading(progress_version.clone(), downloaded, total);
                progress_state.store_state(next.clone());
                let now = Instant::now();
                if now.duration_since(last_event) >= PROGRESS_EVENT_INTERVAL
                    || total.is_some_and(|length| downloaded >= length)
                {
                    last_event = now;
                    let _ = progress_app.emit(UPDATE_STATE_EVENT, &next);
                }
            },
            || {},
        ),
    )
    .await;
    let bytes = match download_result {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            let message = plugin_error_message(ErrorPhase::Download, &error);
            let message = state.set_error(&app, message);
            return Err(message);
        }
        Err(_) => {
            let message = state.set_error(
                &app,
                "Update download timed out; check the connection and try again".to_owned(),
            );
            return Err(message);
        }
    };

    state.emit_state(&app, UpdateState::installing(version, downloaded, total));

    // The guard owns the same process-wide gate used by every device mutation.
    // Restoration and installer launch must remain in one critical section.
    let guard: DeviceMutationGuard<'_> = state.controller.acquire_exclusive().await;
    if guard.restore_for_shutdown().is_err() {
        let message = state.set_error(
            &app,
            "Update blocked: disabled controllers could not be restored; recovery records were retained. Restore them and try again.".to_owned(),
        );
        return Err(message);
    }

    if let Err(error) = update.install(&bytes) {
        let message = plugin_error_message(ErrorPhase::Install, &error);
        let message = state.set_error(&app, message);
        return Err(message);
    }

    // Windows' signed installer exits the current process from install(). On
    // Linux and macOS, use Tauri's native restart path only after installation
    // and recovery restoration have succeeded while the gate is still held.
    #[cfg(not(windows))]
    {
        tauri::process::restart(&app.env())
    }

    #[cfg(windows)]
    {
        Ok(())
    }
}

/// Starts the release-only automatic check. It never downloads or installs an
/// update; the UI must explicitly invoke `install_update` after consent.
pub(crate) fn spawn_startup_check<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(not(debug_assertions))]
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = tauri::Manager::state::<UpdaterState>(&app);
            let _ = check_for_updates_inner(&app, state.inner()).await;
        });
    }

    #[cfg(debug_assertions)]
    {
        let _ = app;
    }
}
