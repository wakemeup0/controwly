//! Bounded HID report-descriptor classification for Linux controller discovery.
//!
//! The parser deliberately understands the HID short-item state machine rather
//! than searching descriptor bytes for controller-looking usages.  It is kept
//! independent of sysfs and of the Linux input implementation so the caller
//! can bound the descriptor read before invoking it.

const MAX_DESCRIPTOR_BYTES: usize = 128 * 1024;
const MAX_COLLECTION_DEPTH: usize = 32;
const MAX_GLOBAL_STACK_DEPTH: usize = 32;
const MAX_LOCAL_USAGES: usize = 64;
const MAX_LOCAL_RANGES: usize = 16;
const MAX_LOCAL_ITEMS: usize = MAX_LOCAL_USAGES + MAX_LOCAL_RANGES;

const GENERIC_DESKTOP_PAGE: u16 = 0x01;
const KEYBOARD_PAGE: u16 = 0x07;
const USAGE_POINTER: u16 = 0x01;
const USAGE_MOUSE: u16 = 0x02;
const USAGE_JOYSTICK: u16 = 0x04;
const USAGE_GAMEPAD: u16 = 0x05;
const USAGE_KEYBOARD: u16 = 0x06;
const INPUT_CONSTANT: u32 = 0x01;
const INPUT_FLAGS_MASK: u32 = 0x017f;
const MAIN_FLAGS_MASK: u32 = 0x01ff;
const COLLECTION_APPLICATION: u8 = 0x01;

const CONTROLLER_ERROR: &str = "HID report descriptor is not a controller-only function";

#[derive(Clone, Copy)]
struct GlobalState {
    usage_page: u16,
}

impl GlobalState {
    const DEFAULT: Self = Self { usage_page: 0 };
}

#[derive(Clone, Copy)]
struct Usage {
    page: u16,
    id: u16,
    extended: bool,
}

#[derive(Clone, Copy)]
struct UsageRange {
    minimum: Usage,
    maximum: Usage,
}

#[derive(Clone, Copy)]
enum LocalUsageRef {
    Usage(usize),
    Range(usize),
}

struct LocalState {
    usages: [Usage; MAX_LOCAL_USAGES],
    usage_count: usize,
    ranges: [UsageRange; MAX_LOCAL_RANGES],
    range_count: usize,
    order: [LocalUsageRef; MAX_LOCAL_ITEMS],
    order_count: usize,
    pending_minimum: Option<Usage>,
}

impl LocalState {
    const EMPTY_USAGE: Usage = Usage {
        page: 0,
        id: 0,
        extended: false,
    };
    const EMPTY_RANGE: UsageRange = UsageRange {
        minimum: Self::EMPTY_USAGE,
        maximum: Self::EMPTY_USAGE,
    };

    const fn new() -> Self {
        Self {
            usages: [Self::EMPTY_USAGE; MAX_LOCAL_USAGES],
            usage_count: 0,
            ranges: [Self::EMPTY_RANGE; MAX_LOCAL_RANGES],
            range_count: 0,
            order: [LocalUsageRef::Usage(0); MAX_LOCAL_ITEMS],
            order_count: 0,
            pending_minimum: None,
        }
    }

    fn clear(&mut self) {
        self.usage_count = 0;
        self.range_count = 0;
        self.order_count = 0;
        self.pending_minimum = None;
    }

    fn is_empty(&self) -> bool {
        self.usage_count == 0 && self.range_count == 0 && self.pending_minimum.is_none()
    }

    fn ensure_complete(&self) -> Result<(), String> {
        if self.pending_minimum.is_some() {
            Err("ambiguous HID local usage range".to_owned())
        } else {
            Ok(())
        }
    }

    fn add_usage(&mut self, usage: Usage) -> Result<(), String> {
        if self.usage_count == self.usages.len() || self.order_count == self.order.len() {
            return Err("HID local usage list is oversized".to_owned());
        }
        let usage_index = self.usage_count;
        self.usages[usage_index] = usage;
        self.usage_count += 1;
        self.order[self.order_count] = LocalUsageRef::Usage(usage_index);
        self.order_count += 1;
        Ok(())
    }

    fn set_minimum(&mut self, minimum: Usage) -> Result<(), String> {
        if self.pending_minimum.is_some() {
            return Err("ambiguous HID local usage range".to_owned());
        }
        self.pending_minimum = Some(minimum);
        Ok(())
    }

    fn set_maximum(&mut self, maximum: Usage) -> Result<(), String> {
        let mut minimum = self
            .pending_minimum
            .take()
            .ok_or_else(|| "ambiguous HID local usage range".to_owned())?;
        if minimum.extended != maximum.extended
            || (minimum.extended && (minimum.page != maximum.page || minimum.id > maximum.id))
            || (!minimum.extended && minimum.id > maximum.id)
        {
            return Err("ambiguous HID local usage range".to_owned());
        }
        if !minimum.extended {
            minimum.page = maximum.page;
        }
        if self.range_count == self.ranges.len() || self.order_count == self.order.len() {
            return Err("HID local usage ranges are oversized".to_owned());
        }
        let range_index = self.range_count;
        self.ranges[range_index] = UsageRange { minimum, maximum };
        self.range_count += 1;
        self.order[self.order_count] = LocalUsageRef::Range(range_index);
        self.order_count += 1;
        Ok(())
    }

    fn complete_last_usage_page(&mut self, usage_page: u16) {
        let mut order_index = self.order_count;
        while order_index != 0 {
            order_index -= 1;
            match self.order[order_index] {
                LocalUsageRef::Usage(usage_index) => {
                    let usage = &mut self.usages[usage_index];
                    if usage.extended {
                        continue;
                    }
                    if usage.page == usage_page {
                        break;
                    }
                    usage.page = usage_page;
                }
                LocalUsageRef::Range(range_index) => {
                    let range = &mut self.ranges[range_index];
                    if range.minimum.extended {
                        continue;
                    }
                    if range.minimum.page == usage_page {
                        break;
                    }
                    range.minimum.page = usage_page;
                    range.maximum.page = usage_page;
                }
            }
        }
    }

    fn contains_page(&self, page: u16) -> bool {
        self.usages[..self.usage_count]
            .iter()
            .any(|usage| usage.page == page)
            || self.ranges[..self.range_count]
                .iter()
                .any(|range| range.minimum.page == page)
    }

    fn collection_usage(&self) -> Result<CollectionUsage, String> {
        self.ensure_complete()?;
        let mut result = CollectionUsage::default();
        if self.order_count != 0 {
            match self.order[0] {
                LocalUsageRef::Usage(usage_index) => result.note(self.usages[usage_index]),
                LocalUsageRef::Range(range_index) => {
                    result.note_collection_range(self.ranges[range_index])?
                }
            }
        }
        if result.ambiguous_controller {
            return Err("ambiguous HID application usage".to_owned());
        }
        Ok(result)
    }
}

#[derive(Clone, Copy, Default)]
struct CollectionUsage {
    controller: Option<u16>,
    blocked: bool,
    ambiguous_controller: bool,
}

impl CollectionUsage {
    fn note(&mut self, usage: Usage) {
        if usage.page != GENERIC_DESKTOP_PAGE {
            return;
        }
        match usage.id {
            USAGE_POINTER | USAGE_MOUSE | USAGE_KEYBOARD => self.blocked = true,
            USAGE_JOYSTICK | USAGE_GAMEPAD => self.note_controller(usage.id),
            _ => {}
        }
    }

    fn note_collection_range(&mut self, range: UsageRange) -> Result<(), String> {
        if range.minimum.page != range.maximum.page || range.minimum.id != range.maximum.id {
            return Err("ambiguous HID application usage".to_owned());
        }
        self.note(range.minimum);
        Ok(())
    }

    fn note_controller(&mut self, usage: u16) {
        match self.controller {
            None => self.controller = Some(usage),
            Some(existing) if existing != usage => self.ambiguous_controller = true,
            Some(_) => {}
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    index: usize,
    globals: GlobalState,
    global_stack: [GlobalState; MAX_GLOBAL_STACK_DEPTH],
    global_stack_len: usize,
    local: LocalState,
    collection_depth: usize,
    controller_usage: Option<u16>,
    blocked: bool,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            index: 0,
            globals: GlobalState::DEFAULT,
            global_stack: [GlobalState::DEFAULT; MAX_GLOBAL_STACK_DEPTH],
            global_stack_len: 0,
            local: LocalState::new(),
            collection_depth: 0,
            controller_usage: None,
            blocked: false,
        }
    }

    fn parse(mut self) -> Result<(u16, u16), String> {
        while self.index < self.bytes.len() {
            let prefix = self.bytes[self.index];
            self.index += 1;
            if prefix == 0xfe {
                return Err("unsupported long HID report item".to_owned());
            }

            let payload_len = match prefix & 0x03 {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 4,
            };
            if payload_len > self.bytes.len() - self.index {
                return Err("truncated HID report item".to_owned());
            }
            let payload_end = self.index + payload_len;
            let payload = &self.bytes[self.index..payload_end];
            self.index = payload_end;

            let item_type = (prefix >> 2) & 0x03;
            let item_tag = prefix >> 4;
            match item_type {
                0 => self.main_item(item_tag, payload)?,
                1 => self.global_item(item_tag, payload)?,
                2 => self.local_item(item_tag, payload)?,
                _ => return Err("unsupported HID report item type".to_owned()),
            }
        }

        if self.collection_depth != 0 {
            return Err("unbalanced HID report collections".to_owned());
        }
        if !self.local.is_empty() {
            return Err("HID report descriptor ends with an incomplete local item".to_owned());
        }
        if self.blocked {
            return Err(CONTROLLER_ERROR.to_owned());
        }
        self.controller_usage
            .map(|usage| (GENERIC_DESKTOP_PAGE, usage))
            .ok_or_else(|| CONTROLLER_ERROR.to_owned())
    }

    fn main_item(&mut self, tag: u8, payload: &[u8]) -> Result<(), String> {
        match tag {
            0x08 => {
                let flags = parse_main_flags(payload, "HID Input item", INPUT_FLAGS_MASK)?;
                self.local.ensure_complete()?;
                self.local.complete_last_usage_page(self.globals.usage_page);
                let keyboard_page_input = self.local.contains_page(KEYBOARD_PAGE)
                    || (self.local.is_empty() && self.globals.usage_page == KEYBOARD_PAGE);
                if flags & INPUT_CONSTANT == 0 && keyboard_page_input {
                    self.blocked = true;
                }
                self.local.clear();
                Ok(())
            }
            0x09 | 0x0b => {
                parse_main_flags(payload, "HID output/feature item", MAIN_FLAGS_MASK)?;
                self.local.ensure_complete()?;
                self.local.complete_last_usage_page(self.globals.usage_page);
                self.local.clear();
                Ok(())
            }
            0x0a => {
                self.local.ensure_complete()?;
                self.local.complete_last_usage_page(self.globals.usage_page);
                let collection_type = read_value(payload);
                if (0x07..=0x7f).contains(&collection_type) || collection_type > 0xff {
                    return Err("unsupported HID collection type".to_owned());
                }
                if self.collection_depth == MAX_COLLECTION_DEPTH {
                    return Err("HID collection nesting is too deep".to_owned());
                }
                if collection_type == u32::from(COLLECTION_APPLICATION) {
                    let collection_usage = self.local.collection_usage()?;
                    self.blocked |= collection_usage.blocked;
                    if let Some(usage) = collection_usage.controller {
                        self.controller_usage.get_or_insert(usage);
                    }
                }
                self.collection_depth += 1;
                self.local.clear();
                Ok(())
            }
            0x0c => {
                expect_len(payload, 0, "HID End Collection item")?;
                self.local.ensure_complete()?;
                if !self.local.is_empty() {
                    return Err("HID End Collection item has local usage data".to_owned());
                }
                if self.collection_depth == 0 {
                    return Err("unbalanced HID End Collection item".to_owned());
                }
                self.collection_depth -= 1;
                self.local.clear();
                Ok(())
            }
            _ => Err("unsupported HID main item".to_owned()),
        }
    }

    fn global_item(&mut self, tag: u8, payload: &[u8]) -> Result<(), String> {
        match tag {
            0x00 => {
                self.globals.usage_page = parse_usage_page(payload)?;
                Ok(())
            }
            0x01..=0x07 | 0x09 => expect_value_width(payload, "HID global item"),
            0x08 => {
                expect_value_width(payload, "HID Report ID item")?;
                let report_id = read_value(payload);
                if report_id == 0 || report_id > u32::from(u8::MAX) {
                    return Err("HID Report ID is invalid".to_owned());
                }
                Ok(())
            }
            0x0a => {
                expect_len(payload, 0, "HID Push item")?;
                if self.global_stack_len == self.global_stack.len() {
                    return Err("HID global state stack is too deep".to_owned());
                }
                self.global_stack[self.global_stack_len] = self.globals;
                self.global_stack_len += 1;
                Ok(())
            }
            0x0b => {
                expect_len(payload, 0, "HID Pop item")?;
                if self.global_stack_len == 0 {
                    return Err("HID global state stack underflow".to_owned());
                }
                self.global_stack_len -= 1;
                self.globals = self.global_stack[self.global_stack_len];
                Ok(())
            }
            _ => Err("unsupported HID global item".to_owned()),
        }
    }

    fn local_item(&mut self, tag: u8, payload: &[u8]) -> Result<(), String> {
        match tag {
            0x00 => self
                .local
                .add_usage(parse_usage(payload, self.globals.usage_page)?),
            0x01 => self
                .local
                .set_minimum(parse_usage(payload, self.globals.usage_page)?),
            0x02 => self
                .local
                .set_maximum(parse_usage(payload, self.globals.usage_page)?),
            0x03..=0x05 | 0x07..=0x09 => expect_value_width(payload, "HID local item"),
            0x0a => Err("unsupported HID local delimiter".to_owned()),
            _ => Err("unsupported HID local item".to_owned()),
        }
    }
}

fn expect_len(payload: &[u8], expected: usize, item: &str) -> Result<(), String> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(format!("{item} has an invalid length"))
    }
}

fn expect_value_width(payload: &[u8], item: &str) -> Result<(), String> {
    if matches!(payload.len(), 1 | 2 | 4) {
        Ok(())
    } else {
        Err(format!("{item} has an invalid length"))
    }
}

fn parse_main_flags(payload: &[u8], item: &str, mask: u32) -> Result<u32, String> {
    let flags = read_value(payload);
    if flags & !mask != 0 {
        Err(format!("{item} has unsupported flags"))
    } else {
        Ok(flags)
    }
}

fn read_value(payload: &[u8]) -> u32 {
    payload
        .iter()
        .enumerate()
        .fold(0u32, |value, (offset, byte)| {
            value | (u32::from(*byte) << (offset * 8))
        })
}

fn parse_usage_page(payload: &[u8]) -> Result<u16, String> {
    expect_value_width(payload, "HID Usage Page item")?;
    u16::try_from(read_value(payload)).map_err(|_| "HID usage page overflows 16 bits".to_owned())
}

fn parse_usage(payload: &[u8], usage_page: u16) -> Result<Usage, String> {
    expect_value_width(payload, "HID Usage item")?;
    let value = read_value(payload);
    match payload.len() {
        1 | 2 => Ok(Usage {
            page: usage_page,
            id: value as u16,
            extended: false,
        }),
        4 => Ok(Usage {
            page: (value >> 16) as u16,
            id: value as u16,
            extended: true,
        }),
        _ => Err("HID Usage item has an invalid length".to_owned()),
    }
}

/// Return the Generic Desktop Joystick or Gamepad usage of a controller-only
/// HID Application collection.
///
/// This function intentionally rejects ambiguous or unsupported descriptor
/// constructs.  In particular, an occurrence of a controller usage is not
/// evidence unless it belongs to an Application collection, and a real
/// keyboard or Mouse/Pointer Application vetoes an otherwise valid gamepad.
pub(crate) fn controller_usage(bytes: &[u8]) -> Result<(u16, u16), String> {
    if bytes.is_empty() || bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err("HID report descriptor is unavailable or oversized".to_owned());
    }
    Parser::new(bytes).parse()
}

#[cfg(test)]
mod tests {
    use super::controller_usage;

    const GAMEPAD_APP: &[u8] = &[0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x81, 0x02, 0xc0];

    #[test]
    fn accepts_generic_desktop_gamepad_application() {
        assert_eq!(controller_usage(GAMEPAD_APP), Ok((0x01, 0x05)));

        let joystick = [0x05, 0x01, 0x09, 0x04, 0xa1, 0x01, 0x81, 0x02, 0xc0];
        assert_eq!(controller_usage(&joystick), Ok((0x01, 0x04)));
    }

    #[test]
    fn accepts_extended_application_usage() {
        let descriptor = [
            0x0b, 0x05, 0x00, 0x01, 0x00, // Usage 0x0001:0005
            0xa1, 0x01, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(controller_usage(&descriptor), Ok((0x01, 0x05)));
    }

    #[test]
    fn resolves_short_usage_pages_at_main_items_but_preserves_extended_pages() {
        let short_page_transition = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x06, 0x00, 0xff, 0x09, 0x06, 0x05, 0x07, 0x81,
            0x02, 0xc0,
        ];
        assert!(controller_usage(&short_page_transition).is_err());

        let extended_page_override = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x06, 0x00, 0xff, 0x0b, 0x06, 0x00, 0xff, 0x00,
            0x05, 0x07, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(controller_usage(&extended_page_override), Ok((0x01, 0x05)));
    }

    #[test]
    fn application_identity_uses_first_local_usage() {
        let vendor_first = [
            0x0b, 0x01, 0x00, 0xff, 0x00, 0x0b, 0x05, 0x00, 0x01, 0x00, 0xa1, 0x01, 0x81, 0x02,
            0xc0,
        ];
        assert!(controller_usage(&vendor_first).is_err());
    }

    #[test]
    fn push_and_pop_restore_usage_page() {
        let descriptor = [
            0x05, 0x01, // Generic Desktop
            0xa4, // Push
            0x05, 0x07, 0x09, 0x06, 0x81, 0x01, // constant keyboard-page input
            0xb4, // Pop
            0x09, 0x05, 0xa1, 0x01, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(controller_usage(&descriptor), Ok((0x01, 0x05)));
    }

    #[test]
    fn accepts_defined_main_widths_and_nonzero_report_ids() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x80, 0x82, 0x02, 0x00, 0x83, 0x02, 0x00, 0x00,
            0x00, 0x82, 0x02, 0x01, // Buffered Bytes flag in a wide Input item
            0x90, 0x92, 0x80, 0x00, // Output permits the Volatile bit at bit 7
            0x92, 0x00, 0x00, 0x93, 0x00, 0x00, 0x00, 0x00, 0xb0, 0xb2, 0x00, 0x00, 0xb3, 0x00,
            0x00, 0x00, 0x00, 0x85, 0x01, 0x86, 0x02, 0x00, 0x87, 0x03, 0x00, 0x00, 0x00, 0xc0,
        ];
        assert_eq!(controller_usage(&descriptor), Ok((0x01, 0x05)));

        let zero_report_id = [0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x00, 0xc0];
        assert!(controller_usage(&zero_report_id).is_err());

        let reserved_input_flag = [0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x81, 0x82, 0xc0];
        assert!(controller_usage(&reserved_input_flag).is_err());

        let wide_application = [0x05, 0x01, 0x09, 0x05, 0xa2, 0x01, 0x00, 0x81, 0x02, 0xc0];
        assert_eq!(controller_usage(&wide_application), Ok((0x01, 0x05)));

        let zero_width_physical = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0xa0, 0x81, 0x02, 0xc0, 0xc0,
        ];
        assert_eq!(controller_usage(&zero_width_physical), Ok((0x01, 0x05)));

        let four_byte_application = [
            0x05, 0x01, 0x09, 0x05, 0xa3, 0x01, 0x00, 0x00, 0x00, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(controller_usage(&four_byte_application), Ok((0x01, 0x05)));
    }

    #[test]
    fn controller_usage_outside_application_is_not_evidence() {
        let descriptor = [0x05, 0x01, 0x09, 0x04, 0x81, 0x02];
        assert!(controller_usage(&descriptor).is_err());
    }

    #[test]
    fn rejects_keyboard_application_and_keyboard_page_input() {
        let keyboard_application = [0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x81, 0x02, 0xc0];
        assert!(controller_usage(&keyboard_application).is_err());

        let mixed = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x81, 0x02, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
            0x81, 0x02, 0xc0,
        ];
        assert!(controller_usage(&mixed).is_err());
    }

    #[test]
    fn constant_keyboard_page_input_does_not_veto_controller() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x05, 0x07, 0x09, 0x06, 0x81, 0x01, 0xc0,
        ];
        assert_eq!(controller_usage(&descriptor), Ok((0x01, 0x05)));
    }

    #[test]
    fn rejects_mouse_or_pointer_application_even_with_gamepad() {
        let mixed_mouse = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x81, 0x02, 0xc0, 0x05, 0x01, 0x09, 0x02, 0xa1,
            0x01, 0x81, 0x06, 0xc0,
        ];
        assert!(controller_usage(&mixed_mouse).is_err());

        let mixed_pointer = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x81, 0x02, 0xc0, 0x05, 0x01, 0x09, 0x01, 0xa1,
            0x01, 0x81, 0x06, 0xc0,
        ];
        assert!(controller_usage(&mixed_pointer).is_err());
    }

    #[test]
    fn preserves_relative_and_absolute_axes_in_gamepad_application() {
        let relative = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x09, 0x30, 0x81, 0x06, 0xc0,
        ];
        assert_eq!(controller_usage(&relative), Ok((0x01, 0x05)));

        let absolute = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x09, 0x30, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(controller_usage(&absolute), Ok((0x01, 0x05)));
    }

    #[test]
    fn preserves_vendor_and_absolute_touchpad_collections() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x02, 0x15,
            0x00, 0x26, 0xff, 0x00, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0xc0, 0x05, 0x01, 0x09,
            0x01, 0xa1, 0x00, 0x06, 0x0d, 0x00, 0x09, 0x22, 0x81, 0x02, 0xc0, 0xc0,
        ];
        assert_eq!(controller_usage(&descriptor), Ok((0x01, 0x05)));
    }

    #[test]
    fn rejects_ambiguous_ranges_and_incomplete_structure() {
        let ambiguous_application = [
            0x05, 0x01, 0x19, 0x04, 0x29, 0x05, 0xa1, 0x01, 0x81, 0x02, 0xc0,
        ];
        assert!(controller_usage(&ambiguous_application).is_err());

        let incomplete_range = [0x05, 0x07, 0x19, 0x00, 0x05, 0x01, 0x09, 0x05, 0xa1, 0x01];
        assert!(controller_usage(&incomplete_range).is_err());

        let unbalanced = [0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x81, 0x02];
        assert!(controller_usage(&unbalanced).is_err());
    }

    #[test]
    fn rejects_truncated_long_and_delimited_items() {
        assert!(controller_usage(&[0x05]).is_err());
        assert!(controller_usage(&[0xfe, 0x01, 0x00, 0x00]).is_err());
        assert!(controller_usage(&[0xa9, 0x01]).is_err());
        assert!(controller_usage(&[0xb4]).is_err());
    }
}
