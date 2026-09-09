use crate::platform::{
    ControlCoverage, ControllerPlatform, DeviceId, EnumerationReport, MutationReport,
    PersistedDisabled, PlatformDevice, PlatformError, RECOVERY_SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const DEFAULT_SHORTCUT: &str = "Ctrl+Shift+F10";
const STATE_FILE: &str = "controller-state.json";
const MAX_RECOVERY_RECORDS: usize = 256;
const MAX_SELECTIONS: usize = 1024;
const MAX_ERRORS: usize = 64;

pub(crate) const PROBLEM_PARTIAL_HID: i32 = 1001;
pub(crate) const PROBLEM_UNCONTROLLED_XINPUT: i32 = 1002;
pub(crate) const PROBLEM_RECOVERY_PENDING: i32 = 1003;
pub(crate) const PROBLEM_RECOVERY_IDENTITY: i32 = 1004;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ControllerDevice {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) connection: String,
    pub(crate) enabled: bool,
    pub(crate) selected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) problem_code: Option<i32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ControllerState {
    pub(crate) devices: Vec<ControllerDevice>,
    pub(crate) shortcut: String,
    pub(crate) shortcut_available: bool,
    pub(crate) errors: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct RestoreSummary {
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiskState {
    schema: u32,
    selected: Vec<String>,
    disabled: Vec<PersistedDisabled>,
    shortcut: String,
}

struct ControllerData {
    selected: BTreeSet<String>,
    disabled: Vec<PersistedDisabled>,
    shortcut: String,
    shortcut_available: bool,
    errors: Vec<String>,
    persist_path: PathBuf,
}

/// Process-wide lease for every operation that can race a native device
/// mutation: commands, tray actions, global shortcuts, shutdown, and updates.
pub(crate) struct MutationCoordinator {
    gate: Arc<Semaphore>,
    platform: Box<dyn ControllerPlatform>,
    data: Mutex<ControllerData>,
}

pub(crate) type SharedController = Arc<MutationCoordinator>;

pub(crate) struct DeviceMutationGuard<'a> {
    coordinator: SharedController,
    _permit: OwnedSemaphorePermit,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl MutationCoordinator {
    pub(crate) fn new(
        platform: Box<dyn ControllerPlatform>,
        data_dir: impl Into<PathBuf>,
    ) -> Result<SharedController, String> {
        let data_dir = data_dir.into();
        fs::create_dir_all(&data_dir)
            .map_err(|error| format!("cannot create controller data directory: {error}"))?;
        let persist_path = data_dir.join(STATE_FILE);
        let disk = load_disk_state(&persist_path)?;
        let data = ControllerData::from_disk(disk, persist_path)?;
        Ok(Arc::new(Self {
            gate: Arc::new(Semaphore::new(1)),
            platform,
            data: Mutex::new(data),
        }))
    }

    pub(crate) async fn acquire_exclusive(self: &Arc<Self>) -> DeviceMutationGuard<'static> {
        let permit = self
            .gate
            .clone()
            .acquire_owned()
            .await
            .expect("controller lifecycle semaphore must remain open");
        DeviceMutationGuard {
            coordinator: self.clone(),
            _permit: permit,
            _lifetime: std::marker::PhantomData,
        }
    }

    pub(crate) async fn get_state(self: &Arc<Self>) -> ControllerState {
        let guard = self.acquire_exclusive().await;
        match tokio::task::spawn_blocking(move || guard.refresh_state()).await {
            Ok(state) => state,
            Err(error) => ControllerState {
                devices: Vec::new(),
                shortcut: DEFAULT_SHORTCUT.to_owned(),
                shortcut_available: false,
                errors: vec![format!(
                    "controller worker terminated while refreshing state: {error}"
                )],
            },
        }
    }

    pub(crate) async fn set_selected(
        self: &Arc<Self>,
        id: String,
        selected: bool,
    ) -> Result<ControllerState, String> {
        let guard = self.acquire_exclusive().await;
        tokio::task::spawn_blocking(move || guard.set_selected(&id, selected))
            .await
            .map_err(|error| {
                format!("controller worker terminated while saving selection: {error}")
            })?
    }

    pub(crate) async fn set_device_enabled(
        self: &Arc<Self>,
        id: String,
        enabled: bool,
    ) -> Result<ControllerState, String> {
        let guard = self.acquire_exclusive().await;
        tokio::task::spawn_blocking(move || guard.set_device_enabled(&id, enabled))
            .await
            .map_err(|error| {
                format!("controller worker terminated while changing device state: {error}")
            })?
    }

    pub(crate) async fn set_selected_enabled(
        self: &Arc<Self>,
        enabled: bool,
    ) -> Result<ControllerState, String> {
        let guard = self.acquire_exclusive().await;
        tokio::task::spawn_blocking(move || guard.set_selected_enabled(enabled))
            .await
            .map_err(|error| {
                format!("controller worker terminated while changing selected devices: {error}")
            })?
    }

    pub(crate) async fn restore_disabled(self: &Arc<Self>) -> Result<ControllerState, String> {
        let guard = self.acquire_exclusive().await;
        tokio::task::spawn_blocking(move || {
            let (_summary, state) = guard.restore_disabled_inner();
            Ok(state)
        })
        .await
        .map_err(|error| format!("controller worker terminated while restoring devices: {error}"))?
    }

    pub(crate) async fn open_bluetooth_settings(self: &Arc<Self>) -> Result<(), String> {
        let guard = self.acquire_exclusive().await;
        tokio::task::spawn_blocking(move || guard.open_bluetooth_settings())
            .await
            .map_err(|error| {
                format!("controller worker terminated while opening Bluetooth settings: {error}")
            })?
    }
}

impl DeviceMutationGuard<'_> {
    pub(crate) fn restore_for_shutdown(&self) -> Result<RestoreSummary, String> {
        let (summary, _state) = self.restore_disabled_inner();
        if summary.failed != 0 {
            return Err(format!(
                "{} controller recovery operation(s) failed; disabled-device records were retained",
                summary.failed
            ));
        }
        Ok(summary)
    }

    pub(crate) fn set_shortcut_available(&self, available: bool, error: Option<String>) {
        let mut data = lock_data(&self.coordinator.data);
        data.shortcut_available = available;
        if let Some(error) = error {
            push_error(&mut data.errors, error);
        }
    }

    pub(crate) fn record_error(&self, error: String) {
        let mut data = lock_data(&self.coordinator.data);
        push_error(&mut data.errors, error);
    }
    fn open_bluetooth_settings(&self) -> Result<(), String> {
        self.coordinator
            .platform
            .open_bluetooth_settings()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn refresh_state(&self) -> ControllerState {
        let report = self.coordinator.platform.enumerate();
        let data = lock_data(&self.coordinator.data);
        let (native_devices, native_errors) = match report {
            Ok(report) => (report.devices, report.errors),
            Err(error) => (Vec::new(), vec![error.to_string()]),
        };
        let mut state = make_state(&data, native_devices);
        state.errors.extend(native_errors);
        state
    }

    fn set_selected(&self, id: &str, selected: bool) -> Result<ControllerState, String> {
        validate_command_id(id)?;
        let report = self
            .coordinator
            .platform
            .enumerate()
            .map_err(|error| error.to_string())?;
        let is_current = report.devices.iter().any(|device| device.id.as_str() == id);
        let has_recovery = {
            let data = lock_data(&self.coordinator.data);
            data.disabled
                .iter()
                .any(|record| record.id().as_str() == id)
        };
        if !is_current && !has_recovery {
            return Err(format!(
                "controller {id} is not present in the verified inventory"
            ));
        }

        let mut data = lock_data(&self.coordinator.data);
        let was_selected = data.selected.contains(id);
        if selected {
            if data.selected.len() >= MAX_SELECTIONS && !was_selected {
                let message = "controller selection limit reached".to_owned();
                push_error(&mut data.errors, message.clone());
                return Err(message);
            }
            data.selected.insert(id.to_owned());
        } else {
            data.selected.remove(id);
        }
        if let Err(error) = persist_data(&data) {
            if was_selected {
                data.selected.insert(id.to_owned());
            } else {
                data.selected.remove(id);
            }
            push_error(&mut data.errors, error.clone());
            return Err(error);
        }
        drop(data);
        Ok(self.refresh_state())
    }

    fn set_device_enabled(&self, id: &str, enabled: bool) -> Result<ControllerState, String> {
        validate_command_id(id)?;
        self.perform_device(id, enabled)?;
        Ok(self.refresh_state())
    }

    fn set_selected_enabled(&self, enabled: bool) -> Result<ControllerState, String> {
        let current = match self.coordinator.platform.enumerate() {
            Ok(report) => report.devices,
            Err(error) => {
                let mut data = lock_data(&self.coordinator.data);
                let message = error.to_string();
                push_error(&mut data.errors, message.clone());
                return Err(message);
            }
        };
        let selected_ids: Vec<String> = {
            let data = lock_data(&self.coordinator.data);
            current
                .iter()
                .filter(|device| data.selected.contains(device.id.as_str()))
                .map(|device| device.id.as_str().to_owned())
                .chain(data.disabled.iter().filter_map(|record| {
                    if data.selected.contains(record.id().as_str())
                        && !current
                            .iter()
                            .any(|device| device.id.as_str() == record.id().as_str())
                    {
                        Some(record.id().as_str().to_owned())
                    } else {
                        None
                    }
                }))
                .collect()
        };

        for id in selected_ids {
            if let Err(error) = self.perform_device(&id, enabled) {
                let mut data = lock_data(&self.coordinator.data);
                push_error(&mut data.errors, format!("{id}: {error}"));
            }
        }
        Ok(self.refresh_state())
    }

    fn perform_device(&self, id: &str, enabled: bool) -> Result<MutationReport, String> {
        let report = self
            .coordinator
            .platform
            .enumerate()
            .map_err(|error| error.to_string())?;
        let current = report
            .devices
            .iter()
            .find(|device| device.id.as_str() == id);
        let recovery = {
            let data = lock_data(&self.coordinator.data);
            data.disabled
                .iter()
                .find(|record| record.id().as_str() == id)
                .cloned()
        };

        if enabled {
            let identity = if let Some(record) = &recovery {
                if record.schema != RECOVERY_SCHEMA || !record.identity().is_valid() {
                    return self.fail(format!("{id}: invalid persisted recovery record"));
                }
                let fresh = self
                    .coordinator
                    .platform
                    .resolve_verified_for_recovery(record.id(), record.identity())
                    .map_err(|error| error.to_string())?;
                if !fresh.same_hardware(record.identity()) {
                    return self.fail(format!(
                        "{id}: persisted recovery identity no longer matches a fresh native resolve"
                    ));
                }
                fresh
            } else if let Some(device) = current {
                device
                    .verified()
                    .map_err(|error| error.to_string())?
                    .clone()
            } else {
                return self.fail(format!(
                    "{id}: controller is not present in the verified inventory"
                ));
            };

            let result = self
                .coordinator
                .platform
                .set_enabled(&identity, true)
                .map_err(|error| error.to_string())?;
            if !self.confirm_enabled(id, &identity) {
                return self.fail(format!(
                    "{id}: enable was not confirmed by a fresh enumeration"
                ));
            }
            if recovery.is_some() {
                self.remove_recovery(&identity)?;
            }
            self.clear_device_errors(id);
            Ok(result)
        } else {
            let device = current.ok_or_else(|| {
                format!("{id}: controller is not present; refusing an unverified disable request")
            })?;
            let identity = device
                .verified()
                .map_err(|error| error.to_string())?
                .clone();
            if identity.coverage != ControlCoverage::Full || !device.capabilities.full_control {
                let message = match identity.coverage {
                    ControlCoverage::HidOnly => format!(
                        "{id}: only the HID collection is controllable; the XInput/XUSB path remains live"
                    ),
                    ControlCoverage::Uncontrolled => format!(
                        "{id}: controller input path is not safely controllable; refusing disable"
                    ),
                    ControlCoverage::Full => format!("{id}: controller cannot be safely controlled"),
                };
                return self.fail(message);
            }
            let record = device
                .recovery_record(now_unix_seconds())
                .map_err(|error| error.to_string())?;
            // Crash-safety order: durable app-owned intent first, native
            // mutation second, post-state confirmation inside the platform.
            self.persist_recovery(record)?;
            let result = self
                .coordinator
                .platform
                .set_enabled(&identity, false)
                .map_err(|error| error.to_string());
            match result {
                Ok(result) => {
                    let confirmed = result
                        .outcomes
                        .first()
                        .and_then(|outcome| outcome.confirmed_enabled)
                        == Some(false);
                    if !confirmed {
                        return self.fail(format!(
                            "{id}: disable call returned without confirmed disabled state; recovery record retained"
                        ));
                    }
                    self.clear_device_errors(id);
                    Ok(result)
                }
                Err(error) => self.fail(format!("{id}: {error}; recovery record retained")),
            }
        }
    }

    fn confirm_enabled(&self, id: &str, expected: &crate::platform::VerifiedDevice) -> bool {
        match self.coordinator.platform.enumerate() {
            Ok(EnumerationReport { devices, errors: _ }) => devices
                .iter()
                .find(|device| device.id.as_str() == id)
                .map(|device| {
                    device.state.enabled
                        && device
                            .verified()
                            .map(|identity| identity.same_hardware(expected))
                            .unwrap_or(false)
                })
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    fn persist_recovery(&self, record: PersistedDisabled) -> Result<(), String> {
        if record.schema != RECOVERY_SCHEMA || !record.identity().is_valid() {
            return self.fail("refusing to persist an invalid controller identity".to_owned());
        }
        let mut data = lock_data(&self.coordinator.data);
        if let Some(existing) = data
            .disabled
            .iter()
            .find(|existing| existing.id() == record.id())
        {
            if !existing.identity().same_hardware(record.identity()) {
                let message = format!(
                    "{}: an existing recovery record has a different verified identity",
                    record.id().as_str()
                );
                push_error(&mut data.errors, message.clone());
                return Err(message);
            }
            return Ok(());
        }
        if data.disabled.len() >= MAX_RECOVERY_RECORDS {
            let message =
                "recovery record limit reached; refusing to disable another controller".to_owned();
            push_error(&mut data.errors, message.clone());
            return Err(message);
        }
        data.disabled.push(record.clone());
        if let Err(error) = persist_data(&data) {
            data.disabled.retain(|existing| {
                !(existing.id() == record.id()
                    && existing.identity().same_hardware(record.identity()))
            });
            push_error(&mut data.errors, error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn remove_recovery(&self, identity: &crate::platform::VerifiedDevice) -> Result<(), String> {
        let mut data = lock_data(&self.coordinator.data);
        let old = data.disabled.clone();
        data.disabled
            .retain(|record| !record.identity().same_hardware(identity));
        if old.len() == data.disabled.len() {
            return Ok(());
        }
        if let Err(error) = persist_data(&data) {
            data.disabled = old;
            let message = format!(
                "{} was enabled, but its recovery record could not be removed: {error}",
                identity.stable_id()
            );
            push_error(&mut data.errors, message.clone());
            return Err(message);
        }
        Ok(())
    }

    fn restore_disabled_inner(&self) -> (RestoreSummary, ControllerState) {
        let records = {
            let data = lock_data(&self.coordinator.data);
            data.disabled.clone()
        };
        let mut summary = RestoreSummary::default();
        for record in records {
            let id = record.id().as_str().to_owned();
            let verified = match self
                .coordinator
                .platform
                .resolve_verified_for_recovery(record.id(), record.identity())
            {
                Ok(identity) if identity.same_hardware(record.identity()) => identity,
                Ok(_) => {
                    summary.failed += 1;
                    self.record_failure(
                        &id,
                        "fresh identity does not match the persisted recovery record; record retained"
                            .to_owned(),
                    );
                    continue;
                }
                Err(PlatformError::RecoveryGone(message)) => {
                    match self.remove_recovery(record.identity()) {
                        Ok(()) => {
                            self.clear_device_errors(&id);
                            summary.succeeded += 1;
                        }
                        Err(error) => {
                            summary.failed += 1;
                            self.record_failure(
                                &id,
                                format!(
                                    "{message}; the recovery record could not be removed: {error}"
                                ),
                            );
                        }
                    }
                    continue;
                }
                Err(error) => {
                    summary.failed += 1;
                    self.record_failure(&id, format!("{error}; recovery record retained"));
                    continue;
                }
            };
            match self.coordinator.platform.set_enabled(&verified, true) {
                Ok(_report) if self.confirm_enabled(&id, &verified) => {
                    match self.remove_recovery(&verified) {
                        Ok(()) => {
                            self.clear_device_errors(&id);
                            summary.succeeded += 1;
                        }
                        Err(error) => {
                            summary.failed += 1;
                            self.record_failure(&id, error);
                        }
                    }
                }
                Ok(_report) => {
                    summary.failed += 1;
                    self.record_failure(
                        &id,
                        "enable was not confirmed by a fresh enumeration; recovery record retained"
                            .to_owned(),
                    );
                }
                Err(error) => {
                    summary.failed += 1;
                    self.record_failure(&id, format!("{error}; recovery record retained"));
                }
            }
        }
        (summary, self.refresh_state())
    }

    fn clear_device_errors(&self, id: &str) {
        let mut data = lock_data(&self.coordinator.data);
        data.errors.retain(|error| {
            error
                .strip_prefix(id)
                .map_or(true, |suffix| !suffix.starts_with(": "))
        });
    }

    fn record_failure(&self, id: &str, message: String) {
        let mut data = lock_data(&self.coordinator.data);
        push_error(&mut data.errors, format!("{id}: {message}"));
    }

    fn fail<T>(&self, message: String) -> Result<T, String> {
        let mut data = lock_data(&self.coordinator.data);
        push_error(&mut data.errors, message.clone());
        Err(message)
    }
}

impl ControllerData {
    fn from_disk(disk: DiskState, persist_path: PathBuf) -> Result<Self, String> {
        if disk.schema != RECOVERY_SCHEMA {
            return Err(format!(
                "controller state schema {} is not supported (expected {})",
                disk.schema, RECOVERY_SCHEMA
            ));
        }
        if disk.selected.len() > MAX_SELECTIONS {
            return Err("controller selection state exceeds the supported limit".to_owned());
        }
        if disk.disabled.len() > MAX_RECOVERY_RECORDS {
            return Err("controller recovery state exceeds the supported limit".to_owned());
        }
        let mut selected = BTreeSet::new();
        for id in disk.selected {
            validate_command_id(&id)?;
            selected.insert(id);
        }
        let mut seen = BTreeSet::new();
        for record in &disk.disabled {
            if record.schema != RECOVERY_SCHEMA
                || !record.identity().is_valid()
                || record.id() != &DeviceId(record.identity().stable_id().to_owned())
            {
                return Err("controller state contains an invalid recovery identity".to_owned());
            }
            if !seen.insert(record.id().as_str().to_owned()) {
                return Err(format!(
                    "controller state contains duplicate recovery id {}",
                    record.id().as_str()
                ));
            }
        }
        let shortcut = if disk.shortcut.trim().is_empty() {
            DEFAULT_SHORTCUT.to_owned()
        } else {
            disk.shortcut
        };
        Ok(Self {
            selected,
            disabled: disk.disabled,
            shortcut,
            shortcut_available: false,
            errors: Vec::new(),
            persist_path,
        })
    }
}

fn load_disk_state(path: &Path) -> Result<DiskState, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("cannot parse {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DiskState {
            schema: RECOVERY_SCHEMA,
            selected: Vec::new(),
            disabled: Vec::new(),
            shortcut: DEFAULT_SHORTCUT.to_owned(),
        }),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn persist_data(data: &ControllerData) -> Result<(), String> {
    let disk = DiskState {
        schema: RECOVERY_SCHEMA,
        selected: data.selected.iter().cloned().collect(),
        disabled: data.disabled.clone(),
        shortcut: data.shortcut.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&disk)
        .map_err(|error| format!("cannot encode controller state: {error}"))?;
    let temporary = data.persist_path.with_extension("json.new");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("cannot create controller state temporary file: {error}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot durably write controller state: {error}"))?;
    drop(file);
    atomic_replace(&temporary, &data.persist_path)
        .map_err(|error| format!("cannot commit controller state: {error}"))
}

#[cfg(not(windows))]
fn atomic_replace(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn atomic_replace(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn make_state(data: &ControllerData, mut devices: Vec<PlatformDevice>) -> ControllerState {
    devices.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut state_devices = Vec::with_capacity(devices.len() + data.disabled.len());
    for device in devices {
        let id = device.id.as_str().to_owned();
        let pending_record = data
            .disabled
            .iter()
            .find(|record| record.id().as_str() == id);
        let mut problem_code = device.problem_code;
        if device.coverage == ControlCoverage::HidOnly {
            problem_code.get_or_insert(PROBLEM_PARTIAL_HID);
        } else if device.coverage == ControlCoverage::Uncontrolled {
            problem_code.get_or_insert(PROBLEM_UNCONTROLLED_XINPUT);
        }
        if pending_record.is_some() && device.state.enabled {
            problem_code.get_or_insert(PROBLEM_RECOVERY_IDENTITY);
        }
        state_devices.push(ControllerDevice {
            id: id.clone(),
            name: device.name,
            connection: device.connection,
            enabled: device.state.enabled,
            selected: data.selected.contains(&id),
            problem_code,
        });
    }
    for record in &data.disabled {
        if !state_devices
            .iter()
            .any(|device| device.id == record.id().as_str())
        {
            state_devices.push(ControllerDevice {
                id: record.id().as_str().to_owned(),
                name: format!("{} (app-disabled; recovery pending)", record.name),
                connection: record.connection.clone(),
                enabled: false,
                selected: data.selected.contains(record.id().as_str()),
                problem_code: Some(PROBLEM_RECOVERY_PENDING),
            });
        }
    }
    state_devices.sort_by(|left, right| left.id.cmp(&right.id));
    ControllerState {
        devices: state_devices,
        shortcut: data.shortcut.clone(),
        shortcut_available: data.shortcut_available,
        errors: data.errors.clone(),
    }
}

fn validate_command_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 1024 || id.chars().any(|character| character == '\0') {
        return Err("invalid controller id".to_owned());
    }
    Ok(())
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn lock_data(data: &Mutex<ControllerData>) -> MutexGuard<'_, ControllerData> {
    data.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn push_error(errors: &mut Vec<String>, message: String) {
    if message.is_empty() || errors.iter().any(|existing| existing == &message) {
        return;
    }
    errors.push(message);
    if errors.len() > MAX_ERRORS {
        let excess = errors.len() - MAX_ERRORS;
        errors.drain(0..excess);
    }
}
