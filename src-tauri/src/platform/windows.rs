//! Native Windows controller discovery and function-level enable/disable.
//!
//! The implementation deliberately works with HID *function* devnodes.  It
//! never treats the USB composite parent as the controller to mutate.  HID
//! preparsed data supplies the controller usage evidence, while the all-class
//! Configuration Manager walk keeps disabled function devnodes addressable for
//! recovery.

use super::{
    ControlCoverage, ControllerPlatform, ControllerProtocol, DeviceCapabilities, DeviceId,
    DeviceState, EnumerationReport, MutationReport, PlatformDevice, PlatformError, VerifiedDevice,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const PLATFORM: &str = "windows";
const STABLE_ID_PREFIX: &str = "windows-controller-";

const GENERIC_DESKTOP_PAGE: u16 = 0x01;
const USAGE_JOYSTICK: u16 = 0x04;
const USAGE_GAME_PAD: u16 = 0x05;
const USAGE_MULTI_AXIS: u16 = 0x08;
const USAGE_MOUSE: u16 = 0x02;
const USAGE_KEYBOARD: u16 = 0x06;
const USAGE_KEYPAD: u16 = 0x07;

// HIDP_LINK_COLLECTION_NODE.CollectionType values from hidpi.h.
const COLLECTION_PHYSICAL: u8 = 0;
const COLLECTION_APPLICATION: u8 = 1;
const COLLECTION_LOGICAL: u8 = 2;
const COLLECTION_REPORT: u8 = 3;
const COLLECTION_NAMED_ARRAY: u8 = 4;
const COLLECTION_USAGE_SWITCH: u8 = 5;
const COLLECTION_USAGE_MODIFIER: u8 = 6;

const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_ALLCLASSES: u32 = 0x0000_0004;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
const DIF_PROPERTYCHANGE: u32 = 0x0000_0012;
const DICS_ENABLE: u32 = 0x0000_0001;
const DICS_DISABLE: u32 = 0x0000_0002;
const DICS_FLAG_GLOBAL: u32 = 0x0000_0001;

const SPDRP_DEVICEDESC: u32 = 0x0000_0000;
const SPDRP_HARDWAREID: u32 = 0x0000_0001;
const SPDRP_SERVICE: u32 = 0x0000_0004;
const SPDRP_CLASS: u32 = 0x0000_0007;
const SPDRP_MFG: u32 = 0x0000_000B;
const SPDRP_FRIENDLYNAME: u32 = 0x0000_000C;
const SPDRP_ENUMERATOR_NAME: u32 = 0x0000_0016;

const ERROR_SUCCESS: u32 = 0;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NO_MORE_ITEMS: u32 = 259;
const CR_NO_SUCH_DEVNODE: u32 = 0x0000_000D;

const DN_STARTED: u32 = 0x0000_0008;
const DN_HAS_PROBLEM: u32 = 0x0000_0400;
const DN_WILL_BE_REMOVED: u32 = 0x0004_0000;
const DN_NEED_RESTART: u32 = 0x0000_0100;
const CM_PROB_DISABLED: u32 = 0x0000_0016;
const CM_PROB_NEED_RESTART: u32 = 0x0000_000E;

// These are intentionally platform-local problem codes.  The controller
// layer also supplies its generic partial/uncontrolled codes, but these values
// preserve useful native evidence in an EnumerationReport.
const PROBLEM_XINPUT_LIVE: i32 = 7302;
const PROBLEM_UNCONTROLLED_FUNCTION: i32 = 7303;
const PROBLEM_STATUS_UNKNOWN: i32 = 7304;

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
const SW_SHOWNORMAL: i32 = 1;
const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x0000_1000;
const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x0000_0200;
const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;

const GUID_DEVINTERFACE_HID: Guid = Guid {
    data1: 0x4D1E55B2,
    data2: 0xF16F,
    data3: 0x11CF,
    data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};
const GUID_DEVINTERFACE_XUSB: Guid = Guid {
    data1: 0xEC87F1E3,
    data2: 0xC13B,
    data3: 0x4100,
    data4: [0xB5, 0xF7, 0x8B, 0x84, 0xD5, 0x42, 0x60, 0xCB],
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpDevinfoData {
    cb_size: u32,
    class_guid: Guid,
    dev_inst: u32,
    reserved: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpDeviceInterfaceData {
    cb_size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpClassInstallHeader {
    cb_size: u32,
    install_function: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpPropchangeParams {
    class_install_header: SpClassInstallHeader,
    state_change: u32,
    scope: u32,
    hw_profile: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_report_byte_length: u16,
    output_report_byte_length: u16,
    feature_report_byte_length: u16,
    reserved: [u16; 17],
    number_link_collection_nodes: u16,
    number_input_button_caps: u16,
    number_input_value_caps: u16,
    number_input_data_indices: u16,
    number_output_button_caps: u16,
    number_output_value_caps: u16,
    number_output_data_indices: u16,
    number_feature_button_caps: u16,
    number_feature_value_caps: u16,
    number_feature_data_indices: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HidpLinkCollectionNode {
    link_usage: u16,
    link_usage_page: u16,
    parent: u16,
    number_of_children: u16,
    next_sibling: u16,
    first_child: u16,
    collection_type: u8,
    is_alias: u8,
    reserved: u16,
    user_context: *mut c_void,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut c_void,
    ) -> *mut c_void;
    fn FormatMessageW(
        flags: u32,
        source: *const c_void,
        message_id: u32,
        language_id: u32,
        buffer: *mut u16,
        size: u32,
        arguments: *const *const u16,
    ) -> u32;
    fn GetLastError() -> u32;
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        hwnd_parent: *mut c_void,
        flags: u32,
    ) -> *mut c_void;
    fn SetupDiDestroyDeviceInfoList(device_info_set: *mut c_void) -> i32;
    fn SetupDiEnumDeviceInfo(
        device_info_set: *mut c_void,
        member_index: u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
    fn SetupDiEnumDeviceInterfaces(
        device_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
        interface_class_guid: *const Guid,
        member_index: u32,
        device_interface_data: *mut SpDeviceInterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        device_info_set: *mut c_void,
        device_interface_data: *mut SpDeviceInterfaceData,
        device_interface_detail_data: *mut c_void,
        device_interface_detail_data_size: u32,
        required_size: *mut u32,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
    fn SetupDiGetDeviceInstanceIdW(
        device_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
        device_instance_id: *mut u16,
        device_instance_id_size: u32,
        required_size: *mut u32,
    ) -> i32;
    fn SetupDiGetDeviceRegistryPropertyW(
        device_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
        property: u32,
        property_reg_data_type: *mut u32,
        property_buffer: *mut u8,
        property_buffer_size: u32,
        required_size: *mut u32,
    ) -> i32;
    fn SetupDiSetClassInstallParamsW(
        device_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
        class_install_params: *mut SpClassInstallHeader,
        class_install_params_size: u32,
    ) -> i32;
    fn SetupDiCallClassInstaller(
        install_function: u32,
        device_info_set: *mut c_void,
        device_info_data: *mut SpDevinfoData,
    ) -> i32;
}

#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Get_DevNode_Status(
        status: *mut u32,
        problem_number: *mut u32,
        dev_inst: u32,
        flags: u32,
    ) -> u32;
    fn CM_Get_Parent(parent: *mut u32, dev_inst: u32, flags: u32) -> u32;
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidD_GetPreparsedData(device: *mut c_void, preparsed_data: *mut *mut c_void) -> u8;
    fn HidD_FreePreparsedData(preparsed_data: *mut c_void) -> u8;
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidP_GetCaps(preparsed_data: *mut c_void, capabilities: *mut HidpCaps) -> i32;
    fn HidP_GetLinkCollectionNodes(
        link_collection_nodes: *mut HidpLinkCollectionNode,
        link_collection_nodes_length: *mut u32,
        preparsed_data: *mut c_void,
    ) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: *mut c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show_command: i32,
    ) -> isize;
}

struct DeviceInfoSet {
    handle: *mut c_void,
}

impl DeviceInfoSet {
    fn all_classes() -> Result<Self, PlatformError> {
        Self::open(null(), DIGCF_ALLCLASSES)
    }
    fn hid_interfaces() -> Result<Self, PlatformError> {
        Self::open(
            &GUID_DEVINTERFACE_HID,
            DIGCF_DEVICEINTERFACE | DIGCF_PRESENT,
        )
    }

    fn open(class_guid: *const Guid, flags: u32) -> Result<Self, PlatformError> {
        let handle = unsafe { SetupDiGetClassDevsW(class_guid, null(), null_mut(), flags) };
        if handle.is_null() || handle == invalid_handle() {
            return Err(PlatformError::OperationFailed(last_error(
                "SetupDiGetClassDevsW",
            )));
        }
        Ok(Self { handle })
    }
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != invalid_handle() {
            unsafe {
                let _ = SetupDiDestroyDeviceInfoList(self.handle);
            }
        }
    }
}

struct WinHandle(*mut c_void);

impl WinHandle {
    fn open(path: &[u16], desired_access: u32) -> Option<Self> {
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                desired_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle.is_null() || handle == invalid_handle() {
            None
        } else {
            Some(Self(handle))
        }
    }
}

impl Drop for WinHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != invalid_handle() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct PreparsedData(*mut c_void);

impl Drop for PreparsedData {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = HidD_FreePreparsedData(self.0);
            }
        }
    }
}

fn invalid_handle() -> *mut c_void {
    (-1isize) as *mut c_void
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn clean_text(value: &str) -> String {
    value.trim_matches('\0').trim().to_owned()
}

fn normalized(value: &str) -> String {
    clean_text(value).to_ascii_uppercase()
}

fn format_error_code(context: &str, code: u32) -> String {
    let mut buffer = [0u16; 512];
    let length = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            null(),
            code,
            0,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            null(),
        )
    };
    let detail = if length == 0 {
        format!("Win32 error {code}")
    } else {
        String::from_utf16_lossy(&buffer[..length as usize])
            .trim()
            .to_owned()
    };
    format!("{context}: {detail} (0x{code:08X})")
}

fn last_error(context: &str) -> String {
    format_error_code(context, unsafe { GetLastError() })
}

fn registry_values(
    set: &DeviceInfoSet,
    data: &mut SpDevinfoData,
    property: u32,
) -> Option<Vec<String>> {
    let mut data_type = 0u32;
    let mut required = 0u32;
    let first = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.handle,
            data,
            property,
            &mut data_type,
            null_mut(),
            0,
            &mut required,
        )
    };
    if first != 0 && required == 0 {
        return None;
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_INSUFFICIENT_BUFFER || required == 0 {
        return None;
    }

    let count = (required as usize / 2).saturating_add(1);
    let mut buffer = vec![0u16; count];
    let mut written = 0u32;
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.handle,
            data,
            property,
            &mut data_type,
            buffer.as_mut_ptr() as *mut u8,
            (buffer.len() * 2) as u32,
            &mut written,
        )
    };
    if ok == 0 {
        return None;
    }

    let used = (written as usize / 2).min(buffer.len());
    let mut values = Vec::new();
    let mut start = 0usize;
    for index in 0..used {
        if buffer[index] == 0 {
            if index > start {
                let value = clean_text(&String::from_utf16_lossy(&buffer[start..index]));
                if !value.is_empty() {
                    values.push(value);
                }
            }
            start = index + 1;
        }
    }
    if start < used {
        let value = clean_text(&String::from_utf16_lossy(&buffer[start..used]));
        if !value.is_empty() {
            values.push(value);
        }
    }
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn device_instance_id(set: &DeviceInfoSet, data: &mut SpDevinfoData) -> Result<String, String> {
    let mut required = 0u32;
    let first =
        unsafe { SetupDiGetDeviceInstanceIdW(set.handle, data, null_mut(), 0, &mut required) };
    if first != 0 && required == 0 {
        return Err("SetupDiGetDeviceInstanceIdW returned no instance ID".to_owned());
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_INSUFFICIENT_BUFFER || required == 0 {
        return Err(last_error("SetupDiGetDeviceInstanceIdW"));
    }
    let mut buffer = vec![0u16; required as usize + 1];
    let ok = unsafe {
        SetupDiGetDeviceInstanceIdW(
            set.handle,
            data,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut required,
        )
    };
    if ok == 0 {
        return Err(last_error("SetupDiGetDeviceInstanceIdW"));
    }
    let end = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    let value = clean_text(&String::from_utf16_lossy(&buffer[..end]));
    if value.is_empty() {
        Err("SetupAPI returned an empty device instance ID".to_owned())
    } else {
        Ok(normalized(&value))
    }
}

#[derive(Clone, Debug)]
struct RawDevnode {
    devinst: u32,
    instance: String,
    class_name: String,
    service: String,
    enumerator: String,
    hardware_ids: Vec<String>,
    friendly_name: String,
    description: String,
    manufacturer: String,
    parent: Option<u32>,
    xusb_interface: bool,
    status: u32,
    problem: Option<i32>,
    status_known: bool,
}

impl RawDevnode {
    fn is_composite_parent(&self) -> bool {
        let class = normalized(&self.class_name);
        let service = normalized(&self.service);
        (class == "USB" && service == "USBCCGP")
            || class == "USB COMPOSITE"
            || (class.contains("COMPOSITE") && !class.contains("XNA") && !class.contains("XUSB"))
    }

    fn is_hid_function(&self) -> bool {
        let class = normalized(&self.class_name);
        class == "HIDCLASS" || class.contains("HID")
    }

    fn is_explicit_keyboard_or_mouse(&self) -> bool {
        let class = normalized(&self.class_name);
        let service = normalized(&self.service);
        matches!(
            class.as_str(),
            "KEYBOARD" | "MOUSE" | "KEYBOARDCLASS" | "MOUSECLASS"
        ) || matches!(
            service.as_str(),
            "KBDCLASS" | "KBDHID" | "MOUCLASS" | "MOUHID"
        )
    }
    fn administratively_disabled(&self) -> bool {
        self.status_known
            && self.status & DN_HAS_PROBLEM != 0
            && self.problem == Some(CM_PROB_DISABLED as i32)
    }

    fn enabled(&self) -> Option<bool> {
        if !self.status_known {
            return None;
        }
        Some(
            if self.status & DN_HAS_PROBLEM != 0 && self.problem == Some(CM_PROB_DISABLED as i32) {
                false
            } else {
                self.status & DN_STARTED != 0
            },
        )
    }
}

fn read_raw_devnode(set: &DeviceInfoSet, data: &mut SpDevinfoData) -> Result<RawDevnode, String> {
    let instance = device_instance_id(set, data)?;
    let class_name = registry_values(set, data, SPDRP_CLASS)
        .and_then(|values| values.into_iter().next())
        .unwrap_or_default();
    let service = registry_values(set, data, SPDRP_SERVICE)
        .and_then(|values| values.into_iter().next())
        .unwrap_or_default();
    let enumerator = registry_values(set, data, SPDRP_ENUMERATOR_NAME)
        .and_then(|values| values.into_iter().next())
        .unwrap_or_default();
    let hardware_ids = registry_values(set, data, SPDRP_HARDWAREID)
        .unwrap_or_default()
        .into_iter()
        .map(|value| normalized(&value))
        .collect();
    let friendly_name = registry_values(set, data, SPDRP_FRIENDLYNAME)
        .and_then(|values| values.into_iter().next())
        .unwrap_or_default();
    let description = registry_values(set, data, SPDRP_DEVICEDESC)
        .and_then(|values| values.into_iter().next())
        .unwrap_or_default();
    let manufacturer = registry_values(set, data, SPDRP_MFG)
        .and_then(|values| values.into_iter().next())
        .unwrap_or_default();

    let mut status = 0u32;
    let mut problem = 0u32;
    let status_code = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.dev_inst, 0) };
    let status_known = status_code == ERROR_SUCCESS;
    let problem = if status_known && problem != 0 {
        Some(problem as i32)
    } else {
        None
    };

    let mut parent = 0u32;
    let parent = match unsafe { CM_Get_Parent(&mut parent, data.dev_inst, 0) } {
        ERROR_SUCCESS => Some(parent),
        CR_NO_SUCH_DEVNODE => None,
        code => {
            return Err(format!(
                "CM_Get_Parent failed for devnode {} (CONFIGRET 0x{code:08X})",
                data.dev_inst
            ));
        }
    };

    Ok(RawDevnode {
        devinst: data.dev_inst,
        instance,
        class_name,
        service,
        enumerator,
        hardware_ids,
        friendly_name,
        description,
        manufacturer,
        parent,
        xusb_interface: false,
        status,
        problem,
        status_known,
    })
}

fn collect_all_devnodes() -> Result<(Vec<RawDevnode>, Vec<String>), PlatformError> {
    let set = DeviceInfoSet::all_classes()?;
    let mut nodes = Vec::new();
    let mut index = 0u32;
    loop {
        let mut data: SpDevinfoData = unsafe { std::mem::zeroed() };
        data.cb_size = std::mem::size_of::<SpDevinfoData>() as u32;
        let ok = unsafe { SetupDiEnumDeviceInfo(set.handle, index, &mut data) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(PlatformError::OperationFailed(last_error(
                "SetupDiEnumDeviceInfo",
            )));
        }
        match read_raw_devnode(&set, &mut data) {
            Ok(node) => {
                if node.status & DN_WILL_BE_REMOVED == 0 {
                    nodes.push(node);
                }
            }
            Err(error) => {
                return Err(PlatformError::OperationFailed(format!(
                    "all-class devnode {index} could not be read: {error}"
                )));
            }
        }
        index = index.saturating_add(1);
    }
    Ok((nodes, Vec::new()))
}

#[derive(Clone, Debug)]
struct HidAggregate {
    usages: BTreeSet<(u16, u16)>,
    allowed: bool,
    disallowed: bool,
    mixed: bool,
}

impl HidAggregate {
    fn new() -> Self {
        Self {
            usages: BTreeSet::new(),
            allowed: false,
            disallowed: false,
            mixed: false,
        }
    }

    fn add(&mut self, usage_page: u16, usage: u16, mixed: bool) {
        if is_allowed_usage(usage_page, usage) {
            self.allowed = true;
            self.usages.insert((usage_page, usage));
        } else if usage_page == GENERIC_DESKTOP_PAGE
            && (usage == USAGE_MOUSE || usage == USAGE_KEYBOARD)
        {
            self.disallowed = true;
        }
        self.mixed |= mixed;
        if self.usages.len() > 1 {
            self.mixed = true;
        }
    }
    fn mark_inspection_failed(&mut self) {
        self.mixed = true;
    }

    fn usage(&self) -> Option<(u16, u16)> {
        self.usages.iter().next().copied()
    }
}

fn is_allowed_usage(usage_page: u16, usage: u16) -> bool {
    usage_page == GENERIC_DESKTOP_PAGE
        && matches!(usage, USAGE_JOYSTICK | USAGE_GAME_PAD | USAGE_MULTI_AXIS)
}
fn classify_hid_collections(
    nodes: &[HidpLinkCollectionNode],
    usage_page: u16,
    usage: u16,
) -> Result<bool, String> {
    let Some(root) = nodes.first() else {
        return Err("HID link-collection array has no top-level collection".to_owned());
    };
    if root.parent != 0 {
        return Err("HID link-collection array root has a parent".to_owned());
    }

    // Microsoft documents array element zero as the top-level collection.
    // Parent == 0 is also the encoded parent index for that root's immediate
    // children, so it cannot be used to count top-level collections.
    let mut mixed = !is_allowed_usage(usage_page, usage)
        || root.link_usage_page != usage_page
        || root.link_usage != usage;
    if !matches!(
        root.collection_type,
        COLLECTION_PHYSICAL
            | COLLECTION_APPLICATION
            | COLLECTION_LOGICAL
            | COLLECTION_REPORT
            | COLLECTION_NAMED_ARRAY
            | COLLECTION_USAGE_SWITCH
            | COLLECTION_USAGE_MODIFIER
    ) {
        mixed = true;
    }
    if root.collection_type == COLLECTION_APPLICATION
        && !is_allowed_usage(root.link_usage_page, root.link_usage)
    {
        mixed = true;
    }
    if root.link_usage_page == GENERIC_DESKTOP_PAGE
        && matches!(root.link_usage, USAGE_MOUSE | USAGE_KEYBOARD | USAGE_KEYPAD)
    {
        mixed = true;
    }

    let mut visited = vec![false; nodes.len()];
    visited[0] = true;
    let mut pending = vec![0usize];
    while let Some(parent_index) = pending.pop() {
        let parent = &nodes[parent_index];
        let mut child_index = parent.first_child as usize;
        let mut child_count = 0usize;
        let mut siblings = BTreeSet::new();

        while child_index != 0 {
            if child_index >= nodes.len() {
                return Err(format!(
                    "HID link-collection node {parent_index} points to child index {child_index} outside the array"
                ));
            }
            if !siblings.insert(child_index) {
                return Err(format!(
                    "HID link-collection node {parent_index} has a cyclic sibling list"
                ));
            }
            if visited[child_index] {
                return Err(format!(
                    "HID link-collection node {child_index} is reachable more than once"
                ));
            }

            let child = &nodes[child_index];
            // The root's index is zero, and therefore an immediate child's
            // Parent field is also zero. Deeper nodes carry their real parent
            // index.
            if child.parent as usize != parent_index {
                return Err(format!(
                    "HID link-collection node {child_index} disagrees with its parent's child link"
                ));
            }
            visited[child_index] = true;
            child_count = child_count.saturating_add(1);

            if !matches!(
                child.collection_type,
                COLLECTION_PHYSICAL
                    | COLLECTION_APPLICATION
                    | COLLECTION_LOGICAL
                    | COLLECTION_REPORT
                    | COLLECTION_NAMED_ARRAY
                    | COLLECTION_USAGE_SWITCH
                    | COLLECTION_USAGE_MODIFIER
            ) {
                mixed = true;
            } else if child.collection_type == COLLECTION_APPLICATION
                && !is_allowed_usage(child.link_usage_page, child.link_usage)
            {
                // A nested Application collection is a separate functional
                // collection. Physical and Logical Pointer groupings are not.
                mixed = true;
            }

            pending.push(child_index);
            child_index = child.next_sibling as usize;
        }

        if child_count != parent.number_of_children as usize {
            return Err(format!(
                "HID link-collection node {parent_index} reports {} children but links {child_count}",
                parent.number_of_children
            ));
        }
    }

    if visited.iter().any(|seen| !seen) {
        return Err("HID link-collection array contains an unreachable node".to_owned());
    }
    Ok(mixed)
}

fn inspect_hid_path(path: &str) -> Result<(u16, u16, bool), String> {
    let path = wide(path);
    let handle = WinHandle::open(&path, GENERIC_READ)
        .or_else(|| WinHandle::open(&path, 0))
        .ok_or_else(|| last_error("CreateFileW HID interface"))?;

    let mut preparsed = null_mut();
    let ok = unsafe { HidD_GetPreparsedData(handle.0, &mut preparsed) };
    if ok == 0 || preparsed.is_null() {
        return Err(last_error("HidD_GetPreparsedData"));
    }
    let preparsed = PreparsedData(preparsed);
    let mut caps = HidpCaps::default();
    let status = unsafe { HidP_GetCaps(preparsed.0, &mut caps) };
    if status != HIDP_STATUS_SUCCESS {
        return Err(format!(
            "HidP_GetCaps failed with HID status 0x{status:08X}"
        ));
    }

    let count = caps.number_link_collection_nodes as usize;
    if count == 0 || count > 4096 {
        return Err(
            "HID preparsed data did not provide a bounded link-collection array".to_owned(),
        );
    }
    let mut nodes = vec![unsafe { std::mem::zeroed::<HidpLinkCollectionNode>() }; count];
    let mut length = caps.number_link_collection_nodes as u32;
    let status =
        unsafe { HidP_GetLinkCollectionNodes(nodes.as_mut_ptr(), &mut length, preparsed.0) };
    if status != HIDP_STATUS_SUCCESS || length == 0 || length as usize > nodes.len() {
        return Err(format!(
            "HidP_GetLinkCollectionNodes failed with HID status 0x{status:08X}"
        ));
    }

    let mixed = classify_hid_collections(&nodes[..length as usize], caps.usage_page, caps.usage)?;
    Ok((caps.usage_page, caps.usage, mixed))
}

fn collect_hid_observations() -> Result<(BTreeMap<u32, HidAggregate>, Vec<String>), PlatformError> {
    let set = DeviceInfoSet::hid_interfaces()?;
    const PATH_START: usize = 4;
    let mut observations = BTreeMap::new();
    let mut errors = Vec::new();
    let mut index = 0u32;

    loop {
        let mut interface_data: SpDeviceInterfaceData = unsafe { std::mem::zeroed() };
        interface_data.cb_size = std::mem::size_of::<SpDeviceInterfaceData>() as u32;
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                set.handle,
                null_mut(),
                &GUID_DEVINTERFACE_HID,
                index,
                &mut interface_data,
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(PlatformError::OperationFailed(last_error(
                "SetupDiEnumDeviceInterfaces(HID)",
            )));
        }

        let mut required = 0u32;
        let first = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.handle,
                &mut interface_data,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            )
        };
        let first_error = unsafe { GetLastError() };
        if first != 0
            || required == 0
            || required > 1_048_576
            || first_error != ERROR_INSUFFICIENT_BUFFER
        {
            return Err(PlatformError::OperationFailed(format!(
                "HID interface {index}: {}",
                format_error_code("SetupDiGetDeviceInterfaceDetailW sizing", first_error)
            )));
        }
        if (required as usize) < PATH_START {
            return Err(PlatformError::OperationFailed(format!(
                "HID interface {index}: SetupDiGetDeviceInterfaceDetailW returned a truncated detail buffer"
            )));
        }

        let mut detail = vec![0u8; required as usize];
        let cb_size = if cfg!(target_pointer_width = "64") {
            8u32
        } else {
            6u32
        };
        unsafe {
            std::ptr::write_unaligned(detail.as_mut_ptr() as *mut u32, cb_size);
        }
        let mut devinfo: SpDevinfoData = unsafe { std::mem::zeroed() };
        devinfo.cb_size = std::mem::size_of::<SpDevinfoData>() as u32;
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.handle,
                &mut interface_data,
                detail.as_mut_ptr() as *mut c_void,
                detail.len() as u32,
                &mut required,
                &mut devinfo,
            )
        };
        if ok == 0 {
            return Err(PlatformError::OperationFailed(format!(
                "HID interface {index}: {}",
                last_error("SetupDiGetDeviceInterfaceDetailW")
            )));
        }

        let path_end = detail[PATH_START..]
            .chunks_exact(2)
            .position(|chunk| chunk[0] == 0 && chunk[1] == 0)
            .map(|position| PATH_START + position * 2)
            .unwrap_or(detail.len());
        if path_end <= PATH_START || (path_end - PATH_START) % 2 != 0 {
            return Err(PlatformError::OperationFailed(format!(
                "HID interface {index} devinst {} returned a malformed device path",
                devinfo.dev_inst
            )));
        }
        let path_words = unsafe {
            std::slice::from_raw_parts(
                detail.as_ptr().add(PATH_START) as *const u16,
                (path_end - PATH_START) / 2,
            )
        };
        let path = String::from_utf16_lossy(path_words);
        match inspect_hid_path(&path) {
            Ok((usage_page, usage, mixed)) => {
                observations
                    .entry(devinfo.dev_inst)
                    .or_insert_with(HidAggregate::new)
                    .add(usage_page, usage, mixed);
            }
            Err(error) => {
                observations
                    .entry(devinfo.dev_inst)
                    .or_insert_with(HidAggregate::new)
                    .mark_inspection_failed();
                errors.push(format!(
                    "HID interface {index} devinst {} could not be inspected: {error}",
                    devinfo.dev_inst
                ));
            }
        }
        index = index.saturating_add(1);
    }
    Ok((observations, errors))
}

fn collect_xusb_interfaces() -> Result<(BTreeSet<u32>, Vec<String>), PlatformError> {
    let set = DeviceInfoSet::open(&GUID_DEVINTERFACE_XUSB, DIGCF_DEVICEINTERFACE)?;
    let mut devinsts = BTreeSet::new();
    let mut errors = Vec::new();
    let mut index = 0u32;
    loop {
        let mut interface_data: SpDeviceInterfaceData = unsafe { std::mem::zeroed() };
        interface_data.cb_size = std::mem::size_of::<SpDeviceInterfaceData>() as u32;
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                set.handle,
                null_mut(),
                &GUID_DEVINTERFACE_XUSB,
                index,
                &mut interface_data,
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            errors.push(last_error("SetupDiEnumDeviceInterfaces(XUSB)"));
            break;
        }

        let mut required = 0u32;
        let first = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.handle,
                &mut interface_data,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            )
        };
        let first_error = unsafe { GetLastError() };
        if first != 0
            || required == 0
            || required > 1_048_576
            || first_error != ERROR_INSUFFICIENT_BUFFER
        {
            errors.push(format!(
                "XUSB interface {index}: {}",
                format_error_code("SetupDiGetDeviceInterfaceDetailW sizing", first_error,)
            ));
            index = index.saturating_add(1);
            continue;
        }
        if required < 4 {
            errors.push(format!(
                "XUSB interface {index}: SetupDiGetDeviceInterfaceDetailW returned a truncated detail buffer"
            ));
            index = index.saturating_add(1);
            continue;
        }
        let mut detail = vec![0u8; required as usize];
        let cb_size = if cfg!(target_pointer_width = "64") {
            8u32
        } else {
            6u32
        };
        unsafe {
            std::ptr::write_unaligned(detail.as_mut_ptr() as *mut u32, cb_size);
        }
        let mut devinfo: SpDevinfoData = unsafe { std::mem::zeroed() };
        devinfo.cb_size = std::mem::size_of::<SpDevinfoData>() as u32;
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.handle,
                &mut interface_data,
                detail.as_mut_ptr() as *mut c_void,
                detail.len() as u32,
                &mut required,
                &mut devinfo,
            )
        };
        if ok != 0 {
            devinsts.insert(devinfo.dev_inst);
        } else {
            errors.push(format!(
                "XUSB interface {index}: {}",
                last_error("SetupDiGetDeviceInterfaceDetailW")
            ));
        }
        index = index.saturating_add(1);
    }
    Ok((devinsts, errors))
}

fn xusb_direct(node: &RawDevnode) -> bool {
    !node.is_composite_parent() && !node.is_hid_function() && node.xusb_interface
}
fn xusb_recovery_native(node: &RawDevnode) -> bool {
    if xusb_direct(node) || node.is_composite_parent() || node.is_hid_function() {
        return xusb_direct(node);
    }
    let class = normalized(&node.class_name);
    let service = normalized(&node.service);
    matches!(
        class.as_str(),
        "XUSB" | "XNA" | "XNACOMPOSITE" | "XNA COMPOSITE"
    ) || matches!(
        service.as_str(),
        "XUSB" | "XUSB21" | "XUSB22" | "XUSB23" | "XUSB24"
    )
}

fn node_by_devinst<'a>(all: &'a [RawDevnode], devinst: u32) -> Option<&'a RawDevnode> {
    all.iter().find(|candidate| candidate.devinst == devinst)
}

fn is_descendant_of(candidate: &RawDevnode, ancestor_devinst: u32, all: &[RawDevnode]) -> bool {
    let mut current = candidate.parent;
    let mut visited = BTreeSet::new();
    while let Some(parent_devinst) = current {
        if parent_devinst == ancestor_devinst {
            return true;
        }
        if !visited.insert(parent_devinst) {
            return false;
        }
        current = node_by_devinst(all, parent_devinst).and_then(|parent| parent.parent);
    }
    false
}

fn exact_xusb_root_devinst(node: &RawDevnode, all: &[RawDevnode]) -> Option<u32> {
    if xusb_direct(node) {
        return Some(node.devinst);
    }
    let mut current = node.parent;
    let mut visited = BTreeSet::new();
    while let Some(parent_devinst) = current {
        if !visited.insert(parent_devinst) {
            return None;
        }
        let parent = node_by_devinst(all, parent_devinst)?;
        if xusb_direct(parent) {
            return Some(parent.devinst);
        }
        current = parent.parent;
    }
    None
}
fn xusb_relation_root_devinst(
    node: &RawDevnode,
    all: &[RawDevnode],
    allow_recovery: bool,
) -> Option<u32> {
    if xusb_direct(node) || (allow_recovery && xusb_recovery_native(node)) {
        return Some(node.devinst);
    }
    let mut current = node.parent;
    let mut visited = BTreeSet::new();
    while let Some(parent_devinst) = current {
        if !visited.insert(parent_devinst) {
            return None;
        }
        let parent = node_by_devinst(all, parent_devinst)?;
        if xusb_direct(parent) || (allow_recovery && xusb_recovery_native(parent)) {
            return Some(parent.devinst);
        }
        current = parent.parent;
    }
    None
}

fn xusb_related<'a>(node: &RawDevnode, all: &'a [RawDevnode]) -> Vec<&'a RawDevnode> {
    let Some(root_devinst) = exact_xusb_root_devinst(node, all) else {
        return Vec::new();
    };
    all.iter()
        .filter(|candidate| {
            xusb_direct(candidate)
                && (candidate.devinst == root_devinst
                    || is_descendant_of(candidate, root_devinst, all))
        })
        .collect()
}

fn xusb_companion_hint(node: &RawDevnode) -> bool {
    let instance = normalized(&node.instance);
    instance.contains("&IG_00") || instance.contains("\\IG_00") || instance.ends_with("IG_00")
}

fn has_verified_hid_descendant(
    candidate: &RawDevnode,
    all: &[RawDevnode],
    hid: &BTreeMap<u32, HidAggregate>,
) -> bool {
    all.iter().any(|descendant| {
        descendant.devinst != candidate.devinst
            && descendant.is_hid_function()
            && descendant.status_known
            && is_descendant_of(descendant, candidate.devinst, all)
            && hid
                .get(&descendant.devinst)
                .map(|aggregate| aggregate.allowed && !aggregate.disallowed && !aggregate.mixed)
                .unwrap_or(false)
    })
}

fn related_unsafe_xusb_descendant(
    node: &RawDevnode,
    all: &[RawDevnode],
    hid: &BTreeMap<u32, HidAggregate>,
    allow_unobserved_hid: bool,
) -> bool {
    let Some(root_devinst) = xusb_relation_root_devinst(node, all, allow_unobserved_hid) else {
        return false;
    };
    all.iter().any(|candidate| {
        candidate.devinst != root_devinst
            && candidate.devinst != node.devinst
            && is_descendant_of(candidate, root_devinst, all)
            && if !candidate.status_known {
                true
            } else if let Some(aggregate) = hid.get(&candidate.devinst) {
                // An observed aggregate is authoritative regardless of the
                // class label on its devnode.
                !aggregate.allowed || aggregate.disallowed || aggregate.mixed
            } else if candidate.is_hid_function() {
                // HID enumeration is PRESENT-only. A missing HID aggregate
                // may be a transport parent with an observed controller leaf,
                // or a vanished leaf during disabled recovery; an active
                // unobserved leaf remains unsafe.
                !allow_unobserved_hid && !has_verified_hid_descendant(candidate, all, hid)
            } else {
                // A non-HID descendant without collection evidence is an
                // unknown function in the exact XUSB subtree.
                true
            }
    })
}
fn related_observed_xusb_conflict(
    node: &RawDevnode,
    all: &[RawDevnode],
    hid: &BTreeMap<u32, HidAggregate>,
) -> bool {
    let Some(root_devinst) = xusb_relation_root_devinst(node, all, true) else {
        return false;
    };
    all.iter().any(|candidate| {
        candidate.devinst != root_devinst
            && candidate.devinst != node.devinst
            && is_descendant_of(candidate, root_devinst, all)
            && (candidate.is_explicit_keyboard_or_mouse()
                || hid
                    .get(&candidate.devinst)
                    .map(|aggregate| !aggregate.allowed || aggregate.disallowed || aggregate.mixed)
                    .unwrap_or(false))
    })
}

fn related_mixed_hid_function(
    node: &RawDevnode,
    all: &[RawDevnode],
    hid: &BTreeMap<u32, HidAggregate>,
) -> bool {
    let Some(root_devinst) = exact_xusb_root_devinst(node, all) else {
        // Independent HID PDOs have their own mutation boundary. Siblings
        // under a transport/hub are not companions of this exact TLC.
        return false;
    };
    all.iter().any(|candidate| {
        candidate.devinst != node.devinst
            && candidate.is_hid_function()
            && is_descendant_of(candidate, root_devinst, all)
            && hid
                .get(&candidate.devinst)
                .map(|aggregate| !aggregate.allowed || aggregate.disallowed || aggregate.mixed)
                .unwrap_or(false)
    })
}

fn connection_label(node: &RawDevnode, all: &[RawDevnode]) -> String {
    let mut current = Some(node);
    let mut visited = BTreeSet::new();
    for _ in 0..8 {
        let Some(candidate) = current else {
            break;
        };
        if !visited.insert(candidate.devinst) {
            break;
        }
        let instance = normalized(&candidate.instance);
        let enumerator = normalized(&candidate.enumerator);
        let hardware = candidate
            .hardware_ids
            .iter()
            .map(|value| normalized(value))
            .collect::<Vec<_>>();
        if instance.starts_with("USB\\")
            || enumerator == "USB"
            || hardware.iter().any(|value| value.starts_with("USB\\"))
        {
            return "USB".to_owned();
        }
        if instance.starts_with("BTH\\")
            || instance.starts_with("BTHENUM\\")
            || enumerator.contains("BTH")
            || hardware
                .iter()
                .any(|value| value.contains("BLUETOOTH") || value.starts_with("BTH"))
        {
            return "Bluetooth".to_owned();
        }
        current = candidate
            .parent
            .and_then(|parent| all.iter().find(|item| item.devinst == parent));
    }
    "Other".to_owned()
}

fn display_name(node: &RawDevnode, usage: u16, hid_function: bool) -> String {
    let mut name = [
        clean_text(&node.friendly_name),
        clean_text(&node.description),
        clean_text(&node.manufacturer),
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    if name.is_empty() {
        if !hid_function {
            return "XUSB/XInput controller".to_owned();
        }
        name = match usage {
            USAGE_JOYSTICK => "HID joystick".to_owned(),
            USAGE_GAME_PAD => "HID game pad".to_owned(),
            USAGE_MULTI_AXIS => "HID multi-axis controller".to_owned(),
            _ => "HID controller".to_owned(),
        };
    }
    name
}

fn hex_bytes(value: &str) -> String {
    let mut result = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        result.push_str(&format!("{byte:02X}"));
    }
    result
}

fn stable_id(instance: &str) -> String {
    format!("{STABLE_ID_PREFIX}{}", hex_bytes(&normalized(instance)))
}

fn fnv64(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn fingerprint(node: &RawDevnode, usage_page: u16, usage: u16) -> String {
    let mut hardware = node.hardware_ids.clone();
    hardware.sort();
    let canonical = format!(
        "{}|{}|{}|{}|{}|{:04X}|{:04X}",
        normalized(&node.instance),
        normalized(&node.class_name),
        normalized(&node.service),
        normalized(&node.enumerator),
        hardware.join(","),
        usage_page,
        usage
    );
    let bytes = canonical.as_bytes();
    let first = fnv64(0xCBF2_9CE4_8422_2325, bytes);
    let second = fnv64(0x8422_2325_CBF2_9CE4, bytes);
    format!("windows-fingerprint-{first:016X}{second:016X}")
}

fn control_limitation(coverage: ControlCoverage) -> Option<&'static str> {
    match coverage {
        ControlCoverage::Full => None,
        ControlCoverage::HidOnly => Some(
            "only the validated HID function is controllable; the XInput/XUSB path remains live",
        ),
        ControlCoverage::Uncontrolled => Some(
            "controller function coverage is ambiguous; the XInput/XUSB path remains live and disable is refused",
        ),
    }
}

fn platform_device(
    node: &RawDevnode,
    identity: VerifiedDevice,
    hid_function: bool,
    all: &[RawDevnode],
) -> Option<PlatformDevice> {
    let connection = connection_label(node, all);
    let coverage = identity.coverage;
    let problem_code = if node.administratively_disabled() {
        None
    } else if let Some(problem) = node.problem {
        Some(problem)
    } else if node.status_known && node.status & DN_HAS_PROBLEM != 0 {
        Some(PROBLEM_STATUS_UNKNOWN)
    } else {
        match coverage {
            ControlCoverage::Full => None,
            ControlCoverage::HidOnly => Some(PROBLEM_XINPUT_LIVE),
            ControlCoverage::Uncontrolled => Some(PROBLEM_UNCONTROLLED_FUNCTION),
        }
    };
    let enabled = node.enabled()?;
    let name = display_name(node, identity.usage, hid_function);
    Some(PlatformDevice {
        id: DeviceId(identity.stable_id.clone()),
        name,
        connection: connection.clone(),
        state: DeviceState { enabled },
        capabilities: DeviceCapabilities {
            hid: hid_function,
            gamepad: identity.protocol == ControllerProtocol::Xusb
                || matches!(identity.usage, USAGE_GAME_PAD | USAGE_MULTI_AXIS),
            joystick: matches!(identity.usage, USAGE_JOYSTICK | USAGE_MULTI_AXIS),
            xinput: identity.protocol == ControllerProtocol::Xusb,
            usb: connection == "USB",
            bluetooth: connection == "Bluetooth",
            full_control: coverage == ControlCoverage::Full,
        },
        coverage,
        problem_code,
        verified: Some(identity),
    })
}

#[derive(Clone, Debug)]
struct ControllerRecord {
    node: RawDevnode,
    identity: VerifiedDevice,
    device: PlatformDevice,
}

#[derive(Clone, Debug)]
struct Inventory {
    nodes: Vec<RawDevnode>,
    hid: BTreeMap<u32, HidAggregate>,
    records: Vec<ControllerRecord>,
    errors: Vec<String>,
}

fn build_record(
    node: &RawDevnode,
    aggregate: &HidAggregate,
    nodes: &[RawDevnode],
    all_hid: &BTreeMap<u32, HidAggregate>,
) -> Option<ControllerRecord> {
    if !node.status_known
        || !aggregate.allowed
        || aggregate.disallowed
        || aggregate.mixed
        || related_mixed_hid_function(node, nodes, all_hid)
        || node.is_composite_parent()
    {
        return None;
    }
    let (usage_page, usage) = aggregate.usage()?;
    let xusb_nodes = xusb_related(node, nodes);
    let has_xusb = !xusb_nodes.is_empty();
    let exact_xusb = xusb_direct(node);
    let allow_unobserved_hid = node.administratively_disabled()
        || exact_xusb_root_devinst(node, nodes)
            .and_then(|root| node_by_devinst(nodes, root))
            .map(|root| root.administratively_disabled())
            .unwrap_or(false);
    let unsafe_hid = related_unsafe_xusb_descendant(node, nodes, all_hid, allow_unobserved_hid);
    if has_xusb && !unsafe_hid {
        // The native XUSB function is the authoritative row for a
        // controller-only HID companion; do not offer a duplicate HID row.
        return None;
    }
    let protocol = if has_xusb {
        ControllerProtocol::Xusb
    } else {
        ControllerProtocol::HidGamepad
    };
    let coverage = if has_xusb {
        if exact_xusb && !unsafe_hid {
            ControlCoverage::Full
        } else {
            ControlCoverage::Uncontrolled
        }
    } else if xusb_companion_hint(node) {
        ControlCoverage::Uncontrolled
    } else if node.is_hid_function() {
        ControlCoverage::Full
    } else {
        ControlCoverage::Uncontrolled
    };
    let identity = VerifiedDevice {
        platform: PLATFORM.to_owned(),
        stable_id: stable_id(&node.instance),
        instance_id: node.instance.clone(),
        fingerprint: fingerprint(node, usage_page, usage),
        protocol,
        coverage,
        usage_page,
        usage,
    };
    let device = platform_device(node, identity.clone(), true, nodes)?;
    Some(ControllerRecord {
        node: node.clone(),
        identity,
        device,
    })
}

fn build_xusb_record(
    node: &RawDevnode,
    nodes: &[RawDevnode],
    all_hid: &BTreeMap<u32, HidAggregate>,
) -> Option<ControllerRecord> {
    if !node.status_known
        || !xusb_direct(node)
        || node.is_composite_parent()
        || all_hid.contains_key(&node.devinst)
    {
        return None;
    }
    let coverage =
        if related_unsafe_xusb_descendant(node, nodes, all_hid, node.administratively_disabled()) {
            ControlCoverage::Uncontrolled
        } else {
            ControlCoverage::Full
        };
    // XUSB is not a HID collection.  Zero usage fields preserve that fact;
    // the exact GUID_DEVINTERFACE_XUSB devinst membership above is the
    // accepted controller evidence for this non-HID function.
    let usage_page = 0;
    let usage = 0;
    let identity = VerifiedDevice {
        platform: PLATFORM.to_owned(),
        stable_id: stable_id(&node.instance),
        instance_id: node.instance.clone(),
        fingerprint: fingerprint(node, usage_page, usage),
        protocol: ControllerProtocol::Xusb,
        coverage,
        usage_page,
        usage,
    };
    let device = platform_device(node, identity.clone(), false, nodes)?;
    Some(ControllerRecord {
        node: node.clone(),
        identity,
        device,
    })
}

fn scan_inventory() -> Result<Inventory, PlatformError> {
    let (mut nodes, mut errors) = collect_all_devnodes()?;
    let (xusb_devinsts, xusb_errors) = collect_xusb_interfaces()?;
    if !xusb_errors.is_empty() {
        return Err(PlatformError::OperationFailed(format!(
            "XUSB interface enumeration was not authoritative: {}",
            xusb_errors.join("; ")
        )));
    }
    for node in &mut nodes {
        node.xusb_interface = xusb_devinsts.contains(&node.devinst);
    }
    let (hid, hid_errors) = collect_hid_observations()?;
    errors.extend(hid_errors);
    let by_devinst: BTreeMap<u32, &RawDevnode> =
        nodes.iter().map(|node| (node.devinst, node)).collect();
    let mut records = Vec::new();
    for (devinst, aggregate) in &hid {
        let Some(node) = by_devinst.get(devinst).copied() else {
            errors.push(format!(
                "HID devnode {devinst} was not present in the all-class scan"
            ));
            continue;
        };
        if !node.status_known {
            if aggregate.allowed {
                errors.push(format!(
                    "HID controller devnode {devinst} has no authoritative Configuration Manager status"
                ));
            }
            continue;
        }
        if let Some(record) = build_record(node, aggregate, &nodes, &hid) {
            records.push(record);
        }
    }
    for node in &nodes {
        if !node.status_known {
            if node.xusb_interface {
                errors.push(format!(
                    "XUSB controller devnode {} has no authoritative Configuration Manager status",
                    node.devinst
                ));
            }
            continue;
        }
        if let Some(record) = build_xusb_record(node, &nodes, &hid) {
            records.push(record);
        }
    }
    records.sort_by(|left, right| left.identity.stable_id.cmp(&right.identity.stable_id));
    records.dedup_by(|left, right| left.identity.stable_id == right.identity.stable_id);
    Ok(Inventory {
        nodes,
        hid,
        records,
        errors,
    })
}

fn identity_shape_is_valid(identity: &VerifiedDevice) -> bool {
    let usage_valid = is_allowed_usage(identity.usage_page, identity.usage)
        || (identity.protocol == ControllerProtocol::Xusb
            && identity.usage_page == 0
            && identity.usage == 0);
    identity.platform == PLATFORM
        && identity.is_valid()
        && usage_valid
        && identity.stable_id == stable_id(&identity.instance_id)
}

fn reconstructed_record(
    node: &RawDevnode,
    identity: &VerifiedDevice,
    inventory: &Inventory,
) -> Result<ControllerRecord, PlatformError> {
    if !node.status_known || !node.administratively_disabled() {
        return Err(PlatformError::IdentityMismatch(format!(
            "{}: recovery requires an authoritative CM_PROB_DISABLED devnode",
            identity.stable_id
        )));
    }
    if fingerprint(node, identity.usage_page, identity.usage) != identity.fingerprint {
        return Err(PlatformError::IdentityMismatch(format!(
            "{}: immutable Windows function properties changed",
            identity.stable_id
        )));
    }
    if node.is_composite_parent() {
        return Err(PlatformError::IdentityMismatch(format!(
            "{}: persisted identity resolves to a USB composite parent",
            identity.stable_id
        )));
    }
    if identity.protocol == ControllerProtocol::HidGamepad
        && identity.coverage == ControlCoverage::Full
        && xusb_companion_hint(node)
    {
        return Err(PlatformError::IdentityMismatch(format!(
            "{}: IG_00 HID companion lacks exact XUSB interface evidence",
            identity.stable_id
        )));
    }
    let related_xusb = xusb_related(node, &inventory.nodes);
    let persisted_xusb_full_target = identity.protocol == ControllerProtocol::Xusb
        && identity.coverage == ControlCoverage::Full
        && (xusb_direct(node) || xusb_recovery_native(node));
    let detected_protocol = if !related_xusb.is_empty() || persisted_xusb_full_target {
        ControllerProtocol::Xusb
    } else {
        ControllerProtocol::HidGamepad
    };
    if detected_protocol != identity.protocol {
        return Err(PlatformError::IdentityMismatch(format!(
            "{}: controller protocol evidence changed",
            identity.stable_id
        )));
    }
    if identity.protocol == ControllerProtocol::Xusb {
        match identity.coverage {
            ControlCoverage::Full => {
                if !persisted_xusb_full_target {
                    return Err(PlatformError::IdentityMismatch(format!(
                        "{}: exact XUSB function is no longer safely identifiable",
                        identity.stable_id
                    )));
                }
                if related_observed_xusb_conflict(node, &inventory.nodes, &inventory.hid) {
                    return Err(PlatformError::IdentityMismatch(format!(
                        "{}: observed XUSB descendant evidence is no longer controller-only",
                        identity.stable_id
                    )));
                }
            }
            ControlCoverage::HidOnly => {
                if !node.is_hid_function() || related_xusb.is_empty() {
                    return Err(PlatformError::IdentityMismatch(format!(
                        "{}: persisted partial XUSB identity lost its validated HID/XUSB pair",
                        identity.stable_id
                    )));
                }
            }
            ControlCoverage::Uncontrolled => {
                if !xusb_direct(node) {
                    return Err(PlatformError::IdentityMismatch(format!(
                        "{}: persisted XUSB identity is not the exact XUSB function devnode",
                        identity.stable_id
                    )));
                }
            }
        }
    } else if !node.is_hid_function() {
        return Err(PlatformError::IdentityMismatch(format!(
            "{}: persisted HID identity no longer resolves to its exact HID function",
            identity.stable_id
        )));
    }
    let device = platform_device(
        node,
        identity.clone(),
        node.is_hid_function(),
        &inventory.nodes,
    )
    .ok_or_else(|| {
        PlatformError::VerificationFailed(format!(
            "{}: authoritative status disappeared during identity reconstruction",
            identity.stable_id
        ))
    })?;
    Ok(ControllerRecord {
        node: node.clone(),
        identity: identity.clone(),
        device,
    })
}
fn locate_identity(
    inventory: &Inventory,
    identity: &VerifiedDevice,
) -> Result<ControllerRecord, PlatformError> {
    if !identity_shape_is_valid(identity) {
        return Err(PlatformError::VerificationFailed(
            "malformed or cross-platform Windows controller identity".to_owned(),
        ));
    }
    let node = inventory
        .nodes
        .iter()
        .find(|node| node.instance == normalized(&identity.instance_id))
        .ok_or_else(|| PlatformError::NotFound(identity.stable_id.clone()))?;

    // A disabled native function can legitimately lose its live HID
    // descendants. Reconstruct the persisted Full identity from the exact
    // disabled devnode before comparing dynamic coverage fields.
    if identity.coverage == ControlCoverage::Full && node.administratively_disabled() {
        return reconstructed_record(node, identity, inventory);
    }

    if let Some(record) = inventory
        .records
        .iter()
        .find(|record| record.identity.stable_id == identity.stable_id)
    {
        if !record.identity.same_hardware(identity) {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: fresh HID evidence does not match the verified identity",
                identity.stable_id
            )));
        }
        return Ok(record.clone());
    }

    reconstructed_record(node, identity, inventory)
}

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_CANCELLED: u32 = 1223;
const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;
const ERROR_REBOOT_REQUIRED: u32 = 3010;
const DN_NEEDS_RESTART: u32 = 0x0000_0100;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationPlan {
    Noop,
    NativeChange,
}

fn mutation_plan(
    node: &RawDevnode,
    current_enabled: bool,
    requested_enabled: bool,
) -> Result<MutationPlan, ()> {
    if current_enabled == requested_enabled {
        if !requested_enabled && !node.administratively_disabled() {
            return Err(());
        }
        return Ok(MutationPlan::Noop);
    }
    Ok(MutationPlan::NativeChange)
}

fn property_change_error(context: &str) -> PlatformError {
    let code = unsafe { GetLastError() };
    let message = last_error(context);
    match code {
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => PlatformError::PermissionDenied(format!(
            "{message}; administrator permission is required to change this device"
        )),
        ERROR_CANCELLED | ERROR_OPERATION_ABORTED => PlatformError::OperationFailed(format!(
            "{message}; the device or another system component vetoed the property change"
        )),
        ERROR_REBOOT_REQUIRED => PlatformError::OperationFailed(format!(
            "{message}; Windows requires a restart before the requested state can be confirmed"
        )),
        _ => PlatformError::OperationFailed(message),
    }
}

fn apply_property_change(
    identity: &VerifiedDevice,
    expected_devinst: u32,
    all_nodes: &[RawDevnode],
    all_hid: &BTreeMap<u32, HidAggregate>,
    enabled: bool,
) -> Result<(), PlatformError> {
    let set = DeviceInfoSet::all_classes()?;
    let mut index = 0u32;
    loop {
        let mut data: SpDevinfoData = unsafe { std::mem::zeroed() };
        data.cb_size = std::mem::size_of::<SpDevinfoData>() as u32;
        let ok = unsafe { SetupDiEnumDeviceInfo(set.handle, index, &mut data) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(PlatformError::OperationFailed(last_error(
                "SetupDiEnumDeviceInfo",
            )));
        }
        let mut current = match read_raw_devnode(&set, &mut data) {
            Ok(current) => current,
            Err(_) => {
                index = index.saturating_add(1);
                continue;
            }
        };
        current.xusb_interface = all_nodes
            .iter()
            .find(|node| node.devinst == current.devinst)
            .map(|node| node.xusb_interface)
            .unwrap_or(false);
        if current.devinst != expected_devinst || current.instance != identity.instance_id {
            index = index.saturating_add(1);
            continue;
        }
        if !current.status_known {
            return Err(PlatformError::VerificationFailed(format!(
                "{}: Configuration Manager could not provide an authoritative target status",
                identity.stable_id
            )));
        }
        if fingerprint(&current, identity.usage_page, identity.usage) != identity.fingerprint {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: function identity changed before property change",
                identity.stable_id
            )));
        }
        if current.is_composite_parent() {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: refusing to mutate a USB composite parent",
                identity.stable_id
            )));
        }
        let persisted_xusb_full_enable_target = enabled
            && identity.protocol == ControllerProtocol::Xusb
            && identity.coverage == ControlCoverage::Full
            && (xusb_direct(&current) || xusb_recovery_native(&current));
        let target_ok = match (identity.protocol, identity.coverage) {
            (ControllerProtocol::HidGamepad, _) => current.is_hid_function(),
            (ControllerProtocol::Xusb, ControlCoverage::Full) => {
                xusb_direct(&current) || persisted_xusb_full_enable_target
            }
            (ControllerProtocol::Xusb, ControlCoverage::HidOnly) => current.is_hid_function(),
            (ControllerProtocol::Xusb, ControlCoverage::Uncontrolled) => xusb_direct(&current),
        };
        if !target_ok {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: verified identity does not resolve to its expected HID/XUSB function",
                identity.stable_id
            )));
        }
        if !enabled
            && identity.protocol == ControllerProtocol::Xusb
            && identity.coverage == ControlCoverage::Full
            && related_unsafe_xusb_descendant(&current, all_nodes, all_hid, false)
        {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: a keyboard, mouse, mixed, or unknown HID companion prevents safe XUSB disable",
                identity.stable_id
            )));
        }

        let mut params = SpPropchangeParams {
            class_install_header: SpClassInstallHeader {
                cb_size: std::mem::size_of::<SpClassInstallHeader>() as u32,
                install_function: DIF_PROPERTYCHANGE,
            },
            state_change: if enabled { DICS_ENABLE } else { DICS_DISABLE },
            scope: DICS_FLAG_GLOBAL,
            hw_profile: 0,
        };
        let params_size = std::mem::size_of::<SpPropchangeParams>() as u32;
        let ok = unsafe {
            SetupDiSetClassInstallParamsW(
                set.handle,
                &mut data,
                &mut params.class_install_header,
                params_size,
            )
        };
        if ok == 0 {
            return Err(property_change_error(
                "SetupDiSetClassInstallParamsW(DIF_PROPERTYCHANGE)",
            ));
        }
        let ok = unsafe { SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.handle, &mut data) };
        if ok == 0 {
            return Err(property_change_error(
                "SetupDiCallClassInstaller(DIF_PROPERTYCHANGE)",
            ));
        }
        return Ok(());
    }
    Err(PlatformError::NotFound(format!(
        "{}: exact function devnode disappeared before property change",
        identity.stable_id
    )))
}

fn node_needs_restart(identity: &VerifiedDevice) -> bool {
    scan_inventory()
        .ok()
        .and_then(|inventory| {
            inventory
                .nodes
                .into_iter()
                .find(|node| node.instance == identity.instance_id)
        })
        .map(|node| node.status & DN_NEEDS_RESTART != 0)
        .unwrap_or(false)
}

fn confirm_state(identity: &VerifiedDevice, expected_enabled: bool) -> bool {
    for _ in 0..8 {
        if let Ok(inventory) = scan_inventory() {
            if let Ok(record) = locate_identity(&inventory, identity) {
                if record.node.status_known
                    && (expected_enabled || record.node.administratively_disabled())
                    && record.device.state.enabled == expected_enabled
                {
                    return true;
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[derive(Clone)]
struct DisabledSnapshot {
    device: PlatformDevice,
}

pub(crate) struct WindowsPlatform {
    disabled: Mutex<BTreeMap<String, DisabledSnapshot>>,
}

fn overlay_device(devices: &mut Vec<PlatformDevice>, replacement: PlatformDevice) {
    let replacement_id = replacement.id.clone();
    if let Some(existing) = devices
        .iter_mut()
        .find(|device| device.id == replacement_id)
    {
        *existing = replacement;
    } else {
        devices.push(replacement);
    }
}
impl WindowsPlatform {
    pub(crate) fn new() -> Self {
        Self {
            disabled: Mutex::new(BTreeMap::new()),
        }
    }

    fn enumerate_internal(&self) -> Result<EnumerationReport, PlatformError> {
        let mut inventory = scan_inventory()?;
        let mut devices = inventory
            .records
            .iter()
            .map(|record| record.device.clone())
            .collect::<Vec<_>>();
        let mut errors = std::mem::take(&mut inventory.errors);
        let disabled = self.disabled.lock().map_err(|_| {
            PlatformError::OperationFailed("Windows controller state mutex poisoned".to_owned())
        })?;
        for (id, snapshot) in disabled.iter() {
            let expected = snapshot.device.verified()?;
            let node = inventory
                .nodes
                .iter()
                .find(|node| node.instance == expected.instance_id);
            if node
                .map(|node| node.administratively_disabled())
                .unwrap_or(false)
            {
                match locate_identity(&inventory, expected) {
                    Ok(record) => overlay_device(&mut devices, record.device),
                    Err(error) => errors.push(format!(
                        "{id}: persisted disabled identity was not overlaid: {error}"
                    )),
                }
            } else if node.is_none() && !devices.iter().any(|device| device.id.as_str() == id) {
                devices.push(snapshot.device.clone());
            }
        }
        for device in &devices {
            if let Some(limitation) = control_limitation(device.coverage) {
                errors.push(format!(
                    "{} ({}): {limitation}",
                    device.name,
                    device.id.as_str()
                ));
            }
        }
        devices.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        Ok(EnumerationReport { devices, errors })
    }
}

impl ControllerPlatform for WindowsPlatform {
    fn enumerate(&self) -> Result<EnumerationReport, PlatformError> {
        self.enumerate_internal()
    }

    fn resolve_verified(&self, id: &DeviceId) -> Result<VerifiedDevice, PlatformError> {
        if id.as_str().len() > 1024 || !id.as_str().starts_with(STABLE_ID_PREFIX) {
            return Err(PlatformError::VerificationFailed(
                "malformed Windows controller ID".to_owned(),
            ));
        }
        let inventory = scan_inventory()?;
        let disabled = self.disabled.lock().map_err(|_| {
            PlatformError::OperationFailed("Windows controller state mutex poisoned".to_owned())
        })?;
        if let Some(snapshot) = disabled.get(id.as_str()) {
            let expected = snapshot.device.verified()?;
            let node_is_disabled = inventory
                .nodes
                .iter()
                .find(|node| node.instance == expected.instance_id)
                .map(|node| node.administratively_disabled())
                .unwrap_or(false);
            if node_is_disabled {
                let record = locate_identity(&inventory, expected)?;
                return Ok(record.identity);
            }
        }
        if let Some(record) = inventory
            .records
            .iter()
            .find(|record| record.device.id == *id)
        {
            return Ok(record.identity.clone());
        }
        if let Some(snapshot) = disabled.get(id.as_str()) {
            let record = locate_identity(&inventory, snapshot.device.verified()?)?;
            return Ok(record.identity);
        }
        Err(PlatformError::NotFound(id.as_str().to_owned()))
    }

    fn resolve_verified_for_recovery(
        &self,
        id: &DeviceId,
        expected: &VerifiedDevice,
    ) -> Result<VerifiedDevice, PlatformError> {
        if id.as_str() != expected.stable_id {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: recovery command ID does not match persisted identity",
                id.as_str()
            )));
        }
        if !identity_shape_is_valid(expected) {
            return Err(PlatformError::VerificationFailed(
                "malformed or cross-platform Windows recovery identity".to_owned(),
            ));
        }
        let inventory = scan_inventory()?;
        let fresh = locate_identity(&inventory, expected)?;
        if fresh.device.id.as_str() != id.as_str() || !fresh.identity.same_hardware(expected) {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: fresh recovery identity does not match persisted Windows hardware",
                id.as_str()
            )));
        }
        Ok(fresh.identity)
    }

    fn set_enabled(
        &self,
        identity: &VerifiedDevice,
        enabled: bool,
    ) -> Result<MutationReport, PlatformError> {
        if !identity_shape_is_valid(identity) {
            return Err(PlatformError::VerificationFailed(
                "malformed or cross-platform Windows controller identity".to_owned(),
            ));
        }
        if !enabled && identity.coverage != ControlCoverage::Full {
            return Err(PlatformError::Unsupported(
                "refusing destructive disable: HID-only or ambiguous coverage leaves the XInput/XUSB path live",
            ));
        }

        // The first scan resolves the persisted identity.  A second, fresh
        // scan immediately before mutation closes the stale-devnode window.
        let first_inventory = scan_inventory()?;
        let first = locate_identity(&first_inventory, identity)?;
        let second_inventory = scan_inventory()?;
        let current = locate_identity(&second_inventory, identity)?;
        if !first.identity.same_hardware(&current.identity)
            || first.node.instance != current.node.instance
        {
            return Err(PlatformError::IdentityMismatch(format!(
                "{}: function identity changed during revalidation",
                identity.stable_id
            )));
        }
        if !current.node.status_known {
            return Err(PlatformError::VerificationFailed(format!(
                "{}: Configuration Manager could not provide an authoritative devnode status",
                identity.stable_id
            )));
        }
        match mutation_plan(&current.node, current.device.state.enabled, enabled) {
            Err(()) => {
                return Err(PlatformError::VerificationFailed(format!(
                    "{}: stopped state is not confirmed by CM_PROB_DISABLED; refusing to record a disable",
                    identity.stable_id
                )));
            }
            Ok(MutationPlan::Noop) => {
                return Ok(MutationReport::confirmed(
                    DeviceId(identity.stable_id.clone()),
                    enabled,
                    false,
                ));
            }
            Ok(MutationPlan::NativeChange) => {}
        }
        if !enabled
            && (!current.device.capabilities.full_control
                || current.identity.coverage != ControlCoverage::Full)
        {
            return Err(PlatformError::Unsupported(
                "refusing destructive disable: the exact controller function is not fully controllable",
            ));
        }

        apply_property_change(
            identity,
            current.node.devinst,
            &second_inventory.nodes,
            &second_inventory.hid,
            enabled,
        )?;
        if !confirm_state(identity, enabled) {
            if node_needs_restart(identity) {
                return Err(PlatformError::OperationFailed(format!(
                    "{}: Windows deferred the property change; a restart is required before the requested state is active",
                    identity.stable_id
                )));
            }
            return Err(PlatformError::VerificationFailed(format!(
                "{}: Windows reported the property change but fresh status confirmation failed",
                identity.stable_id
            )));
        }

        let mut report =
            MutationReport::confirmed(DeviceId(identity.stable_id.clone()), enabled, true);
        if !enabled {
            let mut disabled_device = current.device.clone();
            disabled_device.state.enabled = false;
            if let Ok(mut disabled) = self.disabled.lock() {
                disabled.insert(
                    identity.stable_id.clone(),
                    DisabledSnapshot {
                        device: disabled_device,
                    },
                );
            }
        } else if let Ok(mut disabled) = self.disabled.lock() {
            disabled.remove(&identity.stable_id);
        }
        if let Some(limitation) = control_limitation(identity.coverage) {
            if let Some(outcome) = report.outcomes.first_mut() {
                outcome.error = Some(limitation.to_owned());
            }
        }
        Ok(report)
    }

    fn open_bluetooth_settings(&self) -> Result<(), PlatformError> {
        let operation = wide("open");
        let target = wide("ms-settings:bluetooth");
        // The URI and the null parameter are constants.  No command or path
        // supplied by the UI is ever passed to ShellExecuteW.
        let result = unsafe {
            ShellExecuteW(
                null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if result <= 32 {
            let code = unsafe { GetLastError() };
            return Err(PlatformError::LaunchFailed(format!(
                "ShellExecuteW(ms-settings:bluetooth) returned {result}; Win32 error {code} (0x{code:08X}). Open Windows Settings > Bluetooth manually or repair the Settings app."
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_node(
        usage_page: u16,
        usage: u16,
        parent: u16,
        number_of_children: u16,
        next_sibling: u16,
        first_child: u16,
        collection_type: u8,
    ) -> HidpLinkCollectionNode {
        HidpLinkCollectionNode {
            link_usage: usage,
            link_usage_page: usage_page,
            parent,
            number_of_children,
            next_sibling,
            first_child,
            collection_type,
            is_alias: 0,
            reserved: 0,
            user_context: std::ptr::null_mut(),
        }
    }

    fn xusb_identity(node: &RawDevnode, coverage: ControlCoverage) -> VerifiedDevice {
        VerifiedDevice {
            platform: PLATFORM.to_owned(),
            stable_id: stable_id(&node.instance),
            instance_id: node.instance.clone(),
            fingerprint: fingerprint(node, 0, 0),
            protocol: ControllerProtocol::Xusb,
            coverage,
            usage_page: 0,
            usage: 0,
        }
    }
    fn raw(
        devinst: u32,
        instance: &str,
        class_name: &str,
        parent: Option<u32>,
        status: u32,
        problem: Option<i32>,
    ) -> RawDevnode {
        RawDevnode {
            devinst,
            instance: instance.to_owned(),
            class_name: class_name.to_owned(),
            service: String::new(),
            enumerator: "USB".to_owned(),
            hardware_ids: vec!["USB\\VID_1234&PID_5678".to_owned()],
            friendly_name: "Fixture controller".to_owned(),
            description: "Fixture controller".to_owned(),
            manufacturer: "Fixture".to_owned(),
            parent,
            xusb_interface: false,
            status,
            problem,
            status_known: true,
        }
    }

    fn hid_identity(node: &RawDevnode, coverage: ControlCoverage) -> VerifiedDevice {
        VerifiedDevice {
            platform: PLATFORM.to_owned(),
            stable_id: stable_id(&node.instance),
            instance_id: node.instance.clone(),
            fingerprint: fingerprint(node, GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD),
            protocol: ControllerProtocol::HidGamepad,
            coverage,
            usage_page: GENERIC_DESKTOP_PAGE,
            usage: USAGE_GAME_PAD,
        }
    }
    #[test]
    fn nested_physical_pointer_is_not_a_second_top_level_collection() {
        let mut root = link_node(
            GENERIC_DESKTOP_PAGE,
            USAGE_GAME_PAD,
            0,
            1,
            0,
            1,
            COLLECTION_APPLICATION,
        );
        let child = link_node(GENERIC_DESKTOP_PAGE, 0x01, 0, 0, 0, 0, COLLECTION_PHYSICAL);
        root.number_of_children = 1;
        let mixed = classify_hid_collections(&[root, child], GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD)
            .expect("well-formed nested physical collection");
        assert!(!mixed);
    }

    #[test]
    fn nested_application_pointer_is_mixed() {
        let root = link_node(
            GENERIC_DESKTOP_PAGE,
            USAGE_GAME_PAD,
            0,
            1,
            0,
            1,
            COLLECTION_APPLICATION,
        );
        let child = link_node(
            GENERIC_DESKTOP_PAGE,
            0x01,
            0,
            0,
            0,
            0,
            COLLECTION_APPLICATION,
        );
        assert!(
            classify_hid_collections(&[root, child], GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD,)
                .expect("well-formed nested application collection")
        );
    }

    #[test]
    fn ig00_hid_companion_is_never_full() {
        let node = raw(
            10,
            "USB\\VID_1234&PID_5678&IG_00",
            "HIDClass",
            None,
            DN_STARTED,
            None,
        );
        let mut aggregate = HidAggregate::new();
        aggregate.add(GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD, false);
        let record = build_record(&node, &aggregate, &[node.clone()], &BTreeMap::new())
            .expect("allowed HID fixture should be represented");
        assert_eq!(record.identity.coverage, ControlCoverage::Uncontrolled);
        assert!(!record.device.capabilities.full_control);
    }

    #[test]
    fn failed_hid_inspection_blocks_full_record() {
        let node = raw(
            11,
            "USB\\VID_1234&PID_5678&MI_00",
            "HIDClass",
            None,
            DN_STARTED,
            None,
        );
        let mut aggregate = HidAggregate::new();
        aggregate.add(GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD, false);
        aggregate.mark_inspection_failed();
        assert!(build_record(&node, &aggregate, std::slice::from_ref(&node), &BTreeMap::new())
            .is_none());
    }

    #[test]
    fn only_exact_xusb_interface_evidence_is_native() {
        let mut native = raw(
            20,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_STARTED,
            None,
        );
        native.xusb_interface = true;
        let companion = raw(
            21,
            "USB\\VID_045E&PID_0B13&IG_00",
            "HIDClass",
            Some(native.devinst),
            DN_STARTED,
            None,
        );
        let sibling = raw(
            22,
            "USB\\VID_045E&PID_0B13&MI_01",
            "HIDClass",
            Some(1),
            DN_STARTED,
            None,
        );
        let parent = raw(1, "USB\\VID_045E&PID_0B13", "USB", None, DN_STARTED, None);
        assert!(xusb_direct(&native));
        assert!(!xusb_direct(&companion));
        assert!(!xusb_direct(&parent));
        let related_nodes = [parent.clone(), native.clone(), companion.clone()];
        let related = xusb_related(&related_nodes[2], &related_nodes);
        assert_eq!(
            related
                .iter()
                .filter(|node| node.devinst == native.devinst)
                .count(),
            1
        );
        let unrelated_nodes = [parent, native, sibling];
        assert!(xusb_related(&unrelated_nodes[2], &unrelated_nodes).is_empty());
    }

    #[test]
    fn native_xusb_row_is_full_with_verified_hid_descendant() {
        let mut native = raw(
            30,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_STARTED,
            None,
        );
        native.xusb_interface = true;
        let transport = raw(
            31,
            "USB\\VID_045E&PID_0B13&HID_TRANSPORT",
            "HIDClass",
            Some(native.devinst),
            DN_STARTED,
            None,
        );
        let child = raw(
            32,
            "USB\\VID_045E&PID_0B13&IG_00",
            "HIDClass",
            Some(transport.devinst),
            DN_STARTED,
            None,
        );
        let mut hid = HidAggregate::new();
        hid.add(GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD, false);
        let mut all_hid = BTreeMap::new();
        all_hid.insert(child.devinst, hid);
        let record = build_xusb_record(&native, &[native.clone(), transport, child], &all_hid)
            .expect("native XUSB function remains visible");
        assert_eq!(record.identity.coverage, ControlCoverage::Full);
    }

    #[test]
    fn unknown_xusb_descendant_is_not_full_control() {
        let mut native = raw(
            32,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_STARTED,
            None,
        );
        native.xusb_interface = true;
        let unknown = raw(
            33,
            "USB\\VID_045E&PID_0B13&UNKNOWN",
            "VendorFunction",
            Some(native.devinst),
            DN_STARTED,
            None,
        );
        let record = build_xusb_record(&native, &[native.clone(), unknown], &BTreeMap::new())
            .expect("native XUSB function remains visible");
        assert_eq!(record.identity.coverage, ControlCoverage::Uncontrolled);
    }
    #[test]
    fn missing_active_hid_descendant_is_not_full_control() {
        let mut native = raw(
            34,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_STARTED,
            None,
        );
        native.xusb_interface = true;
        let child = raw(
            35,
            "USB\\VID_045E&PID_0B13&IG_00",
            "HIDClass",
            Some(native.devinst),
            DN_STARTED,
            None,
        );
        let record = build_xusb_record(&native, &[native.clone(), child], &BTreeMap::new())
            .expect("native XUSB function remains visible");
        assert_eq!(record.identity.coverage, ControlCoverage::Uncontrolled);
    }

    #[test]
    fn stopped_without_cm_disabled_is_not_administratively_disabled() {
        let stopped = raw(
            40,
            "USB\\VID_1234&PID_5678&MI_00",
            "HIDClass",
            None,
            0,
            None,
        );
        let disabled = raw(
            41,
            "USB\\VID_1234&PID_5678&MI_01",
            "HIDClass",
            None,
            DN_HAS_PROBLEM,
            Some(CM_PROB_DISABLED as i32),
        );
        assert!(!stopped.administratively_disabled());
        assert!(disabled.administratively_disabled());
        assert_eq!(mutation_plan(&stopped, false, false), Err(()));
        assert_eq!(
            mutation_plan(&stopped, true, false),
            Ok(MutationPlan::NativeChange)
        );
        assert_eq!(
            mutation_plan(&disabled, false, false),
            Ok(MutationPlan::Noop)
        );
    }

    #[test]
    fn unknown_status_controller_is_not_actionable() {
        let mut unknown_hid = raw(
            36,
            "USB\\VID_1234&PID_5678&MI_00",
            "HIDClass",
            None,
            0,
            None,
        );
        unknown_hid.status_known = false;
        let mut hid = HidAggregate::new();
        hid.add(GENERIC_DESKTOP_PAGE, USAGE_GAME_PAD, false);
        assert!(
            build_record(&unknown_hid, &hid, std::slice::from_ref(&unknown_hid), &BTreeMap::new())
                .is_none()
        );

        let mut unknown_xusb = raw(37, "USB\\VID_045E&PID_0B13&MI_00", "XUSB", None, 0, None);
        unknown_xusb.status_known = false;
        unknown_xusb.xusb_interface = true;
        assert!(
            build_xusb_record(
                &unknown_xusb,
                std::slice::from_ref(&unknown_xusb),
                &BTreeMap::new()
            )
            .is_none()
        );
    }

    #[test]
    fn cold_full_hid_recovery_ignores_sibling_and_parent_transport() {
        let transport = raw(
            49,
            "USB\\VID_1234&PID_5678&HID_TRANSPORT",
            "HIDClass",
            Some(1),
            DN_STARTED,
            None,
        );
        let node = raw(
            50,
            "USB\\VID_1234&PID_5678&MI_00",
            "HIDClass",
            Some(transport.devinst),
            DN_HAS_PROBLEM,
            Some(CM_PROB_DISABLED as i32),
        );
        let sibling = raw(
            51,
            "USB\\VID_1234&PID_5678&MI_01",
            "HIDClass",
            Some(1),
            DN_HAS_PROBLEM,
            Some(CM_PROB_DISABLED as i32),
        );
        let identity = hid_identity(&node, ControlCoverage::Full);
        let inventory = Inventory {
            nodes: vec![node.clone(), transport, sibling],
            hid: BTreeMap::new(),
            records: Vec::new(),
            errors: Vec::new(),
        };
        let record = reconstructed_record(&node, &identity, &inventory)
            .expect("the exact disabled HID TLC is recoverable");
        assert!(record.identity.same_hardware(&identity));
        assert!(!record.device.state.enabled);
    }

    #[test]
    fn cold_full_xusb_recovery_restores_full_coverage_without_hid_handle() {
        let mut native = raw(
            60,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_HAS_PROBLEM,
            Some(CM_PROB_DISABLED as i32),
        );
        native.xusb_interface = true;
        let child = raw(
            61,
            "USB\\VID_045E&PID_0B13&IG_00",
            "HIDClass",
            Some(native.devinst),
            DN_STARTED,
            None,
        );
        let identity = xusb_identity(&native, ControlCoverage::Full);
        let stale_projection =
            build_xusb_record(&native, &[native.clone(), child.clone()], &BTreeMap::new())
                .expect("disabled native XUSB row remains addressable");
        assert_eq!(stale_projection.identity.coverage, ControlCoverage::Full);
        let inventory = Inventory {
            nodes: vec![native.clone(), child],
            hid: BTreeMap::new(),
            records: vec![stale_projection],
            errors: Vec::new(),
        };
        let record = locate_identity(&inventory, &identity)
            .expect("disabled XUSB identity is reconstructed before coverage comparison");
        assert_eq!(record.identity.coverage, ControlCoverage::Full);
        assert!(!record.device.state.enabled);
    }

    #[test]
    fn cold_full_xusb_recovery_rejects_explicit_keyboard_without_hid_aggregate() {
        let mut native = raw(
            62,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_HAS_PROBLEM,
            Some(CM_PROB_DISABLED as i32),
        );
        native.xusb_interface = true;
        let keyboard = raw(
            63,
            "USB\\VID_045E&PID_0B13&KEYBOARD",
            "Keyboard",
            Some(native.devinst),
            DN_STARTED,
            None,
        );
        let identity = xusb_identity(&native, ControlCoverage::Full);
        let inventory = Inventory {
            nodes: vec![native.clone(), keyboard],
            hid: BTreeMap::new(),
            records: Vec::new(),
            errors: Vec::new(),
        };
        let result = reconstructed_record(&native, &identity, &inventory);
        assert!(matches!(result, Err(PlatformError::IdentityMismatch(_))));
    }
    #[test]
    fn disabled_snapshot_replaces_same_id_dynamic_projection() {
        let mut native = raw(
            70,
            "USB\\VID_045E&PID_0B13&MI_00",
            "XUSB",
            Some(1),
            DN_HAS_PROBLEM,
            Some(CM_PROB_DISABLED as i32),
        );
        native.xusb_interface = true;
        let nodes = vec![native.clone()];
        let stale = platform_device(
            &native,
            xusb_identity(&native, ControlCoverage::Uncontrolled),
            false,
            &nodes,
        )
        .expect("known disabled node has an authoritative state");
        let replacement = platform_device(
            &native,
            xusb_identity(&native, ControlCoverage::Full),
            false,
            &nodes,
        )
        .expect("known disabled node has an authoritative state");
        let mut devices = vec![stale];
        overlay_device(&mut devices, replacement);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].coverage, ControlCoverage::Full);
    }
}
