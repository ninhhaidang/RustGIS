//! Running a plugin's WebAssembly module in a sandbox.
//!
//! The module is interpreted by `wasmi` with no imports but RustGIS's own (no files, no
//! network, no clock): everything it learns and does goes through the [`Host`]. Each call gets
//! a fresh instance, a fuel budget (so an endless loop stops) and a memory cap.
//!
//! The ABI (version 1) is JSON over linear memory:
//!
//! - the module exports `memory`, `rustgis_alloc(len: i32) -> i32` (room for `len` bytes) and
//!   `rustgis_call(ptr: i32, len: i32) -> i64`, which reads a request and returns where its
//!   reply is, packed as `ptr << 32 | len`. A reply is `{"ok": value}` or `{"error": "text"}`.
//! - RustGIS provides, in module `rustgis`: `call(ptr, len) -> i32`, which runs a host request
//!   (`{"op": "...", "params": {...}}`) and returns the length of its reply (`-1` when there is
//!   none); `result(ptr)`, which copies that reply to `ptr`; and `log(ptr, len)`.

use serde_json::{Value, json};
use wasmi::{Caller, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

/// Fuel (roughly, WebAssembly instructions) for one command, and for one tool run.
/// Both run on the thread that runs commands (the UI's). A command does little work of its
/// own, so a runaway one is stopped in a fraction of a second. A tool may do real work (a
/// 45,000-cell grid takes about two thirds of this budget), so a runaway one runs for about 15
/// seconds in a release build before it is stopped.
pub const COMMAND_FUEL: u64 = 500_000_000;
pub const TOOL_FUEL: u64 = 30_000_000_000;
/// The most memory one instance may use.
pub const MAX_MEMORY: usize = 512 * 1024 * 1024;
/// The most host requests one call may make, and the largest request or reply.
const MAX_HOST_CALLS: u32 = 100_000;
const MAX_MESSAGE: usize = 128 * 1024 * 1024;
const MAX_LOG: usize = 4_096;
/// The most entries a module's function tables may hold.
const MAX_TABLE: usize = 100_000;

/// What a plugin can ask of RustGIS while it runs.
pub trait Host {
    /// Runs a host request: a RustGIS command by id, or a `host.*` request.
    fn call(&mut self, op: &str, params: &Value) -> Result<Value, String>;
    /// A line the plugin logged.
    fn log(&mut self, text: &str) {
        log::info!("plugin: {text}");
    }
    /// What the plugin is doing now: a tool's progress, shown while it runs and then forgotten.
    fn progress(&mut self, text: &str) {
        log::debug!("plugin progress: {text}");
    }
    /// Whether the run was cancelled. A process plugin is then stopped; a WebAssembly one runs
    /// on to its end (its fuel bounds it), and the caller discards what it returns.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A host that only logs (tools run without access to the session).
pub struct NoHost;

impl Host for NoHost {
    fn call(&mut self, op: &str, _: &Value) -> Result<Value, String> {
        Err(format!("'{op}' isn't available here"))
    }
}

/// The engine every module is compiled with: fuel metering on.
pub fn engine() -> &'static Engine {
    static ENGINE: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut c = Config::default();
        c.consume_fuel(true);
        Engine::new(&c)
    })
}

/// Compiles (and validates) a module, checking it has the exports the ABI needs.
pub fn compile(wasm: &[u8]) -> Result<Module, String> {
    let module = Module::new(engine(), wasm).map_err(|e| format!("not a valid WebAssembly module: {e}"))?;
    for name in ["memory", "rustgis_alloc", "rustgis_call"] {
        if module.get_export(name).is_none() {
            return Err(format!("the module doesn't export '{name}' (was it built with the RustGIS plugin SDK?)"));
        }
    }
    for import in module.imports() {
        if import.module() != "rustgis" || !matches!(import.name(), "call" | "result" | "log") {
            return Err(format!("the module needs '{}.{}', which RustGIS doesn't provide", import.module(), import.name()));
        }
    }
    Ok(module)
}

struct State<'a> {
    host: &'a mut dyn Host,
    /// The reply to the last host request, until the module copies it.
    reply: Option<Vec<u8>>,
    calls: u32,
    limits: StoreLimits,
}

/// Copies `len` bytes at `ptr` out of the module's memory, checking the range before copying
/// anything (a hostile length must not make the host allocate).
fn read(mem: &Memory, store: impl wasmi::AsContext, ptr: i32, len: i32) -> Option<Vec<u8>> {
    let (ptr, len) = (usize::try_from(ptr).ok()?, usize::try_from(len).ok()?);
    if len > MAX_MESSAGE {
        return None;
    }
    mem.data(&store).get(ptr..ptr.checked_add(len)?).map(<[u8]>::to_vec)
}

fn memory_of(caller: &Caller<'_, State<'_>>) -> Option<Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Some(m),
        _ => None,
    }
}

fn link<'a>(linker: &mut Linker<State<'a>>) -> Result<(), String> {
    linker
        .func_wrap("rustgis", "call", |mut caller: Caller<'_, State<'a>>, ptr: i32, len: i32| -> i32 {
            let Some(mem) = memory_of(&caller) else { return -1 };
            let too_many = {
                let st = caller.data_mut();
                st.calls = st.calls.saturating_add(1);
                st.calls > MAX_HOST_CALLS
            };
            let req = if too_many { None } else { read(&mem, &caller, ptr, len).map(|b| serde_json::from_slice::<Value>(&b)) };
            let reply = match req {
                None if too_many => json!({"error": "too many requests to RustGIS in one call"}),
                Some(Ok(req)) => {
                    let op = req.get("op").and_then(Value::as_str).unwrap_or_default().to_string();
                    let params = req.get("params").cloned().unwrap_or(Value::Null);
                    let host = &mut caller.data_mut().host;
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.call(&op, &params))) {
                        Ok(Ok(v)) => json!({"ok": v}),
                        Ok(Err(e)) => json!({"error": e}),
                        Err(_) => json!({"error": format!("'{op}' failed unexpectedly")}),
                    }
                }
                _ => json!({"error": "unreadable request"}),
            };
            let bytes = reply.to_string().into_bytes();
            let n = i32::try_from(bytes.len()).unwrap_or(-1);
            caller.data_mut().reply = (n >= 0).then_some(bytes);
            n
        })
        .map_err(|e| e.to_string())?;
    linker
        .func_wrap("rustgis", "result", |mut caller: Caller<'_, State<'a>>, ptr: i32| {
            let Some(mem) = memory_of(&caller) else { return };
            let Some(bytes) = caller.data_mut().reply.take() else { return };
            if let Ok(ptr) = usize::try_from(ptr) {
                let _ = mem.write(&mut caller, ptr, &bytes);
            }
        })
        .map_err(|e| e.to_string())?;
    linker
        .func_wrap("rustgis", "log", |mut caller: Caller<'_, State<'a>>, ptr: i32, len: i32| {
            let Some(mem) = memory_of(&caller) else { return };
            if let Some(b) = read(&mem, &caller, ptr, len.min(MAX_LOG as i32)) {
                let text = String::from_utf8_lossy(&b).to_string();
                caller.data_mut().host.log(&text);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Calls the module with `request`, giving it `fuel`; returns its `ok` value or its error.
pub fn call(module: &Module, host: &mut dyn Host, request: &Value, fuel: u64) -> Result<Value, String> {
    let state = State {
        host,
        reply: None,
        calls: 0,
        limits: StoreLimitsBuilder::new().memory_size(MAX_MEMORY).table_elements(MAX_TABLE).instances(1).memories(1).tables(16).build(),
    };
    let mut store = Store::new(engine(), state);
    store.limiter(|s| &mut s.limits);
    store.set_fuel(fuel).map_err(|e| e.to_string())?;
    let mut linker = Linker::new(engine());
    link(&mut linker)?;
    let trap = |e: wasmi::Error| {
        if matches!(e.as_trap_code(), Some(wasmi::TrapCode::OutOfFuel)) {
            crate::RAN_TOO_LONG.to_string()
        } else {
            format!("it stopped with an error ({e})")
        }
    };
    let instance = linker.instantiate_and_start(&mut store, module).map_err(trap)?;
    let mem = instance.get_memory(&store, "memory").ok_or("the module has no memory")?;
    let alloc: TypedFunc<i32, i32> = instance.get_typed_func(&store, "rustgis_alloc").map_err(|e| format!("rustgis_alloc: {e}"))?;
    let entry: TypedFunc<(i32, i32), i64> = instance.get_typed_func(&store, "rustgis_call").map_err(|e| format!("rustgis_call: {e}"))?;
    let bytes = request.to_string().into_bytes();
    let len = i32::try_from(bytes.len()).map_err(|_| "the request is too large")?;
    // A request too large for the module's memory (large layers or files) fails here: say so.
    let too_large = || format!("the request ({} MB of layers and files) doesn't fit in the plugin's memory", bytes.len() / (1024 * 1024));
    let ptr = alloc.call(&mut store, len).map_err(|e| if e.as_trap_code().is_some() { too_large() } else { trap(e) })?;
    mem.write(&mut store, usize::try_from(ptr).map_err(|_| "bad allocation")?, &bytes).map_err(|_| too_large())?;
    let packed = entry.call(&mut store, (ptr, len)).map_err(trap)?;
    let (out_ptr, out_len) = ((packed as u64 >> 32) as u32, packed as u64 as u32);
    let out = read(&mem, &store, out_ptr as i32, out_len as i32).ok_or("the module's reply is out of bounds")?;
    let reply: Value = serde_json::from_slice(&out).map_err(|e| format!("the module's reply isn't JSON: {e}"))?;
    match (reply.get("ok"), reply.get("error")) {
        (_, Some(e)) => Err(e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())),
        (Some(v), None) => Ok(v.clone()),
        (None, None) => Err("the module's reply has neither 'ok' nor 'error'".into()),
    }
}
