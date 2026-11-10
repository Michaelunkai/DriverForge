// Minimal late-bound COM IDispatch driver (ole32/oleaut32 only). Used by the Windows Update sweep.
//
// Ownership rules (audited):
//  * `ComObj` owns exactly one reference; Drop calls IUnknown::Release once.
//  * VARIANTs we build for arguments are always released with `VariantClear` (frees BSTRs). Object
//    arguments are AddRef'd when placed in a variant so VariantClear's Release balances them.
//  * VARIANT results are converted into `Val` and then cleared; object results transfer their
//    reference into a `ComObj` (the variant is neutralised first so it is not released twice).
//  * EXCEPINFO BSTRs (source/description/help) are all freed.
use std::ffi::c_void;
use std::ptr::null_mut;

pub const VT_EMPTY: u16 = 0;
pub const VT_NULL: u16 = 1;
pub const VT_I2: u16 = 2;
pub const VT_I4: u16 = 3;
pub const VT_R4: u16 = 4;
pub const VT_R8: u16 = 5;
pub const VT_DATE: u16 = 7;
pub const VT_BSTR: u16 = 8;
pub const VT_DISPATCH: u16 = 9;
pub const VT_ERROR: u16 = 10;
pub const VT_BOOL: u16 = 11;
pub const VT_DECIMAL: u16 = 14;
pub const VT_I1: u16 = 16;
pub const VT_UI1: u16 = 17;
pub const VT_UI2: u16 = 18;
pub const VT_UI4: u16 = 19;
pub const VT_I8: u16 = 20;
pub const VT_UI8: u16 = 21;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Guid {
    pub d1: u32,
    pub d2: u16,
    pub d3: u16,
    pub d4: [u8; 8],
}

/// VARIANT: 24 bytes on x64 (8-byte header + 16-byte union holding BRECORD's two pointers).
/// `align(8)` matches the native struct (it contains pointers / i64).
#[repr(C, align(8))]
pub struct Variant {
    pub vt: u16,
    pub r1: u16,
    pub r2: u16,
    pub r3: u16,
    pub payload: [u8; 16],
}

impl Variant {
    pub fn new() -> Self {
        Variant { vt: VT_EMPTY, r1: 0, r2: 0, r3: 0, payload: [0u8; 16] }
    }
    fn word(&self) -> usize {
        usize::from_ne_bytes(self.payload[0..std::mem::size_of::<usize>()].try_into().unwrap())
    }
    fn set_word(&mut self, w: usize) {
        self.payload[0..std::mem::size_of::<usize>()].copy_from_slice(&w.to_ne_bytes());
    }
}

#[repr(C)]
pub struct ExcepInfo {
    pub w_code: u16,
    pub w_reserved: u16,
    pub bstr_source: *mut u16,
    pub bstr_desc: *mut u16,
    pub bstr_help: *mut u16,
    pub help_ctx: u32,
    pub reserved: *mut c_void,
    pub deferred: *mut c_void,
    pub scode: i32,
}

#[repr(C)]
pub struct DispParams {
    pub rgvarg: *mut Variant,
    pub named_ids: *mut i32,
    pub c_args: u32,
    pub c_named: u32,
}

type FnAddRef = unsafe extern "system" fn(*mut c_void) -> u32;
type FnRelease = unsafe extern "system" fn(*mut c_void) -> u32;
type FnGetIds = unsafe extern "system" fn(*mut c_void, *const Guid, *const *const u16, u32, u32, *mut i32) -> i32;
type FnInvoke = unsafe extern "system" fn(
    *mut c_void, i32, *const Guid, u32, u16, *mut DispParams, *mut Variant, *mut ExcepInfo, *mut u32,
) -> i32;

const DISPATCH_METHOD: u16 = 1;
const DISPATCH_PROPERTYGET: u16 = 2;
const DISPATCH_PROPERTYPUT: u16 = 4;
const DISPID_PROPERTYPUT: i32 = -3;
const LOCALE_USER: u32 = 0x0409;

/// Owned IDispatch pointer (one reference).
pub struct ComObj {
    raw: *mut c_void,
}

// WUA objects are only used from the thread that created them; Send lets a whole scan move into a worker.
unsafe impl Send for ComObj {}

fn excep_info_default() -> ExcepInfo {
    ExcepInfo {
        w_code: 0, w_reserved: 0, bstr_source: null_mut(), bstr_desc: null_mut(), bstr_help: null_mut(),
        help_ctx: 0, reserved: null_mut(), deferred: null_mut(), scode: 0,
    }
}

/// Read a BSTR (length-prefixed; uses SysStringLen so embedded NULs are safe) without freeing.
fn bstr_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let len = SysStringLen(p) as usize;
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

fn free_excep(e: &mut ExcepInfo) -> String {
    let desc = bstr_to_string(e.bstr_desc);
    unsafe {
        for p in [e.bstr_source, e.bstr_desc, e.bstr_help] {
            if !p.is_null() {
                SysFreeString(p);
            }
        }
    }
    e.bstr_source = null_mut();
    e.bstr_desc = null_mut();
    e.bstr_help = null_mut();
    desc
}

impl ComObj {
    /// Wrap a raw IDispatch pointer, taking ownership of one reference. Returns None for null.
    fn from_raw(raw: *mut c_void) -> Option<ComObj> {
        if raw.is_null() { None } else { Some(ComObj { raw }) }
    }

    unsafe fn slot(&self, idx: usize) -> *const c_void {
        let vtbl = *(self.raw as *const *const *const c_void);
        *vtbl.add(idx)
    }

    fn get_ids(&self, name: &str) -> Result<i32, String> {
        let wname = to_wide(name);
        let names = [wname.as_ptr()];
        let mut disp_id: i32 = 0;
        let iid_null = Guid::default();
        let hr = unsafe {
            let f = std::mem::transmute::<*const c_void, FnGetIds>(self.slot(5));
            f(self.raw, &iid_null, names.as_ptr(), 1, LOCALE_USER, &mut disp_id)
        };
        if hr < 0 {
            return Err(format!("GetIDsOfNames('{}') failed hr={:#x}", name, hr as u32));
        }
        Ok(disp_id)
    }

    /// One IDispatch::Invoke. `args` must already be in reverse (DISPPARAMS) order.
    fn invoke_once(&self, dispid: i32, flags: u16, args: &mut [Variant], put: bool) -> (i32, Variant, String) {
        let mut named: i32 = DISPID_PROPERTYPUT;
        let mut params = DispParams {
            rgvarg: if args.is_empty() { null_mut() } else { args.as_mut_ptr() },
            named_ids: if put { &mut named } else { null_mut() },
            c_args: args.len() as u32,
            c_named: if put { 1 } else { 0 },
        };
        let iid_null = Guid::default();
        let mut result = Variant::new();
        let mut excep = excep_info_default();
        let mut arg_err: u32 = 0;
        let hr = unsafe {
            let f = std::mem::transmute::<*const c_void, FnInvoke>(self.slot(6));
            f(self.raw, dispid, &iid_null, LOCALE_USER, flags, &mut params, &mut result, &mut excep, &mut arg_err)
        };
        let desc = free_excep(&mut excep);
        (hr, result, desc)
    }

    fn invoke(&self, name: &str, flags_list: &[u16], args: &[Val], put: bool) -> Result<Val, String> {
        let dispid = self.get_ids(name)?;
        let mut vars: Vec<Variant> = args.iter().rev().map(Val::to_variant).collect();
        let mut last = String::new();
        let mut outcome = Err(String::new());
        for &flags in flags_list {
            let (hr, mut result, desc) = self.invoke_once(dispid, flags, &mut vars, put);
            if hr >= 0 {
                outcome = Ok(Val::from_variant(&mut result));
                break;
            }
            variant_clear(&mut result);
            last = if desc.is_empty() {
                format!("hr={:#x}", hr as u32)
            } else {
                format!("hr={:#x}: {}", hr as u32, desc)
            };
        }
        for v in vars.iter_mut() {
            variant_clear(v);
        }
        outcome.map_err(|_| format!("Invoke '{}' failed ({})", name, last))
    }

    pub fn call(&self, name: &str, args: &[Val]) -> Result<Val, String> {
        self.invoke(name, &[DISPATCH_METHOD, DISPATCH_METHOD | DISPATCH_PROPERTYGET], args, false)
    }

    pub fn get(&self, name: &str) -> Result<Val, String> {
        self.invoke(name, &[DISPATCH_PROPERTYGET], &[], false)
    }

    /// IDispatch property put.
    pub fn put(&self, name: &str, val: &Val) -> Result<(), String> {
        self.invoke(name, &[DISPATCH_PROPERTYPUT], std::slice::from_ref(val), true).map(|_| ())
    }

    fn add_ref(&self) {
        unsafe {
            let f = std::mem::transmute::<*const c_void, FnAddRef>(self.slot(1));
            f(self.raw);
        }
    }
}

impl Drop for ComObj {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                let f = std::mem::transmute::<*const c_void, FnRelease>(self.slot(2));
                f(self.raw);
            }
            self.raw = null_mut();
        }
    }
}

#[derive(Debug)]
pub enum Val {
    Empty,
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Obj(ComObj),
}

impl std::fmt::Debug for ComObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ComObj(..)")
    }
}

/// Convert an OLE DECIMAL stored in a VARIANT (vt overlaps wReserved; scale@2, sign@3, Hi32@4, Lo64@8).
pub fn decimal_to_f64(scale: u8, negative: bool, hi32: u32, lo64: u64) -> f64 {
    let mag = (hi32 as f64) * 18446744073709551616.0 + lo64 as f64;
    let v = mag / 10f64.powi(scale as i32);
    if negative { -v } else { v }
}

impl Val {
    pub fn as_str(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            _ => String::new(),
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            Val::Float(f) => Some(*f as i64),
            Val::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> f64 {
        match self {
            Val::Float(f) => *f,
            Val::Int(i) => *i as f64,
            _ => 0.0,
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            Val::Bool(b) => *b,
            Val::Int(i) => *i != 0,
            _ => false,
        }
    }
    pub fn into_obj(self) -> Option<ComObj> {
        match self {
            Val::Obj(o) => Some(o),
            _ => None,
        }
    }

    /// Build a VARIANT the caller must `variant_clear`. Strings allocate a BSTR; objects are AddRef'd.
    fn to_variant(&self) -> Variant {
        let mut v = Variant::new();
        match self {
            Val::Empty => {}
            Val::Int(i) => {
                v.vt = VT_I4;
                v.payload[0..4].copy_from_slice(&(*i as i32).to_ne_bytes());
            }
            Val::Float(f) => {
                v.vt = VT_R8;
                v.payload[0..8].copy_from_slice(&f.to_ne_bytes());
            }
            Val::Str(s) => {
                let b = sys_alloc_string(s);
                if !b.is_null() {
                    v.vt = VT_BSTR;
                    v.set_word(b as usize);
                }
            }
            Val::Bool(b) => {
                v.vt = VT_BOOL;
                let vb: i16 = if *b { -1 } else { 0 };
                v.payload[0..2].copy_from_slice(&vb.to_ne_bytes());
            }
            Val::Obj(o) => {
                o.add_ref();
                v.vt = VT_DISPATCH;
                v.set_word(o.raw as usize);
            }
        }
        v
    }

    /// Consume a result VARIANT (it is left VT_EMPTY / cleared).
    fn from_variant(v: &mut Variant) -> Val {
        let val = match v.vt {
            VT_EMPTY | VT_NULL | VT_ERROR => Val::Empty,
            VT_I1 => Val::Int(v.payload[0] as i8 as i64),
            VT_UI1 => Val::Int(v.payload[0] as i64),
            VT_I2 => Val::Int(i16::from_ne_bytes(v.payload[0..2].try_into().unwrap()) as i64),
            VT_UI2 => Val::Int(u16::from_ne_bytes(v.payload[0..2].try_into().unwrap()) as i64),
            VT_I4 => Val::Int(i32::from_ne_bytes(v.payload[0..4].try_into().unwrap()) as i64),
            VT_UI4 => Val::Int(u32::from_ne_bytes(v.payload[0..4].try_into().unwrap()) as i64),
            VT_I8 => Val::Int(i64::from_ne_bytes(v.payload[0..8].try_into().unwrap())),
            VT_UI8 => Val::Int(u64::from_ne_bytes(v.payload[0..8].try_into().unwrap()) as i64),
            VT_R4 => Val::Float(f32::from_ne_bytes(v.payload[0..4].try_into().unwrap()) as f64),
            VT_R8 | VT_DATE => Val::Float(f64::from_ne_bytes(v.payload[0..8].try_into().unwrap())),
            VT_BOOL => Val::Bool(i16::from_ne_bytes(v.payload[0..2].try_into().unwrap()) != 0),
            VT_DECIMAL => {
                // DECIMAL overlays the whole 16-byte VARIANT head: wReserved(vt) scale sign Hi32 Lo64.
                let scale = v.r1.to_ne_bytes()[0];
                let neg = v.r1.to_ne_bytes()[1] & 0x80 != 0;
                let hi = (v.r2 as u32) | ((v.r3 as u32) << 16);
                let lo = u64::from_ne_bytes(v.payload[0..8].try_into().unwrap());
                Val::Float(decimal_to_f64(scale, neg, hi, lo))
            }
            VT_BSTR => Val::Str(bstr_to_string(v.word() as *const u16)),
            VT_DISPATCH => {
                let raw = v.word() as *mut c_void;
                // Ownership of the reference moves into ComObj; neutralise the variant.
                v.vt = VT_EMPTY;
                v.set_word(0);
                return match ComObj::from_raw(raw) {
                    Some(o) => Val::Obj(o),
                    None => Val::Empty,
                };
            }
            _ => Val::Empty,
        };
        variant_clear(v);
        val
    }
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(pv: *mut c_void, coinit: u32) -> i32;
    fn CoCreateInstance(clsid: *const Guid, outer: *mut c_void, ctx: u32, riid: *const Guid, out: *mut *mut c_void) -> i32;
    fn CLSIDFromProgID(prog: *const u16, out: *mut Guid) -> i32;
}

#[link(name = "oleaut32")]
extern "system" {
    fn SysAllocStringLen(s: *const u16, len: u32) -> *mut u16;
    fn SysFreeString(s: *mut u16);
    fn SysStringLen(s: *const u16) -> u32;
    fn VariantClear(v: *mut Variant) -> i32;
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sys_alloc_string(s: &str) -> *mut u16 {
    let w: Vec<u16> = s.encode_utf16().collect();
    unsafe { SysAllocStringLen(w.as_ptr(), w.len() as u32) }
}

/// Init COM on the calling thread (STA). Safe to call repeatedly; a different existing mode
/// (RPC_E_CHANGED_MODE) is tolerated because late-bound calls still work.
pub fn com_initialize() {
    unsafe {
        CoInitializeEx(null_mut(), 2 /* COINIT_APARTMENTTHREADED */);
    }
}

fn variant_clear(v: &mut Variant) {
    unsafe {
        VariantClear(v as *mut Variant);
    }
}

pub fn create_from_progid(prog: &str) -> Result<ComObj, String> {
    let wide = to_wide(prog);
    let mut clsid = Guid::default();
    let hr = unsafe { CLSIDFromProgID(wide.as_ptr(), &mut clsid) };
    if hr < 0 {
        return Err(format!("CLSIDFromProgID('{}') hr={:#x}", prog, hr as u32));
    }
    // IID_IDispatch = {00020400-0000-0000-C000-000000000046}
    let iid = Guid { d1: 0x00020400, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
    let mut raw: *mut c_void = null_mut();
    // CLSCTX_INPROC_SERVER | CLSCTX_INPROC_HANDLER | CLSCTX_LOCAL_SERVER | CLSCTX_REMOTE_SERVER
    let hr = unsafe { CoCreateInstance(&clsid, null_mut(), 23, &iid, &mut raw) };
    if hr < 0 {
        return Err(format!("CoCreateInstance({}) hr={:#x}", prog, hr as u32));
    }
    ComObj::from_raw(raw).ok_or_else(|| format!("CoCreateInstance({}) returned null", prog))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Variant>(), 24);
        assert_eq!(std::mem::size_of::<DispParams>(), 24);
    }

    #[test]
    fn decimal_conversion() {
        assert_eq!(decimal_to_f64(0, false, 0, 1_048_576), 1_048_576.0);
        assert!((decimal_to_f64(2, false, 0, 12345) - 123.45).abs() < 1e-9);
        assert_eq!(decimal_to_f64(0, true, 0, 5), -5.0);
    }

    #[test]
    fn scalar_roundtrip() {
        let mut v = Val::Int(-7).to_variant();
        assert!(matches!(Val::from_variant(&mut v), Val::Int(-7)));
        let mut v = Val::Bool(true).to_variant();
        assert!(matches!(Val::from_variant(&mut v), Val::Bool(true)));
        let mut v = Val::Float(2.5).to_variant();
        assert!(matches!(Val::from_variant(&mut v), Val::Float(f) if f == 2.5));
    }

    #[test]
    fn string_roundtrip_and_clear() {
        let mut v = Val::Str("héllo wörld".into()).to_variant();
        assert_eq!(v.vt, VT_BSTR);
        match Val::from_variant(&mut v) {
            Val::Str(s) => assert_eq!(s, "héllo wörld"),
            o => panic!("{:?}", o),
        }
        assert_eq!(v.vt, VT_EMPTY);
    }

    #[test]
    fn real_com_object_lifecycle() {
        com_initialize();
        let coll = create_from_progid("Microsoft.Update.UpdateColl").expect("create");
        assert_eq!(coll.get("Count").unwrap().as_i64(), Some(0));
    }
}
