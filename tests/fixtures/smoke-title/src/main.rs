//! A synthetic title, written by us: it does what a title does to show
//! frames and nothing more. Through `d3d12_xs.dll` (HLE: victory-hle's
//! `d3d12x`, objects from victory-gpu) it configures graphics memory,
//! creates the D3D12.X device, a direct queue, a 1280x720 render target and
//! its view, a command allocator and list; then each frame it waits for
//! the frame's start (`WaitFrameEventX`), clears the target, submits the
//! list and presents the target (`PresentX`). After N frames it returns.
//!
//! Arguments (after `--` on `victory run`): `[frames] [exit code]`, default
//! 600 (10 s at 60 Hz: the frame meter logs every 5 s) and 0. A failed
//! step returns 10 + the step instead (10-24; a null device is 24), so
//! callers that pass an exit code keep below 10.
//!
//! Calls go through the objects' tables by offset, as titles' code does:
//! the offsets are the console's 2020-era layouts that victory-gpu
//! implements for a device created as `ID3D12Device`.
#![no_std]
#![no_main]

use core::ffi::{c_char, c_void};
use core::ptr::null;

type This = *const c_void;
type Hr = i32;
type Iid = [u8; 16];

#[link(name = "d3d12_xs", kind = "raw-dylib")]
unsafe extern "system" {
    fn D3D12ConfigureVirtualMemoryX(config: *mut VirtualMemory) -> Hr;
    fn D3D12XboxCreateDevice(
        adapter: This,
        params: *const u32,
        iid: *const Iid,
        out: *mut This,
    ) -> Hr;
}

// The CRT's argument accessors (HLE: the title's arguments).
#[link(name = "api-ms-win-crt-runtime-l1-1-0", kind = "raw-dylib")]
unsafe extern "C" {
    fn __p___argc() -> *mut i32;
    fn __p___argv() -> *mut *mut *mut c_char;
}

#[repr(C)]
struct VirtualMemory {
    flags: u32,
    page_table_pages: u32,
    preserved_bytes: u32,
    lowest: u64,
    highest: u64,
}

#[repr(C)]
struct HeapProperties {
    kind: u32,
    cpu_pages: u32,
    pool: u32,
    creation_nodes: u32,
    visible_nodes: u32,
}

#[repr(C)]
struct ResourceDesc {
    dimension: u32,
    alignment: u64,
    width: u64,
    height: u32,
    depth_or_array_size: u16,
    mip_levels: u16,
    format: u32,
    sample_count: u32,
    sample_quality: u32,
    layout: u32,
    flags: u32,
}

#[repr(C)]
struct ClearValue {
    format: u32,
    color: [f32; 4],
}

/// The plain description and an empty placement after it (newer titles'
/// heaps read one there).
#[repr(C)]
struct DescriptorHeapDesc {
    kind: u32,
    count: u32,
    flags: u32,
    nodes: u32,
    placement: [u64; 2],
}

/// A present plane: the frame's token and the resources shown.
#[repr(C)]
struct Plane {
    token: u64,
    count: u32,
    resources: *const This,
}

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> Iid {
    let (a, b, c) = (a.to_le_bytes(), b.to_le_bytes(), c.to_le_bytes());
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}
static IID_DEVICE: Iid = guid(
    0x189819f1,
    0x1db6,
    0x4b57,
    [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7],
);
static IID_QUEUE: Iid = guid(
    0x0ec870a6,
    0x5d7e,
    0x4c22,
    [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed],
);
static IID_RESOURCE: Iid = guid(
    0x696442be,
    0xa72e,
    0x4059,
    [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad],
);
static IID_HEAP: Iid = guid(
    0x8efb471d,
    0x616c,
    0x4f49,
    [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
);
static IID_ALLOCATOR: Iid = guid(
    0x6102dee4,
    0xaf59,
    0x4b09,
    [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24],
);
static IID_LIST: Iid = guid(
    0x5b160d0f,
    0xac1b,
    0x4185,
    [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55],
);

/// The interface version the device layouts are checked against (2.4).
const DEVICE_VERSION: u32 = 0x0002_0004;
const DIRECT: u32 = 0;
const RTV_HEAP: u32 = 2;
const RGBA8: u32 = 28;
const TEXTURE_2D: u32 = 3;
const ALLOW_RENDER_TARGET: u32 = 1;
const DEFAULT_HEAP: u32 = 1;
const RENDER_TARGET: u32 = 4;
const FRAME_ORIGIN: u32 = 0;
const SIXTIETH_US: u32 = 16_667;

/// The method at `offset` in `object`'s table.
///
/// # Safety
/// `object` is a COM object whose table has an `F` at `offset`.
unsafe fn method<F: Copy>(object: This, offset: usize) -> F {
    // SAFETY: forwarded; the table's first field is its address.
    unsafe {
        let table = *object.cast::<*const u8>();
        table.add(offset).cast::<F>().read()
    }
}

/// Decimal digits; `None` for anything else.
fn number(text: *const c_char) -> Option<u32> {
    if text.is_null() {
        return None;
    }
    let mut value: u32 = 0;
    let mut at = text;
    loop {
        // SAFETY: a NUL-terminated argument.
        let c = unsafe { *at } as u8;
        match c {
            0 if at != text => return Some(value),
            b'0'..=b'9' => value = value.checked_mul(10)?.checked_add(u32::from(c - b'0'))?,
            _ => return None,
        }
        // SAFETY: still inside the string (its NUL is not passed).
        at = unsafe { at.add(1) };
    }
}

/// `[frames] [exit code]` from the title's arguments.
fn arguments() -> (u32, u32) {
    // SAFETY: the runtime's argument accessors; `argv[argc]` is null.
    let (argc, argv) = unsafe { (*__p___argc(), *__p___argv()) };
    // SAFETY: as above, `argc` entries.
    let arg = |i: i32| {
        (i < argc)
            .then(|| unsafe { *argv.add(i as usize) })
            .and_then(|a| number(a))
    };
    (arg(1).unwrap_or(600), arg(2).unwrap_or(0))
}

/// Fails with 10 + `step` when `hr` is a failure.
fn check(hr: Hr, step: u32) -> Result<(), u32> {
    if hr < 0 { Err(10 + step) } else { Ok(()) }
}

fn run(frames: u32) -> Result<(), u32> {
    // SAFETY: the console's API, as titles call it; every object is ours
    // from the call that made it, and lives until the process ends.
    unsafe {
        let mut memory = VirtualMemory {
            flags: 0,
            page_table_pages: 64,
            preserved_bytes: 0,
            lowest: 0,
            highest: 0,
        };
        check(D3D12ConfigureVirtualMemoryX(&mut memory), 0)?;
        let params = [DEVICE_VERSION, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut device: This = null();
        check(
            D3D12XboxCreateDevice(null(), params.as_ptr(), &IID_DEVICE, &mut device),
            1,
        )?;
        if device.is_null() {
            return Err(24);
        }

        type Create = extern "system" fn(This, *const c_void, *const Iid, *mut This) -> Hr;
        let queue_desc = [DIRECT, 0, 0, 0];
        let mut queue: This = null();
        check(
            method::<Create>(device, 0x40)(
                device,
                queue_desc.as_ptr().cast(),
                &IID_QUEUE,
                &mut queue,
            ),
            2,
        )?;

        type Committed = extern "system" fn(
            This,
            *const HeapProperties,
            u32,
            *const ResourceDesc,
            u32,
            *const ClearValue,
            *const Iid,
            *mut This,
        ) -> Hr;
        let heap = HeapProperties {
            kind: DEFAULT_HEAP,
            cpu_pages: 0,
            pool: 0,
            creation_nodes: 1,
            visible_nodes: 1,
        };
        let desc = ResourceDesc {
            dimension: TEXTURE_2D,
            alignment: 0,
            width: 1280,
            height: 720,
            depth_or_array_size: 1,
            mip_levels: 1,
            format: RGBA8,
            sample_count: 1,
            sample_quality: 0,
            layout: 0,
            flags: ALLOW_RENDER_TARGET,
        };
        let clear = ClearValue {
            format: RGBA8,
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let mut target: This = null();
        check(
            method::<Committed>(device, 0xd8)(
                device,
                &heap,
                0,
                &desc,
                RENDER_TARGET,
                &clear,
                &IID_RESOURCE,
                &mut target,
            ),
            3,
        )?;

        let heap_desc = DescriptorHeapDesc {
            kind: RTV_HEAP,
            count: 1,
            flags: 0,
            nodes: 1,
            placement: [0, 0],
        };
        let mut rtv_heap: This = null();
        check(
            method::<Create>(device, 0x70)(
                device,
                (&raw const heap_desc).cast(),
                &IID_HEAP,
                &mut rtv_heap,
            ),
            4,
        )?;
        let rtv = method::<extern "system" fn(This) -> u64>(rtv_heap, 0x48)(rtv_heap);
        method::<extern "system" fn(This, This, *const c_void, u64)>(device, 0xa0)(
            device,
            target,
            null(),
            rtv,
        );

        type CreateAllocator = extern "system" fn(This, u32, *const Iid, *mut This) -> Hr;
        let mut allocator: This = null();
        check(
            method::<CreateAllocator>(device, 0x48)(device, DIRECT, &IID_ALLOCATOR, &mut allocator),
            5,
        )?;
        type CreateList =
            extern "system" fn(This, u32, u32, This, This, *const Iid, *mut This) -> Hr;
        let mut list: This = null();
        check(
            method::<CreateList>(device, 0x60)(
                device,
                0,
                DIRECT,
                allocator,
                null(),
                &IID_LIST,
                &mut list,
            ),
            6,
        )?;
        // Made open, as on PC.
        check(
            method::<extern "system" fn(This) -> Hr>(list, 0x48)(list),
            7,
        )?;

        // Frame pacing as titles do it: an interval, then a wait for each
        // frame's start (`VICTORY_FRAME_INTERVAL_MS` replaces the interval).
        type SetInterval = extern "system" fn(This, This, u32, u32, u32) -> Hr;
        check(
            method::<SetInterval>(device, 0x270)(device, null(), SIXTIETH_US, 0, 0),
            8,
        )?;
        type WaitFrame = extern "system" fn(This, u32, u32, *const u8, u32, *mut u64) -> Hr;
        let wait = method::<WaitFrame>(device, 0x280);
        let reset_allocator = method::<extern "system" fn(This) -> Hr>(allocator, 0x40);
        let reset = method::<extern "system" fn(This, This, This) -> Hr>(list, 0x50);
        let clear_rtv = method::<extern "system" fn(This, u64, *const [f32; 4], u32, *const c_void)>(
            list, 0x1a0,
        );
        let close = method::<extern "system" fn(This) -> Hr>(list, 0x48);
        let execute = method::<extern "system" fn(This, u32, *const This)>(queue, 0x50);
        type Present = extern "system" fn(This, u32, *const Plane, *const u8) -> Hr;
        let present = method::<Present>(queue, 0xf0);

        for frame in 0..frames {
            let mut token = 0u64;
            check(wait(device, FRAME_ORIGIN, 100, null(), 0, &mut token), 9)?;
            check(reset_allocator(allocator), 10)?;
            check(reset(list, allocator, null()), 11)?;
            // A colour that cycles, so a run that shows anything shows it moving.
            let t = (frame % 120) as f32 / 120.0;
            let color = [t, 0.25, 1.0 - t, 1.0];
            clear_rtv(list, rtv, &color, 0, null());
            check(close(list), 12)?;
            execute(queue, 1, &list);
            let plane = Plane {
                token,
                count: 1,
                resources: &target,
            };
            check(present(queue, 1, &plane, null()), 13)?;
        }
    }
    Ok(())
}

/// The entry point the loader calls (`/ENTRY`, build.rs); what it returns
/// is the title's exit code.
#[unsafe(no_mangle)]
pub extern "system" fn smoke_main(_: *mut c_void) -> u32 {
    let (frames, code) = arguments();
    match run(frames) {
        Ok(()) => code,
        Err(failed) => failed,
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // SAFETY: ends the title with an illegal instruction, which the loader
    // reports as a crash.
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}

/// The C runtime's floating-point marker and memory functions (there is no
/// C runtime: build.rs). Written with volatile accesses so that they are
/// not themselves turned into calls to these.
#[unsafe(no_mangle)]
pub static _fltused: i32 = 0;

#[unsafe(no_mangle)]
unsafe extern "C" fn memset(to: *mut c_void, value: i32, n: usize) -> *mut c_void {
    let to8 = to.cast::<u8>();
    for i in 0..n {
        // SAFETY: the caller passes `n` writable bytes.
        unsafe { to8.add(i).write_volatile(value as u8) };
    }
    to
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(to: *mut c_void, from: *const c_void, n: usize) -> *mut c_void {
    let (to8, from8) = (to.cast::<u8>(), from.cast::<u8>());
    for i in 0..n {
        // SAFETY: the caller passes `n` bytes each, not overlapping.
        unsafe { to8.add(i).write_volatile(from8.add(i).read_volatile()) };
    }
    to
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memmove(to: *mut c_void, from: *const c_void, n: usize) -> *mut c_void {
    if (to as usize) < (from as usize) {
        // SAFETY: forwarded; forwards copies are safe when `to` is below.
        return unsafe { memcpy(to, from, n) };
    }
    let (to8, from8) = (to.cast::<u8>(), from.cast::<u8>());
    for i in (0..n).rev() {
        // SAFETY: the caller passes `n` bytes each.
        unsafe { to8.add(i).write_volatile(from8.add(i).read_volatile()) };
    }
    to
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    for i in 0..n {
        // SAFETY: the caller passes `n` readable bytes each.
        let (x, y) = unsafe { (a.add(i).read_volatile(), b.add(i).read_volatile()) };
        if x != y {
            return i32::from(x) - i32::from(y);
        }
    }
    0
}
