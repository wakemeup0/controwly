//! Linux controller platform implementation.
//!
//! Disabling is intentionally helper-first.  The trusted helper revalidates a
//! fresh kernel HID or USB *function* identity and unbinds only an approved
//! controller driver.  This removes the function's evdev, joydev, hidraw, and
//! HIDAPI/SDL paths without unbinding a USB parent.  We do not silently fall
//! back to EVIOCGRAB: an unprivileged evdev grab would leave direct HID clients
//! active and would falsely look like a complete disable in the UI.

#[path = "../../linux/input.rs"]
mod input;

use super::{
    ControlCoverage, ControllerPlatform, ControllerProtocol, DeviceCapabilities, DeviceId,
    DeviceState, EnumerationReport, MutationReport, PlatformDevice, PlatformError, VerifiedDevice,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const HELPER_PATHS: &[&str] = &[
    "/usr/libexec/controwly-linux-input-helper",
    "/usr/lib/controwly/controwly-linux-input-helper",
];
const PKEXEC: &str = "/usr/bin/pkexec";
const PROBLEM_HELPER_MISSING: i32 = 7201;
const PROBLEM_PARTIAL_FUNCTION: i32 = 7202;
const PROBLEM_USAGE_EVIDENCE: i32 = 7203;
const LIMITATION_MESSAGE: &str =
    "Linux control covers only the validated controller HID/USB function; separate sibling functions are intentionally left alone";

#[derive(Clone, Debug)]
struct DisabledController {
    controller: input::Controller,
}

pub(crate) struct LinuxPlatform {
    /// Entries in this map are functions unbound by this process.  Keeping a
    /// snapshot lets the UI continue to show a disabled controller even though
    /// the kernel no longer exposes input event nodes for the unbound function.
    disabled: Mutex<BTreeMap<String, DisabledController>>,
}

impl LinuxPlatform {
    pub(crate) fn new() -> Self {
        Self {
            disabled: Mutex::new(BTreeMap::new()),
        }
    }

    fn enumerate_internal(&self) -> EnumerationReport {
        let discovery = input::discover();
        let helper_available = helper_path().is_some() && trusted_pkexec();
        let mut disabled = self
            .disabled
            .lock()
            .expect("Linux controller state mutex poisoned");
        let mut devices = Vec::new();
        let mut seen = BTreeMap::new();
        let mut errors = discovery.errors;

        for controller in discovery.controllers {
            let key = controller.identity.stable_id.clone();
            let cached_identity_matches = disabled
                .get(&key)
                .is_some_and(|entry| entry.controller.identity == controller.identity);
            // A live bound function is authoritative for the displayed state.
            // Only an exact, approved reappearance invalidates the cached
            // mutation snapshot; a same-stable replacement keeps it private
            // for recovery without masking the live row.
            let live_bound = controller
                .function
                .as_ref()
                .map(|function| function.driver.is_some())
                .unwrap_or(true);
            let live_bound_approved = controller
                .function
                .as_ref()
                .and_then(|function| function.driver.as_deref())
                .is_some_and(input::safe_driver);
            if cached_identity_matches && live_bound_approved {
                disabled.remove(&key);
            }
            let is_disabled = !live_bound;
            let coverage = coverage_for(&controller, helper_available);
            if !helper_available {
                errors.push(format!(
                    "{}: Linux controller helper is not installed as a trusted root-owned executable; refusing an incomplete evdev-only disable",
                    controller.name
                ));
            } else if coverage != ControlCoverage::Full {
                errors.push(format!(
                    "{}: controller function is not safely controllable ({LIMITATION_MESSAGE})",
                    controller.name
                ));
            }
            seen.insert(key.clone(), ());
            devices.push(platform_device(
                &controller,
                coverage,
                !is_disabled,
                helper_available,
            ));
        }

        // A successful unbind removes the event nodes. Preserve that device in
        // state until it is explicitly re-enabled, and never invent a path or
        // identity from UI data.
        for (key, disabled_controller) in disabled.iter() {
            if seen.contains_key(key) {
                continue;
            }
            let controller = &disabled_controller.controller;
            let helper_available = helper_path().is_some() && trusted_pkexec();
            let coverage = coverage_for(controller, helper_available);
            devices.push(platform_device(
                controller,
                coverage,
                false,
                helper_available,
            ));
        }
        devices.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        EnumerationReport { devices, errors }
    }

    fn current_controller(
        &self,
        identity: &VerifiedDevice,
    ) -> Result<input::Controller, PlatformError> {
        let native = native_identity(identity)?;
        let discovery = input::discover();
        if let Some(controller) = discovery
            .controllers
            .into_iter()
            .find(|controller| controller.identity == native)
        {
            return Ok(controller);
        }

        let disabled = self.disabled.lock().map_err(|_| {
            PlatformError::OperationFailed("Linux controller state mutex was poisoned".to_owned())
        })?;
        if let Some(entry) = disabled.get(identity.stable_id()) {
            if entry.controller.identity == native {
                return input::controller_from_unbound(&native).map_err(|message| {
                    PlatformError::IdentityMismatch(format!("{}: {message}", identity.stable_id()))
                });
            }
        }
        drop(disabled);
        // Recovery records are persisted by the shared controller layer. On a
        // fresh process, reconstruct only after the exact function fingerprint,
        // digest, approved driver, and current function properties are revalidated.
        // A live HID report descriptor is required again once the function is bound.
        if identity.coverage == ControlCoverage::Full
            || identity.coverage == ControlCoverage::HidOnly
        {
            return input::controller_from_unbound(&native).map_err(|message| {
                PlatformError::IdentityMismatch(format!("{}: {message}", identity.stable_id()))
            });
        }
        Err(PlatformError::NotFound(identity.stable_id().to_owned()))
    }

    fn disable(
        &self,
        identity: &VerifiedDevice,
        controller: input::Controller,
    ) -> Result<MutationReport, PlatformError> {
        let native = native_identity(identity)?;
        if !controller.can_full_control {
            return Err(PlatformError::OperationFailed(format!(
                "{}: {}",
                controller.name,
                control_problem(&controller)
            )));
        }
        let coverage = coverage_for(&controller, true);
        if coverage == ControlCoverage::Uncontrolled {
            return Err(PlatformError::Unsupported(
                "Linux cannot safely isolate this controller function from an unrelated keyboard/mouse sibling",
            ));
        }
        if self
            .disabled
            .lock()
            .map_err(|_| {
                PlatformError::OperationFailed(
                    "Linux controller state mutex was poisoned".to_owned(),
                )
            })?
            .contains_key(identity.stable_id())
        {
            return Ok(MutationReport::confirmed(
                DeviceId(identity.stable_id().to_owned()),
                false,
                false,
            ));
        }

        invoke_helper("disable", &native)?;
        if !wait_for_unbound(&native) {
            return Err(PlatformError::VerificationFailed(format!(
                "{}: helper returned but the validated function is still bound",
                identity.stable_id()
            )));
        }
        self.disabled
            .lock()
            .map_err(|_| {
                PlatformError::OperationFailed(
                    "Linux controller state mutex was poisoned".to_owned(),
                )
            })?
            .insert(
                identity.stable_id().to_owned(),
                DisabledController {
                    controller: controller.clone(),
                },
            );
        let mut report =
            MutationReport::confirmed(DeviceId(identity.stable_id().to_owned()), false, true);
        if let Some(outcome) = report.outcomes.first_mut() {
            outcome.error = Some(mutation_warning(&controller).to_owned());
        }
        Ok(report)
    }

    fn enable(
        &self,
        identity: &VerifiedDevice,
        controller: Option<input::Controller>,
    ) -> Result<MutationReport, PlatformError> {
        let native = native_identity(identity)?;
        if identity.coverage != ControlCoverage::Full
            && identity.coverage != ControlCoverage::HidOnly
        {
            return Err(PlatformError::Unsupported(
                "this Linux device was not enumerated with function-control coverage",
            ));
        }
        // A live controller is checked before the helper is dispatched. When
        // recovery has no live event node, the expected identity is still only
        // a bounded enable request; the root helper must find the matching
        // journal record under its own lock before doing anything.
        if let Some(controller) = controller {
            if !controller.can_full_control {
                return Err(PlatformError::IdentityMismatch(format!(
                    "{}: current controller function is not safely controllable",
                    identity.stable_id()
                )));
            }
        }
        invoke_helper("enable", &native)?;
        if !wait_for_bound(&native) {
            return Err(PlatformError::VerificationFailed(format!(
                "{}: helper returned but the controller function did not reappear",
                identity.stable_id()
            )));
        }
        self.disabled
            .lock()
            .map_err(|_| {
                PlatformError::OperationFailed(
                    "Linux controller state mutex was poisoned".to_owned(),
                )
            })?
            .remove(identity.stable_id());
        Ok(MutationReport::confirmed(
            DeviceId(identity.stable_id().to_owned()),
            true,
            true,
        ))
    }
}

impl ControllerPlatform for LinuxPlatform {
    fn enumerate(&self) -> Result<EnumerationReport, PlatformError> {
        Ok(self.enumerate_internal())
    }

    fn resolve_verified(&self, id: &DeviceId) -> Result<VerifiedDevice, PlatformError> {
        if !input::valid_stable_id(id.as_str()) {
            return Err(PlatformError::VerificationFailed(
                "malformed Linux controller ID".to_owned(),
            ));
        }
        self.enumerate_internal()
            .devices
            .into_iter()
            .find(|device| device.id == id.clone())
            .and_then(|device| device.verified)
            .ok_or_else(|| {
                PlatformError::NotFound(format!(
                    "{}: controller is not present in the verified inventory",
                    id.as_str()
                ))
            })
    }

    fn resolve_verified_for_recovery(
        &self,
        id: &DeviceId,
        expected: &VerifiedDevice,
    ) -> Result<VerifiedDevice, PlatformError> {
        if id.as_str() != expected.stable_id() || expected.platform != "linux" {
            return Err(PlatformError::IdentityMismatch(
                "Linux recovery ID and persisted platform identity do not match".to_owned(),
            ));
        }
        let native = native_identity(expected)?;
        if let Some(fresh) = self
            .enumerate_internal()
            .devices
            .into_iter()
            .find(|device| device.id == id.clone())
            .and_then(|device| device.verified)
        {
            return if fresh.same_hardware(expected) {
                Ok(fresh)
            } else {
                Err(PlatformError::IdentityMismatch(
                    "fresh Linux recovery identity does not match the persisted identity"
                        .to_owned(),
                ))
            };
        }

        match input::controller_from_unbound(&native) {
            Ok(controller) => {
                let helper_available = helper_path().is_some() && trusted_pkexec();
                let coverage = coverage_for(&controller, helper_available);
                if coverage == ControlCoverage::Uncontrolled {
                    return Err(PlatformError::PermissionDenied(
                        "trusted Linux helper is unavailable for recovery".to_owned(),
                    ));
                }
                let candidate = platform_device(&controller, coverage, false, helper_available)
                    .verified
                    .ok_or_else(|| PlatformError::VerificationFailed(id.as_str().to_owned()))?;
                if candidate.same_hardware(expected) {
                    Ok(candidate)
                } else {
                    Err(PlatformError::IdentityMismatch(
                        "unbound Linux function does not match the persisted recovery identity"
                            .to_owned(),
                    ))
                }
            }
            Err(message) => {
                if input::function_is_definitively_gone(&native) {
                    return Err(PlatformError::RecoveryGone(format!(
                        "{}: no matching disabled HID/USB function or transport remains",
                        id.as_str()
                    )));
                }
                // This is a dispatch-only identity. The subsequent helper
                // invocation must resolve the authoritative root journal under
                // lock; this path never fabricates an enabled result or grants
                // a new disable authority.
                let _ = message;
                Ok(expected.clone())
            }
        }
    }

    fn set_enabled(
        &self,
        identity: &VerifiedDevice,
        enabled: bool,
    ) -> Result<MutationReport, PlatformError> {
        if identity.platform != "linux" || !identity.is_valid() {
            return Err(PlatformError::VerificationFailed(
                "malformed or cross-platform controller identity".to_owned(),
            ));
        }
        if enabled {
            let controller = match self.current_controller(identity) {
                Ok(controller) => Some(controller),
                // A persisted recovery identity may be dispatched to the
                // helper while its live function is temporarily unavailable.
                // The helper is the only authority that may accept it.
                Err(PlatformError::NotFound(_)) | Err(PlatformError::IdentityMismatch(_)) => None,
                Err(error) => return Err(error),
            };
            if let Some(controller) = &controller {
                let helper_available = helper_path().is_some() && trusted_pkexec();
                let expected_coverage = coverage_for(controller, helper_available);
                if identity.coverage != expected_coverage {
                    return Err(PlatformError::IdentityMismatch(format!(
                        "{}: controller coverage changed",
                        identity.stable_id()
                    )));
                }
            } else if identity.coverage == ControlCoverage::Uncontrolled {
                return Err(PlatformError::Unsupported(
                    "this Linux device was not enumerated with function-control coverage",
                ));
            }
            self.enable(identity, controller)
        } else {
            let controller = self.current_controller(identity)?;
            let helper_available = helper_path().is_some() && trusted_pkexec();
            let expected_coverage = coverage_for(&controller, helper_available);
            if identity.coverage != expected_coverage {
                return Err(PlatformError::IdentityMismatch(format!(
                    "{}: controller coverage changed",
                    identity.stable_id()
                )));
            }
            self.disable(identity, controller)
        }
    }

    fn open_bluetooth_settings(&self) -> Result<(), PlatformError> {
        let candidates: &[(&str, &[&str])] = &[
            ("/usr/bin/blueman-manager", &[]),
            ("/usr/bin/gnome-control-center", &["bluetooth"]),
            ("/usr/bin/kcmshell6", &["kcm_bluetooth"]),
            ("/usr/bin/kcmshell5", &["kcm_bluetooth"]),
            ("/usr/bin/systemsettings", &["kcm_bluetooth"]),
        ];
        let mut failures = Vec::new();
        for (program, args) in candidates {
            if !trusted_user_executable(program) {
                continue;
            }
            match Command::new(program).args(*args).spawn() {
                Ok(mut child) => {
                    // A successful dispatch only means that a fixed, installed
                    // launcher accepted the request; window visibility is not
                    // something this backend can prove. Reap quick failures so
                    // a broken first candidate does not mask later managers.
                    let mut still_running = true;
                    for _ in 0..5 {
                        match child.try_wait() {
                            Ok(Some(status)) if status.success() => return Ok(()),
                            Ok(Some(status)) => {
                                failures.push(format!("{program}: exited with status {status}"));
                                still_running = false;
                                break;
                            }
                            Ok(None) => thread::sleep(Duration::from_millis(20)),
                            Err(error) => {
                                failures
                                    .push(format!("{program}: cannot inspect launcher: {error}"));
                                still_running = false;
                                break;
                            }
                        }
                    }
                    if still_running {
                        return Ok(());
                    }
                }
                Err(error) => failures.push(format!("{program}: {error}")),
            }
        }
        let detail = if failures.is_empty() {
            "install Blueman, GNOME Control Center, or KDE Bluetooth settings".to_owned()
        } else {
            failures.join("; ")
        };
        Err(PlatformError::LaunchFailed(detail))
    }
}

fn native_identity(identity: &VerifiedDevice) -> Result<input::Identity, PlatformError> {
    let native = input::Identity {
        platform: identity.platform.clone(),
        stable_id: identity.stable_id.clone(),
        instance_id: identity.instance_id.clone(),
        fingerprint: identity.fingerprint.clone(),
    };
    input::validate_identity(&native).map_err(PlatformError::VerificationFailed)?;
    let fields = input::parse_fingerprint(&native.fingerprint).ok_or_else(|| {
        PlatformError::VerificationFailed("malformed Linux identity fingerprint".to_owned())
    })?;
    let expected_protocol = fields
        .get("driver")
        .is_some_and(|driver| matches!(driver.as_str(), "xpad" | "xpadneo"))
        .then_some(ControllerProtocol::Xusb)
        .unwrap_or(ControllerProtocol::HidGamepad);
    let usage_page = fields
        .get("usage_page")
        .and_then(|value| value.parse::<u16>().ok());
    let usage = fields
        .get("usage")
        .and_then(|value| value.parse::<u16>().ok());
    if identity.protocol != expected_protocol
        || identity.usage_page != usage_page.unwrap_or(0)
        || identity.usage != usage.unwrap_or(0)
    {
        return Err(PlatformError::IdentityMismatch(
            "controller protocol or HID usage evidence changed".to_owned(),
        ));
    }
    Ok(native)
}
fn coverage_for(controller: &input::Controller, helper_available: bool) -> ControlCoverage {
    if helper_available && controller.can_full_control {
        ControlCoverage::Full
    } else {
        ControlCoverage::Uncontrolled
    }
}

fn platform_device(
    controller: &input::Controller,
    coverage: ControlCoverage,
    enabled: bool,
    helper_available: bool,
) -> PlatformDevice {
    let connection = controller.connection.clone();
    let driver = controller.function.as_ref().and_then(|function| {
        function
            .driver
            .as_deref()
            .or(function.expected_driver.as_deref())
    });
    let protocol = if driver.is_some_and(|value| matches!(value, "xpad" | "xpadneo")) {
        ControllerProtocol::Xusb
    } else {
        ControllerProtocol::HidGamepad
    };
    let (usage_page, usage) = input::parse_fingerprint(&controller.identity.fingerprint)
        .and_then(|fields| {
            Some((
                fields.get("usage_page")?.parse::<u16>().ok()?,
                fields.get("usage")?.parse::<u16>().ok()?,
            ))
        })
        .unwrap_or((0, 0));
    let evidence_valid = usage_page == 1 && matches!(usage, 4 | 5 | 8);
    let capabilities = DeviceCapabilities {
        hid: true,
        gamepad: evidence_valid,
        joystick: evidence_valid,
        xinput: driver.is_some_and(|value| matches!(value, "xpad" | "xpadneo")),
        usb: connection == "usb",
        bluetooth: connection == "bluetooth",
        full_control: coverage == ControlCoverage::Full && evidence_valid,
    };
    let problem_code = if !evidence_valid {
        Some(PROBLEM_USAGE_EVIDENCE)
    } else if !helper_available {
        Some(PROBLEM_HELPER_MISSING)
    } else if coverage != ControlCoverage::Full {
        controller.problem_code.or(Some(PROBLEM_PARTIAL_FUNCTION))
    } else {
        controller.problem_code
    };
    PlatformDevice {
        id: DeviceId(controller.identity.stable_id.clone()),
        name: controller.name.clone(),
        connection,
        state: DeviceState { enabled },
        capabilities,
        coverage,
        problem_code,
        verified: Some(VerifiedDevice {
            platform: controller.identity.platform.clone(),
            stable_id: controller.identity.stable_id.clone(),
            instance_id: controller.identity.instance_id.clone(),
            fingerprint: controller.identity.fingerprint.clone(),
            protocol,
            coverage,
            usage_page,
            usage,
        }),
    }
}
fn mutation_warning(controller: &input::Controller) -> &'static str {
    let driver = controller.function.as_ref().and_then(|function| {
        function
            .driver
            .as_deref()
            .or(function.expected_driver.as_deref())
    });
    if driver.is_some_and(|value| matches!(value, "xpad" | "xpadneo")) {
        "Linux unbound the validated Xbox USB controller interface; direct libusb access to the USB parent is outside this coverage"
    } else {
        LIMITATION_MESSAGE
    }
}

fn control_problem(controller: &input::Controller) -> String {
    if controller.mixed_unrelated {
        "the input function also exposes an unrelated keyboard/mouse capability".to_owned()
    } else if let Some(function) = &controller.function {
        match function.driver.as_deref() {
            Some(driver) => {
                format!("kernel driver {driver:?} is not in the trusted controller allow-list")
            }
            None => "controller function has no bound kernel driver".to_owned(),
        }
    } else {
        "no stable HID or USB controller function was found".to_owned()
    }
}

fn wait_for_unbound(identity: &input::Identity) -> bool {
    for _ in 0..10 {
        if let Ok(function) = input::find_unbound_function(identity) {
            if function.driver.is_none() {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn wait_for_bound(identity: &input::Identity) -> bool {
    for _ in 0..20 {
        if input::discover().controllers.iter().any(|controller| {
            controller.identity == *identity
                && controller.can_full_control
                && controller
                    .function
                    .as_ref()
                    .and_then(|function| function.driver.as_deref())
                    .is_some_and(input::safe_driver)
        }) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn invoke_helper(action: &str, identity: &input::Identity) -> Result<(), PlatformError> {
    let helper = helper_path().ok_or_else(|| {
        PlatformError::PermissionDenied(
            "trusted Linux input helper is not installed; install the packaged polkit helper rather than granting the GUI root".to_owned(),
        )
    })?;
    if !trusted_pkexec() {
        return Err(PlatformError::PermissionDenied(
            "pkexec is unavailable or not trusted; an active polkit authorization agent is required".to_owned(),
        ));
    }
    if !matches!(action, "disable" | "enable") {
        return Err(PlatformError::OperationFailed(
            "internal helper action rejected".to_owned(),
        ));
    }
    let output = Command::new(PKEXEC)
        .arg(helper)
        .arg(format!("--{action}"))
        .arg("--platform")
        .arg("linux")
        .arg("--stable-id")
        .arg(&identity.stable_id)
        .arg("--instance-id")
        .arg(&identity.instance_id)
        .arg("--fingerprint")
        .arg(&identity.fingerprint)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| PlatformError::LaunchFailed(format!("{PKEXEC}: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = bounded_output(&output.stderr).or_else(|| bounded_output(&output.stdout));
    let message = detail.unwrap_or_else(|| format!("helper exited with status {}", output.status));
    if output.status.code() == Some(126) || output.status.code() == Some(127) {
        Err(PlatformError::PermissionDenied(message))
    } else {
        Err(PlatformError::OperationFailed(message))
    }
}

fn helper_path() -> Option<&'static str> {
    HELPER_PATHS
        .iter()
        .copied()
        .find(|path| trusted_root_executable(path))
}

fn trusted_pkexec() -> bool {
    trusted_root_executable(PKEXEC)
}

fn trusted_root_executable(path: &str) -> bool {
    let executable = std::path::Path::new(path);
    if !executable.is_absolute() {
        return false;
    }
    let Ok(metadata) = fs::symlink_metadata(executable) else {
        return false;
    };
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
    {
        return false;
    }
    let mut ancestor = executable.parent();
    while let Some(path) = ancestor {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return false;
        };
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return false;
        }
        if path == std::path::Path::new("/") {
            break;
        }
        ancestor = path.parent();
    }
    true
}

fn trusted_user_executable(path: &str) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

fn bounded_output(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let cleaned = text
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .collect::<String>()
        .trim()
        .to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}
