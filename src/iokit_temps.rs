//! Apple silicon temperature sensors, read without root through IOKit's HID
//! event system (the same private API `macmon` and Stats use).
//!
//! macOS only. Nothing here is linked at build time: IOKit and CoreFoundation
//! are `dlopen`ed when the prober first asks for sensors, so the binary still
//! cross-compiles from Linux without a macOS SDK. If a symbol is missing or
//! Apple changes the API, `Sensors::open` returns `None` and insider falls
//! back to thermal pressure, as before.
//!
//! Private API, from IOKit's IOHIDEventSystemClient.h / IOHIDEvent.h:
//!
//! ```c
//! IOHIDEventSystemClientRef IOHIDEventSystemClientCreate(CFAllocatorRef);
//! int        IOHIDEventSystemClientSetMatching(IOHIDEventSystemClientRef, CFDictionaryRef);
//! CFArrayRef IOHIDEventSystemClientCopyServices(IOHIDEventSystemClientRef);
//! CFTypeRef  IOHIDServiceClientCopyProperty(IOHIDServiceClientRef, CFStringRef);
//! IOHIDEventRef IOHIDServiceClientCopyEvent(IOHIDServiceClientRef, int64_t type,
//!                                           int32_t options, int64_t timestamp);
//! double     IOHIDEventGetFloatValue(IOHIDEventRef, int32_t field);
//! ```

use std::ffi::{c_char, c_int, c_void, CStr};

type CFTypeRef = *const c_void;
type CFIndex = isize;

const RTLD_NOW: c_int = 2;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT32: CFIndex = 3;

/// kHIDPage_AppleVendor / kHIDUsage_AppleVendor_TemperatureSensor
const PAGE_APPLE_VENDOR: i32 = 0xff00;
const USAGE_TEMPERATURE_SENSOR: i32 = 5;
/// kIOHIDEventTypeTemperature; its value field is IOHIDEventFieldBase(type)
const EVENT_TEMPERATURE: i64 = 15;
const FIELD_TEMPERATURE: i32 = 15 << 16;

extern "C" {
    // libSystem, always linked
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Function pointers resolved from the two frameworks.
struct Api {
    cf_release: unsafe extern "C" fn(CFTypeRef),
    cf_get_type_id: unsafe extern "C" fn(CFTypeRef) -> usize,
    cf_string_get_type_id: unsafe extern "C" fn() -> usize,
    cf_string_create: unsafe extern "C" fn(CFTypeRef, *const c_char, u32) -> CFTypeRef,
    cf_string_get_cstring: unsafe extern "C" fn(CFTypeRef, *mut c_char, CFIndex, u32) -> u8,
    cf_number_create: unsafe extern "C" fn(CFTypeRef, CFIndex, *const c_void) -> CFTypeRef,
    cf_dictionary_create: unsafe extern "C" fn(
        CFTypeRef,
        *const CFTypeRef,
        *const CFTypeRef,
        CFIndex,
        *const c_void,
        *const c_void,
    ) -> CFTypeRef,
    cf_array_get_count: unsafe extern "C" fn(CFTypeRef) -> CFIndex,
    cf_array_get_value_at_index: unsafe extern "C" fn(CFTypeRef, CFIndex) -> CFTypeRef,
    /// &kCFTypeDictionaryKeyCallBacks / &kCFTypeDictionaryValueCallBacks
    dict_key_callbacks: *const c_void,
    dict_value_callbacks: *const c_void,

    client_create: unsafe extern "C" fn(CFTypeRef) -> CFTypeRef,
    client_set_matching: unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> c_int,
    client_copy_services: unsafe extern "C" fn(CFTypeRef) -> CFTypeRef,
    service_copy_property: unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> CFTypeRef,
    service_copy_event: unsafe extern "C" fn(CFTypeRef, i64, i32, i64) -> CFTypeRef,
    event_get_float_value: unsafe extern "C" fn(CFTypeRef, i32) -> f64,
}

/// Look up `name` (NUL-terminated) and reinterpret it as `T` (a fn pointer
/// or a data pointer).
unsafe fn sym<T: Copy>(lib: *mut c_void, name: &[u8]) -> Option<T> {
    debug_assert_eq!(name.last(), Some(&0));
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    let p = dlsym(lib, name.as_ptr().cast());
    (!p.is_null()).then(|| std::mem::transmute_copy(&p))
}

impl Api {
    unsafe fn load() -> Option<Api> {
        let cf = dlopen(
            b"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation\0"
                .as_ptr()
                .cast(),
            RTLD_NOW,
        );
        let io = dlopen(
            b"/System/Library/Frameworks/IOKit.framework/IOKit\0"
                .as_ptr()
                .cast(),
            RTLD_NOW,
        );
        if cf.is_null() || io.is_null() {
            return None;
        }
        Some(Api {
            cf_release: sym(cf, b"CFRelease\0")?,
            cf_get_type_id: sym(cf, b"CFGetTypeID\0")?,
            cf_string_get_type_id: sym(cf, b"CFStringGetTypeID\0")?,
            cf_string_create: sym(cf, b"CFStringCreateWithCString\0")?,
            cf_string_get_cstring: sym(cf, b"CFStringGetCString\0")?,
            cf_number_create: sym(cf, b"CFNumberCreate\0")?,
            cf_dictionary_create: sym(cf, b"CFDictionaryCreate\0")?,
            cf_array_get_count: sym(cf, b"CFArrayGetCount\0")?,
            cf_array_get_value_at_index: sym(cf, b"CFArrayGetValueAtIndex\0")?,
            dict_key_callbacks: sym(cf, b"kCFTypeDictionaryKeyCallBacks\0")?,
            dict_value_callbacks: sym(cf, b"kCFTypeDictionaryValueCallBacks\0")?,
            client_create: sym(io, b"IOHIDEventSystemClientCreate\0")?,
            client_set_matching: sym(io, b"IOHIDEventSystemClientSetMatching\0")?,
            client_copy_services: sym(io, b"IOHIDEventSystemClientCopyServices\0")?,
            service_copy_property: sym(io, b"IOHIDServiceClientCopyProperty\0")?,
            service_copy_event: sym(io, b"IOHIDServiceClientCopyEvent\0")?,
            event_get_float_value: sym(io, b"IOHIDEventGetFloatValue\0")?,
        })
    }

    unsafe fn cfstr(&self, s: &[u8]) -> CFTypeRef {
        (self.cf_string_create)(std::ptr::null(), s.as_ptr().cast(), CF_STRING_ENCODING_UTF8)
    }

    unsafe fn cfnum(&self, v: i32) -> CFTypeRef {
        (self.cf_number_create)(
            std::ptr::null(),
            CF_NUMBER_SINT32,
            (&v as *const i32).cast(),
        )
    }

    /// A CFString's contents, or None if `s` is null or not a string.
    unsafe fn string(&self, s: CFTypeRef) -> Option<String> {
        if s.is_null() || (self.cf_get_type_id)(s) != (self.cf_string_get_type_id)() {
            return None;
        }
        let mut buf = [0 as c_char; 256];
        if (self.cf_string_get_cstring)(
            s,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            CF_STRING_ENCODING_UTF8,
        ) == 0
        {
            return None;
        }
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    }
}

/// An open HID event-system client matched to the temperature sensors.
pub struct Sensors {
    api: Api,
    client: CFTypeRef,
    services: CFTypeRef,
    /// (index into `services`, product name)
    names: Vec<(CFIndex, String)>,
}

// The CF objects are only touched from the prober thread that owns this.
unsafe impl Send for Sensors {}

impl Sensors {
    pub fn open() -> Option<Sensors> {
        unsafe {
            let api = Api::load()?;
            let client = (api.client_create)(std::ptr::null());
            if client.is_null() {
                return None;
            }

            let keys = [
                api.cfstr(b"PrimaryUsagePage\0"),
                api.cfstr(b"PrimaryUsage\0"),
            ];
            let vals = [
                api.cfnum(PAGE_APPLE_VENDOR),
                api.cfnum(USAGE_TEMPERATURE_SENSOR),
            ];
            let matching = (api.cf_dictionary_create)(
                std::ptr::null(),
                keys.as_ptr(),
                vals.as_ptr(),
                2,
                api.dict_key_callbacks,
                api.dict_value_callbacks,
            );
            for o in keys.iter().chain(vals.iter()) {
                if !o.is_null() {
                    (api.cf_release)(*o);
                }
            }
            if matching.is_null() {
                (api.cf_release)(client);
                return None;
            }
            (api.client_set_matching)(client, matching);
            (api.cf_release)(matching);

            let services = (api.client_copy_services)(client);
            if services.is_null() {
                (api.cf_release)(client);
                return None;
            }

            let product = api.cfstr(b"Product\0");
            let mut names = Vec::new();
            for i in 0..(api.cf_array_get_count)(services) {
                let svc = (api.cf_array_get_value_at_index)(services, i);
                if svc.is_null() {
                    continue;
                }
                let p = (api.service_copy_property)(svc, product);
                if let Some(name) = api.string(p) {
                    names.push((i, name));
                }
                if !p.is_null() {
                    (api.cf_release)(p);
                }
            }
            if !product.is_null() {
                (api.cf_release)(product);
            }

            let s = Sensors {
                api,
                client,
                services,
                names,
            };
            (!s.names.is_empty()).then_some(s)
        }
    }

    /// Current reading of every sensor, in °C, as (product name, value).
    /// Implausible values (sensors that report 0 or garbage) are dropped.
    pub fn read(&self) -> Vec<(String, f64)> {
        let mut out = Vec::with_capacity(self.names.len());
        unsafe {
            for (i, name) in &self.names {
                let svc = (self.api.cf_array_get_value_at_index)(self.services, *i);
                if svc.is_null() {
                    continue;
                }
                let ev = (self.api.service_copy_event)(svc, EVENT_TEMPERATURE, 0, 0);
                if ev.is_null() {
                    continue;
                }
                let t = (self.api.event_get_float_value)(ev, FIELD_TEMPERATURE);
                (self.api.cf_release)(ev);
                if t.is_finite() && t > 0.0 && t < 150.0 {
                    out.push((name.clone(), t));
                }
            }
        }
        out
    }
}

impl Drop for Sensors {
    fn drop(&mut self) {
        unsafe {
            (self.api.cf_release)(self.services);
            (self.api.cf_release)(self.client);
        }
    }
}
