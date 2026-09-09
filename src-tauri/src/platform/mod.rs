use serde::{Deserialize, Serialize};
use std::fmt;

pub(crate) const RECOVERY_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct DeviceId(pub(crate) String);

impl DeviceId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DeviceCapabilities {
    pub(crate) hid: bool,
    pub(crate) gamepad: bool,
    pub(crate) joystick: bool,
    pub(crate) xinput: bool,
    pub(crate) usb: bool,
    pub(crate) bluetooth: bool,
    pub(crate) full_control: bool,
}

impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self {
            hid: false,
            gamepad: false,
            joystick: false,
            xinput: false,
            usb: false,
            bluetooth: false,
            full_control: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DeviceState {
    pub(crate) enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ControlCoverage {
    Full,
    HidOnly,
    Uncontrolled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PlatformDevice {
    pub(crate) id: DeviceId,
    pub(crate) name: String,
    pub(crate) connection: String,
    pub(crate) state: DeviceState,
    pub(crate) capabilities: DeviceCapabilities,
    pub(crate) coverage: ControlCoverage,
    pub(crate) problem_code: Option<i32>,
    /// The identity is populated only by a fresh native enumeration. It is
    /// deliberately omitted from all serialized/public state.
    #[serde(skip)]
    pub(crate) verified: Option<VerifiedDevice>,
}

impl PlatformDevice {
    pub(crate) fn verified(&self) -> Result<&VerifiedDevice, PlatformError> {
        self.verified
            .as_ref()
            .filter(|identity| identity.is_valid())
            .ok_or_else(|| PlatformError::VerificationFailed(self.id.as_str().to_owned()))
    }

    pub(crate) fn recovery_record(
        &self,
        requested_at: u64,
    ) -> Result<PersistedDisabled, PlatformError> {
        Ok(PersistedDisabled {
            schema: RECOVERY_SCHEMA,
            id: self.id.clone(),
            name: self.name.clone(),
            connection: self.connection.clone(),
            identity: self.verified()?.clone(),
            requested_at,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ControllerProtocol {
    HidGamepad,
    Xusb,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct VerifiedDevice {
    /// Platform discriminator prevents a record from being replayed on a
    /// different OS implementation.
    pub(crate) platform: String,
    /// Stable opaque ID exposed to the UI and used only as an enumeration key.
    pub(crate) stable_id: String,
    /// Native devnode/function identity. This is never accepted directly from
    /// a command; it is created by verified enumeration or trusted recovery.
    pub(crate) instance_id: String,
    /// Hash/normalized tuple of immutable hardware identity properties.
    pub(crate) fingerprint: String,
    pub(crate) protocol: ControllerProtocol,
    pub(crate) coverage: ControlCoverage,
    /// HID usage evidence captured while the collection was inspectable.
    pub(crate) usage_page: u16,
    pub(crate) usage: u16,
}

impl Default for VerifiedDevice {
    fn default() -> Self {
        Self {
            platform: String::new(),
            stable_id: String::new(),
            instance_id: String::new(),
            fingerprint: String::new(),
            protocol: ControllerProtocol::HidGamepad,
            coverage: ControlCoverage::Uncontrolled,
            usage_page: 0,
            usage: 0,
        }
    }
}

impl VerifiedDevice {
    pub(crate) fn is_valid(&self) -> bool {
        !self.platform.is_empty()
            && !self.stable_id.is_empty()
            && !self.instance_id.is_empty()
            && !self.fingerprint.is_empty()
    }

    pub(crate) fn stable_id(&self) -> &str {
        &self.stable_id
    }

    pub(crate) fn same_hardware(&self, other: &Self) -> bool {
        self.is_valid()
            && other.is_valid()
            && self.platform == other.platform
            && self.stable_id == other.stable_id
            && self.instance_id == other.instance_id
            && self.fingerprint == other.fingerprint
            && self.protocol == other.protocol
            && self.coverage == other.coverage
            && self.usage_page == other.usage_page
            && self.usage == other.usage
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PersistedDisabled {
    pub(crate) schema: u32,
    pub(crate) id: DeviceId,
    pub(crate) name: String,
    pub(crate) connection: String,
    pub(crate) identity: VerifiedDevice,
    pub(crate) requested_at: u64,
}

impl PersistedDisabled {
    pub(crate) fn id(&self) -> &DeviceId {
        &self.id
    }

    pub(crate) fn identity(&self) -> &VerifiedDevice {
        &self.identity
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EnumerationReport {
    pub(crate) devices: Vec<PlatformDevice>,
    pub(crate) errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DeviceOutcome {
    pub(crate) id: DeviceId,
    pub(crate) requested_enabled: bool,
    pub(crate) confirmed_enabled: Option<bool>,
    pub(crate) changed: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct MutationReport {
    pub(crate) outcomes: Vec<DeviceOutcome>,
}

impl MutationReport {
    pub(crate) fn confirmed(id: DeviceId, requested_enabled: bool, changed: bool) -> Self {
        Self {
            outcomes: vec![DeviceOutcome {
                id,
                requested_enabled,
                confirmed_enabled: Some(requested_enabled),
                changed,
                error: None,
            }],
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum PlatformError {
    Unsupported(&'static str),
    PermissionDenied(String),
    NotFound(String),
    /// The OS reports that this exact app-owned function is gone and no
    /// persistent disabled state remains (Linux unbound helper semantics).
    RecoveryGone(String),
    IdentityMismatch(String),
    OperationFailed(String),
    VerificationFailed(String),
    LaunchFailed(String),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) => write!(f, "unsupported platform: {message}"),
            Self::PermissionDenied(message) => write!(f, "permission denied: {message}"),
            Self::NotFound(message) => write!(f, "device not found: {message}"),
            Self::RecoveryGone(message) => {
                write!(
                    f,
                    "app-disabled device is gone and has no persistent OS state: {message}"
                )
            }
            Self::IdentityMismatch(message) => write!(f, "device identity changed: {message}"),
            Self::OperationFailed(message) => write!(f, "device operation failed: {message}"),
            Self::VerificationFailed(message) => write!(f, "device verification failed: {message}"),
            Self::LaunchFailed(message) => {
                write!(f, "could not open Bluetooth settings: {message}")
            }
        }
    }
}

pub(crate) trait ControllerPlatform: Send + Sync {
    fn enumerate(&self) -> Result<EnumerationReport, PlatformError>;
    fn resolve_verified(&self, id: &DeviceId) -> Result<VerifiedDevice, PlatformError>;
    /// Revalidates an app-owned recovery identity against the current native
    /// function. Implementations may inspect disabled/all-class records when
    /// a live HID handle cannot be opened, but must never trust the hint
    /// without exact native identity/fingerprint checks.
    fn resolve_verified_for_recovery(
        &self,
        id: &DeviceId,
        expected: &VerifiedDevice,
    ) -> Result<VerifiedDevice, PlatformError> {
        let fresh = self.resolve_verified(id)?;
        if fresh.same_hardware(expected) {
            Ok(fresh)
        } else {
            Err(PlatformError::IdentityMismatch(format!(
                "{}: fresh recovery identity does not match the persisted identity",
                id.as_str()
            )))
        }
    }
    fn set_enabled(
        &self,
        identity: &VerifiedDevice,
        enabled: bool,
    ) -> Result<MutationReport, PlatformError>;
    fn open_bluetooth_settings(&self) -> Result<(), PlatformError>;
}

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(not(any(windows, target_os = "linux")))]
struct UnsupportedPlatform;

#[cfg(not(any(windows, target_os = "linux")))]
impl ControllerPlatform for UnsupportedPlatform {
    fn enumerate(&self) -> Result<EnumerationReport, PlatformError> {
        Err(PlatformError::Unsupported(
            "controller management is not implemented for this operating system",
        ))
    }

    fn resolve_verified(&self, _id: &DeviceId) -> Result<VerifiedDevice, PlatformError> {
        Err(PlatformError::Unsupported(
            "controller management is not implemented for this operating system",
        ))
    }

    fn set_enabled(
        &self,
        _identity: &VerifiedDevice,
        _enabled: bool,
    ) -> Result<MutationReport, PlatformError> {
        Err(PlatformError::Unsupported(
            "controller management is not implemented for this operating system",
        ))
    }

    fn open_bluetooth_settings(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "Bluetooth settings are not implemented for this operating system",
        ))
    }
}

pub(crate) fn create() -> Box<dyn ControllerPlatform> {
    #[cfg(windows)]
    {
        return Box::new(windows::WindowsPlatform::new());
    }
    #[cfg(target_os = "linux")]
    {
        return Box::new(linux::LinuxPlatform::new());
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        return Box::new(UnsupportedPlatform);
    }
}
