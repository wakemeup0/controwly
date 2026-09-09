//! Root-owned, polkit-launched Linux controller function mechanism.
//!
//! The GUI never runs this binary directly as root.  It invokes the fixed
//! `/usr/bin/pkexec` path, which authorizes this fixed installed executable via
//! the packaged polkit action.  This process accepts only a verified opaque
//! identity, derives the sysfs function from the identity's validated kernel
//! name, and refuses keyboard/mouse/mixed functions.

#[path = "../../linux/input.rs"]
mod input;

use input::Identity;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::thread;
use std::time::Duration;

const MAX_ARGUMENT: usize = 4096;

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if unsafe { geteuid() } != 0 {
        fail(4, "the Linux input helper must run as root through polkit");
    }
    if args.len() == 1
        && matches!(
            args[0].as_str(),
            "--prepare-removal" | "--clear-removal-gate" | "--restore-journal"
        )
        && env::var_os("PKEXEC_UID").is_some()
    {
        fail(
            4,
            "package maintenance helper modes must run from the package manager as direct root",
        );
    }
    if args.len() == 1 && args[0] == "--prepare-removal" {
        let journal_lock =
            input::JournalLock::acquire().unwrap_or_else(|message| fail(5, &message));
        if let Err(message) = prepare_removal(&journal_lock) {
            fail(5, &message);
        }
        restore_journal(&journal_lock);
    }
    if args.len() == 1 && args[0] == "--clear-removal-gate" {
        let journal_lock =
            input::JournalLock::acquire().unwrap_or_else(|message| fail(5, &message));
        if let Err(message) = finish_install(&journal_lock) {
            fail(5, &message);
        }
        ok();
    }
    if args.len() == 1 && args[0] == "--restore-journal" {
        let journal_lock =
            input::JournalLock::acquire().unwrap_or_else(|message| fail(5, &message));
        restore_journal(&journal_lock);
    }
    let request = match Request::parse(args) {
        Ok(request) => request,
        Err(message) => fail(2, &message),
    };
    let action = request.action;
    let identity = Identity {
        platform: request.platform,
        stable_id: request.stable_id,
        instance_id: request.instance_id,
        fingerprint: request.fingerprint,
    };
    // Enable is deliberately keyed only by the caller's bounded stable ID.
    // The remaining identity fields are untrusted GUI data and are resolved
    // from the root-owned journal below.
    if action == "enable" {
        if identity.platform != "linux" || !input::valid_stable_id(&identity.stable_id) {
            fail(2, "malformed Linux controller stable ID");
        }
    } else if let Err(message) = input::validate_identity(&identity) {
        fail(2, &message);
    }
    let journal_lock = input::JournalLock::acquire().unwrap_or_else(|message| fail(5, &message));

    match action.as_str() {
        "disable" => disable(&identity, &journal_lock),
        "enable" => enable(&identity, &journal_lock),
        _ => fail(2, "unsupported helper action"),
    }
}

const O_DIRECTORY: i32 = 0o200000;
const O_NOFOLLOW: i32 = 0o400000;

fn prepare_removal(journal_lock: &input::JournalLock) -> Result<(), String> {
    ensure_removal_gate(journal_lock)
}

fn finish_install(journal_lock: &input::JournalLock) -> Result<(), String> {
    clear_removal_gate(journal_lock)
}

fn gate_file(file: &std::fs::File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect Controwly removal gate: {error}"))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o777 != 0o600
    {
        return Err("Controwly removal gate is not a root-owned mode-0600 regular file".to_owned());
    }
    Ok(())
}

fn gate_directory() -> Result<(), String> {
    let parent = Path::new(input::REMOVAL_GATE_PATH)
        .parent()
        .ok_or_else(|| "Controwly removal gate has no trusted parent".to_owned())?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open(parent)
        .map_err(|error| format!("cannot open Controwly recovery directory: {error}"))?;
    let metadata = directory
        .metadata()
        .map_err(|error| format!("cannot inspect Controwly recovery directory: {error}"))?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("Controwly recovery directory is not root-owned and non-writable".to_owned());
    }
    directory
        .sync_all()
        .map_err(|error| format!("cannot sync Controwly recovery directory: {error}"))
}

fn ensure_removal_gate(_journal_lock: &input::JournalLock) -> Result<(), String> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(input::REMOVAL_GATE_PATH)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NOFOLLOW)
            .open(input::REMOVAL_GATE_PATH)
            .map_err(|error| format!("cannot open Controwly removal gate: {error}"))?,
        Err(error) => return Err(format!("cannot create Controwly removal gate: {error}")),
    };
    gate_file(&file)?;
    file.sync_all()
        .map_err(|error| format!("cannot sync Controwly removal gate: {error}"))?;
    drop(file);
    gate_directory()
}

fn removal_gate_active() -> Result<bool, String> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(input::REMOVAL_GATE_PATH)
    {
        Ok(file) => {
            gate_file(&file)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot inspect Controwly removal gate: {error}")),
    }
}

fn clear_removal_gate(journal_lock: &input::JournalLock) -> Result<(), String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(input::REMOVAL_GATE_PATH)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot open Controwly removal gate: {error}")),
    };
    gate_file(&file)?;
    drop(file);
    fs::remove_file(input::REMOVAL_GATE_PATH)
        .map_err(|error| format!("cannot clear Controwly removal gate: {error}"))?;
    if let Err(error) = gate_directory() {
        // Re-establish the gate before reporting failure.  This is a
        // post-install lifecycle error, not permission to resume disables.
        let restore = ensure_removal_gate(journal_lock);
        return Err(format!(
            "Controwly removal gate clear is uncertain; disables remain blocked: {error}; gate restore: {restore:?}"
        ));
    }
    Ok(())
}

fn restore_journal(journal_lock: &input::JournalLock) -> ! {
    let records = match input::journal_read_locked(journal_lock) {
        Ok(records) => records,
        Err(message) => fail(5, &message),
    };
    let mut failures = Vec::new();
    for identity in records {
        if let Err(message) = recover_record(&identity, journal_lock, true) {
            failures.push(format!("{}: {message}", identity.stable_id));
            // Do not issue another read/modify/write after an uncertain
            // journal outcome.  A retry can reconcile the authoritative file
            // without risking loss of an unrelated record.
            break;
        }
    }
    if failures.is_empty() {
        ok();
    }
    fail(5, &failures.join("; "));
}

fn recover_record(
    identity: &Identity,
    journal_lock: &input::JournalLock,
    clear_gone: bool,
) -> Result<(), String> {
    let current = current_controller(identity)?;
    let controller = match current {
        CurrentController::Exact(controller) => controller,
        CurrentController::StableMismatch => {
            // A reconnect may reuse the stable hardware key while exposing a
            // different function.  Only remove the old record when its own
            // validated sysfs function is definitively gone; never bind the
            // replacement.
            if clear_gone && input::function_is_definitively_gone(identity) {
                return input::journal_remove_locked(journal_lock, identity)
                    .map_err(|message| format!("gone recovery record removal outcome is uncertain; retry restoration: {message}"));
            }
            return Err("fresh controller uses the stable key but does not match the authoritative recovery record".to_owned());
        }
        CurrentController::Absent => {
            if clear_gone && input::function_is_definitively_gone(identity) {
                return input::journal_remove_locked(journal_lock, identity)
                    .map_err(|message| format!("gone recovery record removal outcome is uncertain; retry restoration: {message}"));
            }
            input::controller_from_unbound(identity)
                .map_err(|message| format!("{message}; recovery journal retained"))?
        }
    };

    let function = controller.function.as_ref().ok_or_else(|| {
        "validated recovery controller has no function; recovery journal retained".to_owned()
    })?;
    if function.driver.is_some() {
        verify_bound_controller(&controller, identity)?;
        return input::journal_remove_locked(journal_lock, identity)
            .map_err(|message| format!("controller is enabled and safe, but recovery record removal outcome is uncertain; retry: {message}"));
    }

    verify_unbound_controller(&controller, identity)?;
    input::bind(function)
        .map_err(|error| format!("{}; recovery journal retained", error.message))?;
    wait_for_bound(identity)?;

    // Re-read and verify the fresh kernel state after bind.  Never roll back
    // with the pre-bind FunctionIdentity: a failed confirmation is an
    // uncertain kernel outcome and the journal must remain authoritative.
    let rebound = match current_controller(identity)? {
        CurrentController::Exact(controller) => controller,
        CurrentController::StableMismatch => {
            return Err("rebound controller uses the stable key but does not match the authoritative recovery record; recovery journal retained".to_owned());
        }
        CurrentController::Absent => {
            return Err(
                "kernel did not expose the rebound controller; recovery journal retained"
                    .to_owned(),
            );
        }
    };
    verify_bound_controller(&rebound, identity)?;
    input::journal_remove_locked(journal_lock, identity)
        .map_err(|message| format!("controller is enabled and safe, but recovery record removal outcome is uncertain; retry: {message}"))
}

enum CurrentController {
    Exact(input::Controller),
    StableMismatch,
    Absent,
}

fn current_controller(identity: &Identity) -> Result<CurrentController, String> {
    let discovery = input::discover();
    let mut stable_mismatch = false;
    for controller in discovery.controllers {
        if controller.identity.stable_id != identity.stable_id {
            continue;
        }
        if controller.identity == *identity {
            return Ok(CurrentController::Exact(controller));
        }
        stable_mismatch = true;
    }
    Ok(if stable_mismatch {
        CurrentController::StableMismatch
    } else {
        CurrentController::Absent
    })
}

fn verify_bound_controller(
    controller: &input::Controller,
    identity: &Identity,
) -> Result<(), String> {
    if controller.identity != *identity {
        return Err(
            "fresh controller identity does not match the authoritative recovery record".to_owned(),
        );
    }
    if !controller.can_full_control || controller.mixed_unrelated {
        return Err("bound controller function is no longer safely isolated".to_owned());
    }
    let function = controller
        .function
        .as_ref()
        .ok_or_else(|| "bound controller has no validated function".to_owned())?;
    verify_function(function, identity, true)
}

fn verify_unbound_controller(
    controller: &input::Controller,
    identity: &Identity,
) -> Result<(), String> {
    if controller.identity != *identity {
        return Err(
            "fresh unbound controller identity does not match the authoritative recovery record"
                .to_owned(),
        );
    }
    if controller.mixed_unrelated {
        return Err("unbound controller has an unrelated keyboard or mouse sibling".to_owned());
    }
    let function = controller
        .function
        .as_ref()
        .ok_or_else(|| "unbound controller has no validated function".to_owned())?;
    verify_function(function, identity, false)
}

fn verify_function(
    function: &input::FunctionIdentity,
    identity: &Identity,
    require_bound: bool,
) -> Result<(), String> {
    let fields = input::parse_fingerprint(&identity.fingerprint)
        .ok_or_else(|| "authoritative recovery fingerprint is malformed".to_owned())?;
    let expected_bus = match fields.get("bus").map(String::as_str) {
        Some("hid") => input::FunctionBus::Hid,
        Some("usb") => input::FunctionBus::Usb,
        _ => {
            return Err(
                "authoritative recovery fingerprint has an unsupported function bus".to_owned(),
            )
        }
    };
    let expected_function = fields
        .get("function")
        .ok_or_else(|| "authoritative recovery fingerprint has no function identity".to_owned())?;
    if function.bus != expected_bus || function.id != *expected_function {
        return Err(
            "fresh kernel function does not match the authoritative recovery record".to_owned(),
        );
    }
    let expected_driver = fields
        .get("driver")
        .filter(|driver| !driver.is_empty())
        .ok_or_else(|| "authoritative recovery fingerprint has no approved driver".to_owned())?;
    if !input::safe_driver(expected_driver) {
        return Err("authoritative recovery record names an unapproved driver".to_owned());
    }
    if require_bound {
        if function.driver.as_deref() != Some(expected_driver.as_str()) {
            return Err(
                "fresh controller driver does not match the authoritative recovery record"
                    .to_owned(),
            );
        }
    } else {
        if function.driver.is_some() {
            return Err("refusing to bind a function that is already bound".to_owned());
        }
        if function.expected_driver.as_deref() != Some(expected_driver.as_str()) {
            return Err(
                "unbound function driver does not match the authoritative recovery record"
                    .to_owned(),
            );
        }
    }
    if require_bound && function.bus == input::FunctionBus::Hid {
        input::verify_hid_descriptor_identity(&function.id, &fields)?;
    }
    if require_bound && function.bus == input::FunctionBus::Usb {
        verify_usb_authorized(function)?;
    }
    Ok(())
}

fn verify_usb_authorized(function: &input::FunctionIdentity) -> Result<(), String> {
    let path = Path::new("/sys/bus/usb/devices").join(&function.id);
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("cannot verify USB interface authorization: {error}"))?;
    let sysfs_root = fs::canonicalize("/sys/devices")
        .map_err(|error| format!("cannot verify USB sysfs root: {error}"))?;
    if !canonical.starts_with(&sysfs_root)
        || canonical.file_name().and_then(|name| name.to_str()) != Some(function.id.as_str())
    {
        return Err("USB interface path escaped the fixed sysfs root".to_owned());
    }
    let authorized = fs::read_to_string(canonical.join("authorized"))
        .map_err(|error| format!("cannot read USB interface authorization: {error}"))?;
    if authorized.trim() != "1" {
        return Err("USB interface is not freshly authorized".to_owned());
    }
    Ok(())
}

fn wait_for_bound(identity: &Identity) -> Result<(), String> {
    for _ in 0..20 {
        match current_controller(identity)? {
            CurrentController::Exact(controller) => {
                verify_bound_controller(&controller, identity)?;
                return Ok(());
            }
            CurrentController::StableMismatch => {
                return Err("fresh rebound controller does not match the authoritative recovery record; recovery journal retained".to_owned());
            }
            CurrentController::Absent => thread::sleep(Duration::from_millis(25)),
        }
    }
    Err("kernel did not confirm that the validated controller function was rebound; recovery journal retained".to_owned())
}

fn disable(identity: &Identity, journal_lock: &input::JournalLock) {
    if removal_gate_active().unwrap_or_else(|message| fail(5, &message)) {
        fail(
            5,
            "controller disable is blocked while package removal is in progress",
        );
    }
    let discovery = input::discover();
    let Some(controller) = discovery
        .controllers
        .iter()
        .find(|controller| controller.identity == *identity)
    else {
        fail(
            3,
            "fresh Linux input enumeration did not match the requested controller identity",
        );
    };
    if let Err(message) = verify_bound_controller(controller, identity) {
        fail(3, &message);
    }
    let journal_identity = controller.identity.clone();
    let Some(function) = controller.function.as_ref() else {
        fail(3, "controller has no validated HID or USB function");
    };

    // Commit and re-read the root-owned recovery intent while the same
    // cross-process flock is held.  No unbind or USB deauthorization occurs
    // until the durable journal record is confirmed.
    if let Err(message) = input::journal_add_locked(journal_lock, &journal_identity) {
        fail(
            5,
            &format!("cannot durably record controller recovery intent: {message}"),
        );
    }
    let recorded = match input::journal_read_locked(journal_lock) {
        Ok(records) => records.iter().any(|record| record == &journal_identity),
        Err(message) => fail(
            5,
            &format!("cannot confirm controller recovery intent: {message}"),
        ),
    };
    if !recorded {
        fail(
            5,
            "durable controller recovery intent was not confirmed; no kernel mutation performed",
        );
    }
    if let Err(error) = input::unbind(function) {
        fail(
            4,
            &format!(
                "controller unbind failed; recovery journal retained: {}",
                error.message
            ),
        );
    }
    for _ in 0..20 {
        if let Ok(current) = input::find_unbound_function(&journal_identity) {
            // find_unbound_function has already validated the authoritative
            // canonical function path and immutable properties.  A HID
            // report descriptor can legitimately disappear with its event
            // child after unbind, so descriptor verification belongs only to
            // the fresh bound-state checks before journal clearing.
            if current.driver.is_none() {
                ok();
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    fail(5, "kernel did not confirm that the validated controller function was unbound; recovery journal retained");
}

fn enable(requested: &Identity, journal_lock: &input::JournalLock) {
    let records = match input::journal_read_locked(journal_lock) {
        Ok(records) => records,
        Err(message) => fail(5, &message),
    };
    if let Some(identity) = records
        .into_iter()
        .find(|record| record.stable_id == requested.stable_id)
    {
        if let Err(message) = recover_record(&identity, journal_lock, false) {
            fail(5, &message);
        }
        ok();
    }

    // A post-confirmation journal clear may have become uncertain after an
    // atomic rename.  If the root record is already absent, the only safe
    // success path is a fresh exact, already-bound controller verification;
    // this path performs no bind and no journal mutation.
    if let Err(message) = input::validate_identity(requested) {
        fail(3, &message);
    }
    let discovery = input::discover();
    let Some(controller) = discovery
        .controllers
        .iter()
        .find(|controller| controller.identity == *requested)
    else {
        fail(
            3,
            "controller identity is not present in the root recovery journal",
        );
    };
    if let Err(message) = verify_bound_controller(controller, requested) {
        fail(3, &message);
    }
    ok();
}

fn ok() -> ! {
    println!("ok");
    std::process::exit(0);
}

fn fail(code: i32, message: &str) -> ! {
    let sanitized = message
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(512)
        .collect::<String>();
    let _ = writeln!(std::io::stderr(), "{sanitized}");
    std::process::exit(code);
}

struct Request {
    action: String,
    platform: String,
    stable_id: String,
    instance_id: String,
    fingerprint: String,
}

impl Request {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut action = None;
        let mut platform = None;
        let mut stable_id = None;
        let mut instance_id = None;
        let mut fingerprint = None;
        let mut index = 0;
        while index < args.len() {
            let flag = args[index].as_str();
            if matches!(flag, "--disable" | "--enable") {
                if action
                    .replace(flag.trim_start_matches("--").to_owned())
                    .is_some()
                {
                    return Err("duplicate helper action".to_owned());
                }
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("missing value for {flag}"))?;
            if value.len() > MAX_ARGUMENT
                || value
                    .bytes()
                    .any(|byte| byte == 0 || byte.is_ascii_control())
            {
                return Err("helper argument is malformed".to_owned());
            }
            match flag {
                "--platform" => set_once(&mut platform, value, flag)?,
                "--stable-id" => set_once(&mut stable_id, value, flag)?,
                "--instance-id" => set_once(&mut instance_id, value, flag)?,
                "--fingerprint" => set_once(&mut fingerprint, value, flag)?,
                _ => return Err(format!("unsupported helper argument {flag}")),
            }
            index += 2;
        }
        Ok(Self {
            action: action.ok_or_else(|| "missing helper action".to_owned())?,
            platform: platform.ok_or_else(|| "missing platform".to_owned())?,
            stable_id: stable_id.ok_or_else(|| "missing stable ID".to_owned())?,
            instance_id: instance_id.ok_or_else(|| "missing instance ID".to_owned())?,
            fingerprint: fingerprint.ok_or_else(|| "missing identity fingerprint".to_owned())?,
        })
    }
}
fn set_once(slot: &mut Option<String>, value: &str, flag: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate argument {flag}"));
    }
    slot.replace(value.to_owned());
    Ok(())
}

unsafe extern "C" {
    fn geteuid() -> u32;
}
