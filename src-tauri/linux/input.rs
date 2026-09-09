//! Linux controller discovery and function-level identity validation.
//!
//! This module intentionally operates on kernel function identities, never on
//! paths supplied by the UI.  A controller is eligible for the privileged
//! helper only when one controller-capable input function can be identified and
//! the function's driver is in the small allow-list below.  Keyboard, mouse,
//! and unrelated composite functions are refused.  The helper unbinds/rebinds
//! that one HID or USB interface, which covers evdev, joydev, hidraw, and
//! HIDAPI/SDL users of that function.  It does not unbind a USB parent device.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

#[path = "hid_descriptor.rs"]
mod hid_descriptor;

const SYS_INPUT: &str = "/sys/class/input";
const SYS_HID: &str = "/sys/bus/hid/devices";
const SYS_USB: &str = "/sys/bus/usb/devices";
const DEV_INPUT: &str = "/dev/input";
const JOURNAL_DIR: &str = "/var/lib/controwly";
const JOURNAL_PATH: &str = "/var/lib/controwly/disabled-functions";
const JOURNAL_LOCK_PATH: &str = "/var/lib/controwly/disabled-functions.lock";
pub(crate) const REMOVAL_GATE_PATH: &str = "/var/lib/controwly/removal-in-progress";

const MAX_ATTRIBUTE: usize = 4096;
const MAX_FINGERPRINT: usize = 2048;
const MAX_JOURNAL_BYTES: usize = 1024 * 1024;
const MAX_DESCRIPTOR_BYTES: usize = 128 * 1024;

const O_DIRECTORY: i32 = 0o200000;
const O_NOFOLLOW: i32 = 0o400000;

// Linux input event/key constants.  These are stable UAPI values and avoid a
// dependency on a C crate in both the GUI and the small privileged helper.
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_JOYSTICK: u16 = 0x120;
const BTN_GAMEPAD: u16 = 0x130;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;
const BTN_TOOL_FINGER: u16 = 0x145;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

const PROBLEM_UNCONTROLLED: i32 = 7101;
const PROBLEM_MIXED_FUNCTION: i32 = 7102;
const PROBLEM_UNSUPPORTED_DRIVER: i32 = 7103;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) platform: String,
    pub(crate) stable_id: String,
    pub(crate) instance_id: String,
    pub(crate) fingerprint: String,
}
pub(crate) fn valid_stable_id(value: &str) -> bool {
    valid_token(value, "linux-controller-")
}
fn ensure_journal_dir() -> Result<(), String> {
    let var = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open("/var")
        .map_err(|error| format!("cannot inspect Linux recovery ancestry: {error}"))?;
    validate_directory(&var, "/var")?;
    let parent = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open("/var/lib")
        .map_err(|error| format!("cannot inspect Linux recovery parent directory: {error}"))?;
    validate_directory(&parent, "/var/lib")?;

    match OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open(JOURNAL_DIR)
    {
        Ok(directory) => {
            validate_directory(&directory, JOURNAL_DIR)?;
            parent
                .sync_all()
                .map_err(|error| format!("cannot sync Linux recovery parent directory: {error}"))?;
            Ok(())
        }

        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(JOURNAL_DIR)
                .map_err(|error| format!("cannot create Linux recovery directory: {error}"))?;
            fs::set_permissions(JOURNAL_DIR, fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("cannot secure Linux recovery directory: {error}"))?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(O_DIRECTORY | O_NOFOLLOW)
                .open(JOURNAL_DIR)
                .map_err(|error| format!("cannot reopen Linux recovery directory: {error}"))?;
            validate_directory(&directory, JOURNAL_DIR)?;
            parent
                .sync_all()
                .map_err(|error| format!("cannot sync Linux recovery parent directory: {error}"))?;
            Ok(())
        }
        Err(error) => Err(format!("cannot inspect Linux recovery directory: {error}")),
    }
}

fn validate_directory(file: &File, path: &str) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {path}: {error}"))?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!("{path} is not a root-owned non-writable directory"));
    }
    Ok(())
}

fn validate_regular_file(file: &File, path: &str) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {path}: {error}"))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "{path} is not a root-owned non-writable regular file"
        ));
    }
    Ok(())
}

pub(crate) struct JournalLock {
    file: File,
}

impl JournalLock {
    pub(crate) fn acquire() -> Result<Self, String> {
        ensure_journal_dir()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW)
            .open(JOURNAL_LOCK_PATH)
            .map_err(|error| format!("cannot open Linux recovery lock: {error}"))?;
        validate_regular_file(&file, JOURNAL_LOCK_PATH)?;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
            return Err(format!(
                "cannot acquire Linux recovery lock: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self { file })
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        unsafe {
            let _ = flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

pub(crate) fn journal_read_locked(_lock: &JournalLock) -> Result<Vec<Identity>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(JOURNAL_PATH)
    {
        Ok(file) => {
            validate_regular_file(&file, JOURNAL_PATH)?;
            file
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot open Linux recovery journal: {error}")),
    };
    let mut contents = String::new();
    file.take((MAX_JOURNAL_BYTES + 1) as u64)
        .read_to_string(&mut contents)
        .map_err(|error| format!("cannot read Linux recovery journal: {error}"))?;
    if contents.len() > MAX_JOURNAL_BYTES {
        return Err("Linux recovery journal is oversized".to_owned());
    }
    let mut records = Vec::new();
    for line in contents.lines() {
        let mut fields = line.split('\t');
        let stable_id = fields
            .next()
            .ok_or_else(|| "malformed Linux recovery journal row".to_owned())?;
        let instance_id = fields
            .next()
            .ok_or_else(|| "malformed Linux recovery journal row".to_owned())?;
        let fingerprint = fields
            .next()
            .ok_or_else(|| "malformed Linux recovery journal row".to_owned())?;
        if fields.next().is_some() {
            return Err("malformed Linux recovery journal row".to_owned());
        }
        let identity = Identity {
            platform: "linux".to_owned(),
            stable_id: stable_id.to_owned(),
            instance_id: instance_id.to_owned(),
            fingerprint: fingerprint.to_owned(),
        };
        validate_identity(&identity)?;
        if records
            .iter()
            .any(|record: &Identity| record.stable_id == identity.stable_id)
        {
            return Err("duplicate Linux recovery journal identity".to_owned());
        }
        records.push(identity);
    }
    Ok(records)
}

pub(crate) fn journal_add_locked(_lock: &JournalLock, identity: &Identity) -> Result<(), String> {
    validate_identity(identity)?;
    let mut records = journal_read_locked(_lock)?;
    if let Some(existing) = records
        .iter()
        .find(|record| record.stable_id == identity.stable_id)
    {
        if existing != identity {
            return Err("Linux recovery journal identity conflict".to_owned());
        }
        return Ok(());
    }
    records.push(identity.clone());
    journal_write(&records)
}

pub(crate) fn journal_remove_locked(
    _lock: &JournalLock,
    identity: &Identity,
) -> Result<(), String> {
    validate_identity(identity)?;
    let mut records = journal_read_locked(_lock)?;
    let original_len = records.len();
    records.retain(|record| record.stable_id != identity.stable_id);
    if records.len() == original_len {
        return Ok(());
    }
    journal_write(&records)
}

fn journal_write(records: &[Identity]) -> Result<(), String> {
    ensure_journal_dir()?;
    let temporary = format!("{JOURNAL_PATH}.tmp.{}", std::process::id());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(&temporary)
        .map_err(|error| format!("cannot create Linux recovery journal temporary file: {error}"))?;
    validate_regular_file(&file, &temporary)?;
    for identity in records {
        validate_identity(identity)?;
        writeln!(
            file,
            "{}\t{}\t{}",
            identity.stable_id, identity.instance_id, identity.fingerprint
        )
        .map_err(|error| format!("cannot write Linux recovery journal: {error}"))?;
    }
    file.sync_all()
        .map_err(|error| format!("cannot sync Linux recovery journal: {error}"))?;
    drop(file);
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot set Linux recovery journal permissions: {error}"))?;
    fs::rename(&temporary, JOURNAL_PATH)
        .map_err(|error| format!("cannot atomically install Linux recovery journal: {error}"))?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open(JOURNAL_DIR)
        .map_err(|error| format!("cannot reopen Linux recovery directory: {error}"))?;
    validate_directory(&directory, JOURNAL_DIR)?;
    directory
        .sync_all()
        .map_err(|error| format!("cannot sync Linux recovery directory: {error}"))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FunctionBus {
    Hid,
    Usb,
}

impl FunctionBus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Hid => "hid",
            Self::Usb => "usb",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FunctionIdentity {
    pub(crate) bus: FunctionBus,
    /// Kernel function name (HID device name or USB interface name), not a
    /// user-provided path. It is validated before it is ever joined to sysfs.
    pub(crate) id: String,
    /// Driver currently bound to this function, if any.
    pub(crate) driver: Option<String>,
    /// Driver captured in a verified identity.  It remains available after
    /// unbind so a restore can bind the same approved driver back.
    pub(crate) expected_driver: Option<String>,
    pub(crate) properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct EventNode {
    pub(crate) name: String,
    pub(crate) is_gamepad: bool,
    pub(crate) is_touchpad: bool,
    pub(crate) is_keyboard: bool,
    pub(crate) is_mouse: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Controller {
    pub(crate) identity: Identity,
    pub(crate) name: String,
    pub(crate) connection: String,
    pub(crate) function: Option<FunctionIdentity>,
    pub(crate) mixed_unrelated: bool,
    pub(crate) can_full_control: bool,
    pub(crate) problem_code: Option<i32>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Discovery {
    pub(crate) controllers: Vec<Controller>,
    pub(crate) errors: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct FunctionError {
    pub(crate) message: String,
}

/// Discover controller-capable input functions.  This deliberately does not
/// use vendor allow-lists: capabilities and the kernel's physical function
/// identity determine classification.
pub(crate) fn discover() -> Discovery {
    let mut errors = Vec::new();
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut hid_usage_cache: BTreeMap<String, Option<(u16, u16)>> = BTreeMap::new();

    let entries = match fs::read_dir(SYS_INPUT) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!("cannot read {SYS_INPUT}: {error}"));
            return Discovery {
                controllers: Vec::new(),
                errors,
            };
        }
    };

    let mut event_entries = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            parse_event_name(&name).map(|number| (number, name, entry.path()))
        })
        .collect::<Vec<_>>();
    event_entries.sort_by_key(|(number, _, _)| *number);

    for (_number, event_name, class_path) in event_entries {
        let device_path = match fs::canonicalize(class_path.join("device")) {
            Ok(path) => path,
            Err(error) => {
                errors.push(format!(
                    "{event_name}: cannot resolve input device: {error}"
                ));
                continue;
            }
        };
        let name = read_attr(&device_path.join("name"))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Linux input device".to_owned());
        let keys = read_bitmap(&device_path.join("capabilities/key"), &mut errors);
        let abs = read_bitmap(&device_path.join("capabilities/abs"), &mut errors);
        let rel = read_bitmap(&device_path.join("capabilities/rel"), &mut errors);
        let ev = read_bitmap(&device_path.join("capabilities/ev"), &mut errors);
        let capability_usage = gamepad_usage(&keys, &abs);
        let is_touchpad = has_touchpad_capability(&keys, &abs);
        let is_keyboard = has_typing_keyboard_capability(&keys);

        let has_relative_pointer = rel.contains(&REL_X) || rel.contains(&REL_Y);
        let has_mouse_buttons = keys.iter().any(|code| {
            matches!(
                *code,
                BTN_LEFT | BTN_RIGHT | BTN_MIDDLE | BTN_SIDE | BTN_EXTRA
            )
        });
        // A real PlayStation-style touch surface may expose BTN_LEFT, but it
        // is not an unrelated pointer unless it also exposes relative axes.
        // Relative motion remains an unconditional isolation veto.
        let is_mouse = has_relative_pointer || (has_mouse_buttons && !is_touchpad);
        let is_event_device = ev.contains(&EV_KEY) || ev.contains(&EV_ABS);
        if !is_event_device {
            continue;
        }

        let function = find_function(&device_path, &name);
        let controller_usage = match function.as_ref() {
            Some(function) if function.bus == FunctionBus::Hid => {
                let key = function_key(function);
                if let Some(usage) = hid_usage_cache.get(&key) {
                    *usage
                } else {
                    let usage = hid_descriptor_controller_usage(&function.id).ok();
                    hid_usage_cache.insert(key, usage);
                    usage
                }
            }
            Some(_) | None => capability_usage,
        };
        let is_gamepad = controller_usage.is_some();
        let group_key = function
            .as_ref()
            .map(function_key)
            .unwrap_or_else(|| fallback_group_key(&device_path, &name));

        let group = groups
            .entry(group_key)
            .or_insert_with(|| Group::new(name.clone(), function.clone(), controller_usage));
        if group.controller_usage.is_none() {
            group.controller_usage = controller_usage;
        }
        group.nodes.push(EventNode {
            name,
            is_gamepad,
            is_touchpad,
            is_keyboard,
            is_mouse,
        });
    }

    let mut controllers = Vec::new();
    for (_key, group) in groups {
        if !group.nodes.iter().any(|node| node.is_gamepad) {
            continue;
        }
        // A keyboard or relative-pointer capability is an unconditional
        // isolation veto. In particular, a touchpad never excuses REL_X/Y.
        if group
            .nodes
            .iter()
            .any(|node| node.is_keyboard || node.is_mouse)
        {
            continue;
        }
        match group.into_controller() {
            Ok(controller) => controllers.push(controller),
            Err(error) => errors.push(error),
        }
    }
    controllers.sort_by(|left, right| left.identity.stable_id.cmp(&right.identity.stable_id));
    Discovery {
        controllers,
        errors,
    }
}

#[derive(Clone, Debug)]
struct Group {
    name: String,
    function: Option<FunctionIdentity>,
    controller_usage: Option<(u16, u16)>,
    nodes: Vec<EventNode>,
}

impl Group {
    fn new(
        name: String,
        function: Option<FunctionIdentity>,
        controller_usage: Option<(u16, u16)>,
    ) -> Self {
        Self {
            name,
            function,
            controller_usage,
            nodes: Vec::new(),
        }
    }

    fn into_controller(self) -> Result<Controller, String> {
        let gamepad_name = self
            .nodes
            .iter()
            .find(|node| node.is_gamepad)
            .map(|node| node.name.clone())
            .unwrap_or_else(|| self.name.clone());
        let mixed_unrelated = self
            .nodes
            .iter()
            .any(|node| node.is_keyboard || node.is_mouse);
        if !self
            .nodes
            .iter()
            .any(|node| node.is_gamepad || node.is_touchpad)
        {
            return Err("controller function had no controllable input nodes".to_owned());
        }

        let function = self.function;
        let properties = function
            .as_ref()
            .map(|function| function.properties.clone())
            .unwrap_or_default();
        let phys = properties.get("phys").cloned().unwrap_or_default();
        let uniq = properties.get("uniq").cloned().unwrap_or_default();
        let id = properties.get("id").cloned().unwrap_or_default();
        let connection = connection_from(&properties, function.as_ref());
        let function_token = function
            .as_ref()
            .map(|value| format!("{}:{}", value.bus.as_str(), value.id))
            .unwrap_or_else(|| "input:unknown".to_owned());
        let (usage_page, usage) = self
            .controller_usage
            .ok_or_else(|| "controller function lacked strict HID usage evidence".to_owned())?;
        let stable_key = stable_selection_key(
            function
                .as_ref()
                .map(|value| value.bus.as_str())
                .unwrap_or("input"),
            function
                .as_ref()
                .map(|value| value.id.as_str())
                .unwrap_or("unknown"),
            &properties,
            usage_page,
            usage,
        );
        let stable_id = format!("linux-controller-{}", digest128(&stable_key));
        let instance_id = format!("linux-function-{}", digest128(&function_token));
        let fingerprint = fingerprint_for(
            function.as_ref(),
            &gamepad_name,
            &phys,
            &uniq,
            &id,
            &connection,
            mixed_unrelated,
            usage_page,
            usage,
        );
        if fingerprint.len() > MAX_FINGERPRINT {
            return Err(format!("{gamepad_name}: identity fingerprint is too long"));
        }
        let can_full_control = function
            .as_ref()
            .map(|value| value.driver.as_deref().is_some_and(safe_driver))
            .unwrap_or(false)
            && !mixed_unrelated;
        let problem_code = if mixed_unrelated {
            Some(PROBLEM_MIXED_FUNCTION)
        } else if function.is_none() {
            Some(PROBLEM_UNCONTROLLED)
        } else if !can_full_control {
            Some(PROBLEM_UNSUPPORTED_DRIVER)
        } else {
            None
        };
        Ok(Controller {
            identity: Identity {
                platform: "linux".to_owned(),
                stable_id,
                instance_id,
                fingerprint,
            },
            name: sanitize_display(&gamepad_name),
            connection,
            function,
            mixed_unrelated,
            can_full_control,
            problem_code,
        })
    }
}

/// Locate the exact HID device or USB interface that owns an event function.
/// No path from the UI is used; only kernel-created names are accepted.
fn find_function(device_path: &Path, _name: &str) -> Option<FunctionIdentity> {
    let mut current = device_path.to_path_buf();
    let mut fallback_properties = BTreeMap::new();
    fallback_properties.insert(
        "name".to_owned(),
        read_attr(&device_path.join("name")).unwrap_or_else(|| "Linux input device".to_owned()),
    );
    fallback_properties.insert(
        "phys".to_owned(),
        read_attr(&device_path.join("phys")).unwrap_or_default(),
    );
    fallback_properties.insert(
        "uniq".to_owned(),
        read_attr(&device_path.join("uniq")).unwrap_or_default(),
    );
    fallback_properties.insert("id".to_owned(), input_id(&device_path.join("id")));

    loop {
        if let Some(file_name) = current.file_name().and_then(|value| value.to_str()) {
            let bus = if is_hid_name(file_name) {
                Some(FunctionBus::Hid)
            } else if is_usb_interface_name(file_name) {
                Some(FunctionBus::Usb)
            } else {
                None
            };
            if let Some(bus) = bus {
                let Ok(function_path) = canonical_function_path(bus, &current, file_name) else {
                    // An apparently function-shaped ancestor that fails the
                    // authoritative sysfs checks is never a valid target.
                    return None;
                };
                let properties = function_properties(&function_path, &fallback_properties);
                let driver = driver_name(&function_path);
                return Some(FunctionIdentity {
                    bus,
                    id: file_name.to_owned(),
                    expected_driver: driver.clone(),
                    driver,
                    properties,
                });
            }
        }
        if current == Path::new("/sys") || !current.pop() {
            break;
        }
    }
    None
}

fn function_properties(
    function_path: &Path,
    fallback: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    // Keep reads relative to the supplied function path. In particular, an
    // opened /proc/self/fd directory remains the authority during an xpad
    // authorization transition; canonicalization is used only for identity
    // and parent-name derivation.
    let resolved = fs::canonicalize(function_path).unwrap_or_else(|_| function_path.to_path_buf());
    let uevent = read_kv_file(&function_path.join("uevent"));
    let function_name = resolved
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let is_usb = is_usb_interface_name(function_name);
    if is_usb {
        let mut properties = BTreeMap::new();
        let parent = resolved.parent().unwrap_or_else(|| Path::new("/"));
        let parent_uevent = read_kv_file(&parent.join("uevent"));
        let physical = parent
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        properties.insert(
            "name".to_owned(),
            fallback.get("name").cloned().unwrap_or_default(),
        );
        properties.insert(
            "id".to_owned(),
            uevent.get("PRODUCT").cloned().unwrap_or_default(),
        );
        // USB interface uevent files do not expose the input child's phys or
        // uniq. Use the interface's own transport topology instead of leaking
        // an event-child value into the recovery identity.
        properties.insert("phys".to_owned(), physical);
        properties.insert("uniq".to_owned(), String::new());
        properties.insert("interface".to_owned(), function_name.to_owned());
        properties.insert("subsystem".to_owned(), "usb".to_owned());
        if let Some(devtype) = uevent.get("DEVTYPE").filter(|value| !value.is_empty()) {
            properties.insert("devtype".to_owned(), devtype.clone());
        }
        if let Some(interface_type) = uevent.get("TYPE").filter(|value| !value.is_empty()) {
            properties.insert("type".to_owned(), interface_type.clone());
        }
        if let Some(serial) = read_attr(&parent.join("serial"))
            .or_else(|| parent_uevent.get("SERIAL").cloned())
            .filter(|serial| !serial.is_empty())
        {
            properties.insert("serial".to_owned(), serial);
        }
        for key in ["BUSNUM", "DEVNUM"] {
            if let Some(value) = uevent
                .get(key)
                .or_else(|| parent_uevent.get(key))
                .filter(|value| !value.is_empty())
            {
                properties.insert(key.to_ascii_lowercase(), value.clone());
            }
        }
        if let Some(interface_descriptor) =
            uevent.get("INTERFACE").filter(|value| !value.is_empty())
        {
            properties.insert(
                "interface_descriptor".to_owned(),
                interface_descriptor.clone(),
            );
        }
        for (key, file) in [
            ("interface_class", "bInterfaceClass"),
            ("interface_subclass", "bInterfaceSubClass"),
            ("interface_protocol", "bInterfaceProtocol"),
        ] {
            if let Some(value) =
                read_attr(&function_path.join(file)).filter(|value| !value.is_empty())
            {
                properties.insert(key.to_owned(), value);
            }
        }
        for key in ["PRODUCT", "DEVTYPE", "TYPE"] {
            if let Some(value) = uevent.get(key).filter(|value| !value.is_empty()) {
                properties.insert(key.to_ascii_lowercase(), value.clone());
            }
        }
        return properties;
    }

    let mut properties = fallback.clone();
    let id = uevent
        .get("HID_ID")
        .cloned()
        .or_else(|| properties.get("id").cloned())
        .unwrap_or_default();
    properties.insert("id".to_owned(), id);
    properties.insert(
        "phys".to_owned(),
        uevent
            .get("HID_PHYS")
            .cloned()
            .or_else(|| properties.get("phys").cloned())
            .unwrap_or_default(),
    );
    properties.insert(
        "uniq".to_owned(),
        uevent
            .get("HID_UNIQ")
            .cloned()
            .or_else(|| properties.get("uniq").cloned())
            .unwrap_or_default(),
    );
    properties.insert(
        "name".to_owned(),
        uevent
            .get("HID_NAME")
            .cloned()
            .or_else(|| properties.get("name").cloned())
            .unwrap_or_default(),
    );
    for key in ["HID_ID", "HID_NAME", "HID_PHYS", "HID_UNIQ"] {
        if let Some(value) = uevent.get(key).filter(|value| !value.is_empty()) {
            properties.insert(key.to_ascii_lowercase(), value.clone());
        }
    }
    properties
}
fn canonical_function_path(
    bus: FunctionBus,
    path: &Path,
    expected_name: &str,
) -> Result<PathBuf, String> {
    if !valid_function_name(bus.as_str(), expected_name) {
        return Err("malformed kernel function name".to_owned());
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot resolve {bus:?} function: {error}"))?;
    let devices_root = fs::canonicalize("/sys/devices")
        .map_err(|error| format!("cannot resolve canonical sysfs root: {error}"))?;
    let bus_devices = Path::new(match bus {
        FunctionBus::Hid => SYS_HID,
        FunctionBus::Usb => SYS_USB,
    });
    let bus_alias = fs::canonicalize(bus_devices.join(expected_name))
        .map_err(|error| format!("cannot resolve canonical bus function alias: {error}"))?;
    if bus_alias != canonical
        || !canonical.starts_with(&devices_root)
        || canonical.file_name().and_then(|value| value.to_str()) != Some(expected_name)
    {
        return Err("function path is not the canonical fixed-bus sysfs function".to_owned());
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("cannot inspect function directory: {error}"))?;
    if !metadata.is_dir() {
        return Err("validated function is not a directory".to_owned());
    }
    let subsystem = fs::canonicalize(canonical.join("subsystem"))
        .map_err(|error| format!("cannot resolve function subsystem: {error}"))?;
    let subsystem_root = fs::canonicalize(match bus {
        FunctionBus::Hid => "/sys/bus/hid",
        FunctionBus::Usb => "/sys/bus/usb",
    })
    .map_err(|error| format!("cannot resolve expected function subsystem: {error}"))?;
    if subsystem != subsystem_root {
        return Err("function subsystem is not the expected HID/USB bus".to_owned());
    }
    let uevent = read_kv_file(&canonical.join("uevent"));
    match bus {
        FunctionBus::Hid if !uevent.get("HID_ID").is_some_and(|value| !value.is_empty()) => {
            return Err("HID function lacks authoritative HID_ID evidence".to_owned());
        }
        FunctionBus::Usb
            if uevent.get("DEVTYPE").map(String::as_str) != Some("usb_interface")
                || !uevent.get("PRODUCT").is_some_and(|value| !value.is_empty())
                || !uevent.get("TYPE").is_some_and(|value| !value.is_empty()) =>
        {
            return Err("USB function lacks authoritative interface/type evidence".to_owned());
        }
        _ => {}
    }
    Ok(canonical)
}

fn driver_name(path: &Path) -> Option<String> {
    let link = fs::read_link(path.join("driver")).ok()?;
    link.file_name()?.to_str().map(str::to_owned)
}

pub(crate) fn safe_driver(driver: &str) -> bool {
    matches!(
        driver,
        "hid-generic"
            | "hid-playstation"
            | "sony"
            | "xpad"
            | "xpadneo"
            | "hid-nintendo"
            | "hid-steam"
            | "usbhid"
    )
}

fn function_key(function: &FunctionIdentity) -> String {
    format!("{}:{}", function.bus.as_str(), function.id)
}
fn fallback_group_key(path: &Path, name: &str) -> String {
    let phys = read_attr(&path.join("phys")).unwrap_or_default();
    let uniq = read_attr(&path.join("uniq")).unwrap_or_default();
    format!("fallback:{phys}:{uniq}:{name}")
}
fn stable_function_name<'a>(bus: &str, function: &'a str) -> &'a str {
    if bus == "hid" {
        function
            .rsplit_once('.')
            .map(|(prefix, _)| prefix)
            .unwrap_or(function)
    } else {
        function
    }
}

fn stable_interface_name(
    bus: &str,
    function: &str,
    properties: &BTreeMap<String, String>,
) -> String {
    let candidate = properties
        .get("interface")
        .map(String::as_str)
        .unwrap_or(function);
    match bus {
        "hid" => stable_function_name(bus, candidate).to_owned(),
        "usb" => candidate
            .split_once(':')
            .map(|(_, interface)| interface)
            .unwrap_or(candidate)
            .to_owned(),
        _ => candidate.to_owned(),
    }
}

fn stable_selection_key(
    bus: &str,
    function: &str,
    properties: &BTreeMap<String, String>,
    usage_page: u16,
    usage: u16,
) -> String {
    let serial = if bus == "usb" {
        properties
            .get("serial")
            .or_else(|| properties.get("uniq"))
            .map(String::as_str)
            .unwrap_or("")
    } else {
        properties.get("uniq").map(String::as_str).unwrap_or("")
    };
    // A validated serial/uniq is the reconnect anchor. Only devices without
    // one need the topology path, so moving a serialised controller to another
    // USB port does not change its selection ID.
    // USB serials survive a port move, so they suppress the topology path in
    // the selection key. HID uniq values still need their physical function
    // path to distinguish multiple report functions on one device.
    let physical = if bus == "usb" && !serial.is_empty() {
        ""
    } else {
        properties.get("phys").map(String::as_str).unwrap_or("")
    };
    let interface = stable_interface_name(bus, function, properties);
    let transport = properties.get("id").map(String::as_str).unwrap_or("");
    format!(
        "bus={};transport={};serial={};phys={};interface={};usage={usage_page}:{usage}",
        encode_component(bus),
        encode_component(transport),
        encode_component(serial),
        encode_component(physical),
        encode_component(&interface),
    )
}

fn fingerprint_for(
    function: Option<&FunctionIdentity>,
    name: &str,
    phys: &str,
    uniq: &str,
    id: &str,
    connection: &str,
    mixed_unrelated: bool,
    usage_page: u16,
    usage: u16,
) -> String {
    let mut fields = vec![
        ("v", "2".to_owned()),
        (
            "bus",
            function
                .map(|value| value.bus.as_str())
                .unwrap_or("input")
                .to_owned(),
        ),
        (
            "function",
            function
                .map(|value| value.id.as_str())
                .unwrap_or("unknown")
                .to_owned(),
        ),
        (
            "driver",
            function
                .and_then(|value| value.driver.as_deref().or(value.expected_driver.as_deref()))
                .unwrap_or("")
                .to_owned(),
        ),
        ("name", name.to_owned()),
        ("phys", phys.to_owned()),
        ("uniq", uniq.to_owned()),
        ("id", id.to_owned()),
        ("connection", connection.to_owned()),
        (
            "mixed",
            if mixed_unrelated {
                "1".to_owned()
            } else {
                "0".to_owned()
            },
        ),
        ("usage_page", usage_page.to_string()),
        ("usage", usage.to_string()),
    ];
    if let Some(function) = function {
        for (key, value) in &function.properties {
            if !fields.iter().any(|(existing, _)| *existing == key.as_str()) {
                fields.push((key.as_str(), value.clone()));
            }
        }
    }
    fields
        .into_iter()
        .map(|(key, value)| format!("{key}={}", encode_component(&value)))
        .collect::<Vec<_>>()
        .join(";")
}

pub(crate) fn parse_fingerprint(fingerprint: &str) -> Option<BTreeMap<String, String>> {
    if fingerprint.len() > MAX_FINGERPRINT || !fingerprint.is_ascii() {
        return None;
    }
    let mut result = BTreeMap::new();
    for field in fingerprint.split(';') {
        let (key, encoded) = field.split_once('=')?;
        if key.is_empty() || !key.bytes().all(valid_key_byte) {
            return None;
        }
        if result
            .insert(key.to_owned(), decode_component(encoded)?)
            .is_some()
        {
            return None;
        }
    }
    if result.get("v").map(String::as_str) != Some("2") {
        return None;
    }
    Some(result)
}

pub(crate) fn validate_identity(identity: &Identity) -> Result<(), String> {
    if identity.platform != "linux" {
        return Err("identity platform is not linux".to_owned());
    }
    if !valid_token(&identity.stable_id, "linux-controller-")
        || !valid_token(&identity.instance_id, "linux-function-")
    {
        return Err("identity token is malformed".to_owned());
    }
    let fields = parse_fingerprint(&identity.fingerprint)
        .ok_or_else(|| "identity fingerprint is malformed".to_owned())?;
    let bus = fields.get("bus").map(String::as_str).unwrap_or("");
    let function = fields.get("function").map(String::as_str).unwrap_or("");
    if !matches!(bus, "hid" | "usb") || !valid_function_name(bus, function) {
        return Err("identity function token is malformed".to_owned());
    }
    if fields.get("mixed").map(String::as_str) != Some("0") {
        return Err("controller function has an unrelated keyboard/mouse sibling".to_owned());
    }
    let driver = fields.get("driver").map(String::as_str).unwrap_or("");
    if !safe_driver(driver) {
        return Err("controller function driver is not approved".to_owned());
    }
    if !fields.get("id").is_some_and(|value| !value.is_empty()) {
        return Err("controller transport identity is missing".to_owned());
    }
    if bus == "usb" {
        for key in [
            "phys",
            "interface",
            "subsystem",
            "devtype",
            "type",
            "busnum",
            "devnum",
        ] {
            if !fields.get(key).is_some_and(|value| !value.is_empty()) {
                return Err(format!("USB identity is missing {key} evidence"));
            }
        }
        let has_interface_descriptor = fields
            .get("interface_descriptor")
            .is_some_and(|value| !value.is_empty());
        let has_interface_tuple = [
            "interface_class",
            "interface_subclass",
            "interface_protocol",
        ]
        .iter()
        .all(|key| fields.get(*key).is_some_and(|value| !value.is_empty()));
        if !has_interface_descriptor && !has_interface_tuple {
            return Err("USB identity lacks interface class/protocol evidence".to_owned());
        }
    }
    let usage_page = fields.get("usage_page").map(String::as_str).unwrap_or("");
    let usage = fields.get("usage").map(String::as_str).unwrap_or("");
    if usage_page != "1" || !matches!(usage, "4" | "5" | "8") {
        return Err(
            "controller HID usage evidence is missing or not a gamepad/joystick".to_owned(),
        );
    }
    let usage_page_number = usage_page
        .parse::<u16>()
        .map_err(|_| "controller HID usage-page evidence is malformed".to_owned())?;
    let usage_number = usage
        .parse::<u16>()
        .map_err(|_| "controller HID usage evidence is malformed".to_owned())?;
    let function_token = format!("{bus}:{function}");
    let stable_key = stable_selection_key(bus, function, &fields, usage_page_number, usage_number);
    let expected_stable = format!("linux-controller-{}", digest128(&stable_key));
    let expected_instance = format!("linux-function-{}", digest128(&function_token));
    if identity.stable_id != expected_stable || identity.instance_id != expected_instance {
        return Err("identity digest does not match its immutable function fingerprint".to_owned());
    }
    Ok(())
}

pub(crate) fn identity_matches(controller: &Controller, identity: &Identity) -> bool {
    controller.identity == *identity
}

pub(crate) fn function_is_definitively_gone(identity: &Identity) -> bool {
    if validate_identity(identity).is_err() {
        return false;
    }
    let Some(fields) = parse_fingerprint(&identity.fingerprint) else {
        return false;
    };
    let Some(function_id) = fields.get("function") else {
        return false;
    };
    match fields.get("bus").map(String::as_str) {
        Some("hid") => {
            let function_path = Path::new(SYS_HID).join(function_id);
            matches!(
                fs::metadata(function_path),
                Err(error) if error.kind() == io::ErrorKind::NotFound
            )
        }
        Some("usb") => {
            let Some((device_id, _)) = function_id.split_once(':') else {
                return false;
            };
            if !is_usb_device_name(device_id) {
                return false;
            }
            let function_path = Path::new(SYS_USB).join(function_id);
            match fs::metadata(function_path) {
                Ok(_) => false,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let parent_path = Path::new(SYS_USB).join(device_id);
                    matches!(
                        fs::metadata(parent_path),
                        Err(error) if error.kind() == io::ErrorKind::NotFound
                    )
                }
                Err(_) => false,
            }
        }
        _ => false,
    }
}

pub(crate) fn find_unbound_function(identity: &Identity) -> Result<FunctionIdentity, String> {
    validate_identity(identity)?;
    let fields = parse_fingerprint(&identity.fingerprint)
        .ok_or_else(|| "malformed fingerprint".to_owned())?;
    let bus = match fields.get("bus").map(String::as_str) {
        Some("hid") => FunctionBus::Hid,
        Some("usb") => FunctionBus::Usb,
        _ => return Err("unsupported function bus".to_owned()),
    };
    let function_id = fields
        .get("function")
        .ok_or_else(|| "missing function identity".to_owned())?;
    if !valid_function_name(
        fields.get("bus").map(String::as_str).unwrap_or(""),
        function_id,
    ) {
        return Err("malformed function identity".to_owned());
    }
    let root = match bus {
        FunctionBus::Hid => Path::new(SYS_HID),
        FunctionBus::Usb => Path::new(SYS_USB),
    };
    let path = root.join(function_id);
    let metadata = fs::metadata(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            format!("function is not present: {error}")
        } else {
            format!("cannot inspect function identity: {error}")
        }
    })?;
    if !metadata.is_dir() {
        return Err("function identity is not a directory".to_owned());
    }
    let function_path = canonical_function_path(bus, &path, function_id)?;
    let fallback = BTreeMap::new();
    let properties = function_properties(&function_path, &fallback);
    compare_properties(bus, &fields, &properties)?;
    let expected_driver = fields
        .get("driver")
        .cloned()
        .filter(|value| !value.is_empty());
    let driver = driver_name(&function_path);
    Ok(FunctionIdentity {
        bus,
        id: function_id.clone(),
        driver,
        expected_driver,
        properties,
    })
}
pub(crate) fn controller_from_unbound(identity: &Identity) -> Result<Controller, String> {
    let function = find_unbound_function(identity)?;
    if function.driver.is_some() {
        return Err("controller function is still bound".to_owned());
    }
    let expected_driver = function
        .expected_driver
        .as_deref()
        .ok_or_else(|| "unbound controller has no persisted driver".to_owned())?;
    if !safe_driver(expected_driver) {
        return Err("unbound controller driver is not approved".to_owned());
    }
    let fields = parse_fingerprint(&identity.fingerprint)
        .ok_or_else(|| "malformed controller fingerprint".to_owned())?;
    let name = sanitize_display(
        fields
            .get("name")
            .map(String::as_str)
            .unwrap_or("Linux controller"),
    );
    let connection = fields
        .get("connection")
        .cloned()
        .unwrap_or_else(|| connection_from(&function.properties, Some(&function)));
    Ok(Controller {
        identity: identity.clone(),
        name,
        connection,
        function: Some(function),
        mixed_unrelated: false,
        can_full_control: true,
        problem_code: None,
    })
}

pub(crate) fn verify_hid_descriptor_identity(
    function_id: &str,
    fields: &BTreeMap<String, String>,
) -> Result<(), String> {
    let expected_page = fields
        .get("usage_page")
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "missing HID usage-page evidence".to_owned())?;
    let expected_usage = fields
        .get("usage")
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "missing HID usage evidence".to_owned())?;
    let (actual_page, actual_usage) = hid_descriptor_controller_usage(function_id)?;
    if expected_page != actual_page || expected_usage != actual_usage {
        return Err("HID report usage changed since enumeration".to_owned());
    }
    Ok(())
}

pub(crate) fn hid_descriptor_controller_usage(function_id: &str) -> Result<(u16, u16), String> {
    if !is_hid_name(function_id) {
        return Err("malformed HID function identity".to_owned());
    }
    let path = Path::new(SYS_HID).join(function_id);
    let function_path = canonical_function_path(FunctionBus::Hid, &path, function_id)?;
    let file = File::open(function_path.join("report_descriptor"))
        .map_err(|error| format!("cannot open HID report descriptor: {error}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_DESCRIPTOR_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read HID report descriptor: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err("HID report descriptor is unavailable or oversized".to_owned());
    }
    hid_descriptor::controller_usage(&bytes)
}

fn compare_properties(
    bus: FunctionBus,
    expected: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Result<(), String> {
    let keys: &[&str] = match bus {
        FunctionBus::Hid => &["id", "phys", "uniq"],
        FunctionBus::Usb => &[
            "id",
            "serial",
            "phys",
            "interface",
            "subsystem",
            "devtype",
            "type",
            "busnum",
            "devnum",
            "interface_descriptor",
            "interface_class",
            "interface_subclass",
            "interface_protocol",
        ],
    };
    for key in keys {
        let expected_value = expected.get(*key).map(String::as_str).unwrap_or("");
        if expected_value.is_empty() {
            continue;
        }
        let current_value = current.get(*key).map(String::as_str).unwrap_or("");
        if current_value.is_empty() {
            return Err(format!("function {key} identity is missing"));
        }
        if expected_value != current_value {
            return Err(format!("function {key} identity changed"));
        }
    }
    Ok(())
}

pub(crate) fn unbind(function: &FunctionIdentity) -> Result<(), FunctionError> {
    let driver = function.driver.as_deref().ok_or_else(|| FunctionError {
        message: "controller function has no bound kernel driver".to_owned(),
    })?;
    if !safe_driver(driver) {
        return Err(FunctionError {
            message: format!(
                "kernel driver {driver:?} is not approved for controller function control"
            ),
        });
    }
    if function.bus == FunctionBus::Usb && driver == "xpad" {
        return authorize_usb_interface(function, false);
    }
    write_binding(function.bus, driver, "unbind", &function.id)
}

pub(crate) fn bind(function: &FunctionIdentity) -> Result<(), FunctionError> {
    let driver = function
        .driver
        .as_deref()
        .or(function.expected_driver.as_deref())
        .ok_or_else(|| FunctionError {
            message: "persisted controller function has no approved driver".to_owned(),
        })?;
    if !safe_driver(driver) {
        return Err(FunctionError {
            message: format!(
                "kernel driver {driver:?} is not approved for controller function control"
            ),
        });
    }
    if function.bus == FunctionBus::Usb && driver == "xpad" {
        return authorize_usb_interface(function, true);
    }
    write_binding(function.bus, driver, "bind", &function.id)
}

fn write_binding(
    bus: FunctionBus,
    driver: &str,
    operation: &str,
    function_id: &str,
) -> Result<(), FunctionError> {
    if !valid_driver_token(driver) || !valid_function_name(bus.as_str(), function_id) {
        return Err(FunctionError {
            message: "validated kernel function token rejected".to_owned(),
        });
    }
    let root = match bus {
        FunctionBus::Hid => "/sys/bus/hid/drivers",
        FunctionBus::Usb => "/sys/bus/usb/drivers",
    };
    let path = Path::new(root).join(driver).join(operation);
    let canonical_root = fs::canonicalize(root).map_err(|error| FunctionError {
        message: format!("cannot verify kernel driver root: {error}"),
    })?;
    let canonical_driver =
        fs::canonicalize(Path::new(root).join(driver)).map_err(|error| FunctionError {
            message: format!("approved kernel driver is unavailable: {error}"),
        })?;
    if !canonical_driver.starts_with(&canonical_root) {
        return Err(FunctionError {
            message: "kernel driver path escaped the fixed sysfs root".to_owned(),
        });
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(map_io_error)?;
    file.write_all(function_id.as_bytes()).map_err(map_io_error)
}

fn authorize_usb_interface(
    function: &FunctionIdentity,
    enabled: bool,
) -> Result<(), FunctionError> {
    if function.bus != FunctionBus::Usb || !is_usb_interface_name(&function.id) {
        return Err(FunctionError {
            message: "USB interface identity failed strict validation".to_owned(),
        });
    }
    let expected_driver = function
        .driver
        .as_deref()
        .or(function.expected_driver.as_deref());
    if expected_driver != Some("xpad") {
        return Err(FunctionError {
            message: "USB authorization is restricted to the approved xpad driver".to_owned(),
        });
    }
    let device = Path::new(SYS_USB).join(&function.id);
    let canonical_device = canonical_function_path(FunctionBus::Usb, &device, &function.id)
        .map_err(|message| FunctionError { message })?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open(&canonical_device)
        .map_err(map_io_error)?;
    validate_directory(&directory, canonical_device.to_string_lossy().as_ref())
        .map_err(|message| FunctionError { message })?;
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let pinned_canonical = fs::canonicalize(&pinned).map_err(|error| FunctionError {
        message: format!("cannot pin USB interface identity: {error}"),
    })?;
    if pinned_canonical != canonical_device {
        return Err(FunctionError {
            message: "USB interface changed while being authorized".to_owned(),
        });
    }
    let current = function_properties(&pinned, &BTreeMap::new());
    compare_properties(FunctionBus::Usb, &function.properties, &current)
        .map_err(|message| FunctionError { message })?;

    let authorized_path = pinned.join("authorized");
    let value = if enabled { "1" } else { "0" };
    let mut authorized = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(O_NOFOLLOW)
        .open(&authorized_path)
        .map_err(map_io_error)?;
    validate_regular_file(&authorized, authorized_path.to_string_lossy().as_ref())
        .map_err(|message| FunctionError { message })?;
    authorized
        .write_all(value.as_bytes())
        .map_err(map_io_error)?;
    authorized.flush().map_err(map_io_error)?;

    let mut observed = String::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(&authorized_path)
        .map_err(map_io_error)?
        .take(MAX_ATTRIBUTE as u64)
        .read_to_string(&mut observed)
        .map_err(map_io_error)?;
    if observed.trim() != value {
        return Err(FunctionError {
            message: format!("USB interface authorization did not settle at {value}"),
        });
    }
    if enabled {
        // USB authorization must be followed by probing the exact interface;
        // never write a parent device, default, or arbitrary driver path.
        let mut probe = OpenOptions::new()
            .write(true)
            .open("/sys/bus/usb/drivers_probe")
            .map_err(map_io_error)?;
        probe
            .write_all(function.id.as_bytes())
            .map_err(map_io_error)?;
        probe.flush().map_err(map_io_error)?;
    }
    Ok(())
}

fn map_io_error(error: io::Error) -> FunctionError {
    FunctionError {
        message: error.to_string(),
    }
}

fn read_attr(path: &Path) -> Option<String> {
    let mut value = String::new();
    let file = File::open(path).ok()?;
    file.take(MAX_ATTRIBUTE as u64)
        .read_to_string(&mut value)
        .ok()?;
    Some(value.trim_end_matches(['\n', '\r']).to_owned())
}

fn read_kv_file(path: &Path) -> BTreeMap<String, String> {
    read_attr(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn input_id(path: &Path) -> String {
    let mut values = Vec::new();
    for field in ["bustype", "vendor", "product", "version"] {
        values.push(read_attr(&path.join(field)).unwrap_or_default());
    }
    values.join(":")
}

fn read_bitmap(path: &Path, errors: &mut Vec<String>) -> Vec<u16> {
    let Some(raw) = read_attr(path) else {
        return Vec::new();
    };
    let word_bits = std::mem::size_of::<usize>() * 8;
    let max_word_digits = word_bits / 4;
    let mut bits = Vec::new();
    for (word_index, word) in raw.split_whitespace().rev().enumerate() {
        if word.is_empty() || word.len() > max_word_digits {
            errors.push(format!("{}: malformed capability bitmap", path.display()));
            return Vec::new();
        }
        let Ok(value) = u64::from_str_radix(word, 16) else {
            errors.push(format!("{}: malformed capability bitmap", path.display()));
            return Vec::new();
        };
        for bit in 0..word_bits {
            if value & (1u64 << bit) != 0 {
                let index = word_index.saturating_mul(word_bits).saturating_add(bit);
                if index <= u16::MAX as usize {
                    bits.push(index as u16);
                }
            }
        }
    }
    bits.sort_unstable();
    bits.dedup();
    bits
}

fn gamepad_usage(keys: &[u16], abs: &[u16]) -> Option<(u16, u16)> {
    let joystick = keys
        .iter()
        .any(|code| *code >= BTN_JOYSTICK && *code < BTN_JOYSTICK + 0x20);
    let gamepad = keys
        .iter()
        .any(|code| *code >= BTN_GAMEPAD && *code < BTN_GAMEPAD + 0x20);
    let dpad = keys.iter().any(|code| {
        matches!(
            *code,
            BTN_DPAD_UP | BTN_DPAD_DOWN | BTN_DPAD_LEFT | BTN_DPAD_RIGHT
        )
    });
    let usage = if gamepad || dpad {
        0x05
    } else if joystick {
        0x04
    } else {
        return None;
    };
    (!abs.is_empty() || joystick).then_some((0x01, usage))
}

fn has_touchpad_capability(keys: &[u16], abs: &[u16]) -> bool {
    // Do not classify by vendor/name. A genuine controller touch surface must
    // expose both multitouch position axes and BTN_TOOL_FINGER. A single
    // unrelated ABS axis is not enough to excuse a pointer-like function.
    abs.contains(&ABS_MT_POSITION_X)
        && abs.contains(&ABS_MT_POSITION_Y)
        && keys.contains(&BTN_TOOL_FINGER)
}

fn has_typing_keyboard_capability(keys: &[u16]) -> bool {
    // KEY_* values through KEY_DELETE cover normal typing, modifiers,
    // function/numpad, and cursor/navigation keys. Preserve controller
    // auxiliary media/share keys above that range as legitimate buttons.
    keys.iter()
        .copied()
        .any(|code| code != 0 && (code <= 0x6f || matches!(code, 0x7d..=0x7f)))
}

fn connection_from(
    properties: &BTreeMap<String, String>,
    function: Option<&FunctionIdentity>,
) -> String {
    let lower = properties
        .values()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    if lower.contains("bluetooth")
        || lower.contains("0005:")
        || function
            .is_some_and(|value| value.bus == FunctionBus::Hid && value.id.starts_with("0005:"))
    {
        "bluetooth".to_owned()
    } else if function.is_some_and(|value| value.bus == FunctionBus::Usb)
        || lower.contains("usb")
        || lower.contains("0003:")
    {
        "usb".to_owned()
    } else {
        "other".to_owned()
    }
}

fn parse_event_name(name: &str) -> Option<u32> {
    name.strip_prefix("event")?.parse().ok()
}

fn is_hid_name(name: &str) -> bool {
    let parts = name.split([':', '.']).collect::<Vec<_>>();
    parts.len() == 4
        && parts[0].len() == 4
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() >= 1
        && parts[..3]
            .iter()
            .all(|part| part.chars().all(|c| c.is_ascii_hexdigit()))
        && parts[3].chars().all(|c| c.is_ascii_hexdigit())
}

fn is_decimal_token(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_usb_device_name(name: &str) -> bool {
    let mut parts = name.split('-');
    let Some(bus) = parts.next() else {
        return false;
    };
    if !is_decimal_token(bus) {
        return false;
    }
    let mut has_port_chain = false;
    for chain in parts {
        has_port_chain = true;
        if chain.split('.').any(|port| !is_decimal_token(port)) {
            return false;
        }
    }
    has_port_chain
}

fn is_usb_interface_name(name: &str) -> bool {
    let Some((device, interface)) = name.split_once(':') else {
        return false;
    };
    let Some((number, alternate)) = interface.split_once('.') else {
        return false;
    };
    is_usb_device_name(device) && is_decimal_token(number) && is_decimal_token(alternate)
}

fn valid_function_name(bus: &str, value: &str) -> bool {
    match bus {
        "hid" => is_hid_name(value),
        "usb" => is_usb_interface_name(value),
        _ => false,
    }
}

fn valid_driver_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_token(value: &str, prefix: &str) -> bool {
    value.len() <= 128
        && value.starts_with(prefix)
        && value[prefix.len()..].len() == 32
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn valid_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    encoded
}

fn decode_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            if !bytes[index].is_ascii_alphanumeric() && !matches!(bytes[index], b'.' | b'_' | b'-')
            {
                return None;
            }
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let high = from_hex(bytes[index + 1])?;
        let low = from_hex(bytes[index + 2])?;
        output.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(output).ok()
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + value - 10) as char,
    }
}

fn from_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn sanitize_display(value: &str) -> String {
    let filtered = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    if filtered.is_empty() {
        "Linux controller".to_owned()
    } else {
        filtered.chars().take(160).collect()
    }
}

fn digest128(value: &str) -> String {
    let mut first = 0xcbf29ce484222325u64;
    let mut second = 0x84222325cbf29ceu64;
    for byte in value.as_bytes() {
        first ^= u64::from(*byte);
        first = first.wrapping_mul(0x100000001b3);
        second ^= u64::from(*byte).rotate_left(17);
        second = second.wrapping_mul(0x100000001b3).rotate_left(7);
    }
    format!("{first:016x}{second:016x}")
}

const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn native_bitmap_preserves_controller_and_keyboard_mouse_bits() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("controwly-bitmap-{}-{nonce}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(b"1000000010000 0 0 0 40000000\n").unwrap();
        drop(file);
        let mut errors = Vec::new();
        let bits = read_bitmap(&path, &mut errors);
        fs::remove_file(path).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(bits, [0x1e, BTN_LEFT, BTN_GAMEPAD]);
        assert_eq!(gamepad_usage(&bits, &[0]), Some((1, 5)));
        assert!(has_typing_keyboard_capability(&bits));
    }
}
