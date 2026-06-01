# rwave plugin protocol

rwave reads VCD and FST natively via its built-in `wellen` backend.
Additional waveform formats are not compiled into the rwave binary. They
are loaded at runtime from plugin shared libraries that conform to the
stable C ABI described here.

This document is the contract. A plugin that follows it will be loaded
by rwave without rebuilding the rwave binary.

## Lifecycle

1. User runs an rwave command on a file whose format is not built in.
2. rwave takes the file extension as the format token, e.g. `foo` for
   `data.foo`. The convention is: a plugin handling extension `<ext>`
   is packaged as `rwave_<ext>` and exports its cdylib as
   `librwave_<ext>_backend.{so,dll}`. No registry on rwave's side.
3. rwave looks for that plugin shared library on disk (see Discovery).
4. rwave `dlopen`s it and resolves the symbol `rwave_backend`.
5. rwave calls that function once per process. It returns a const vtable.
6. rwave validates `abi_version` in the vtable. Mismatch is fatal.
7. For each file, rwave calls `vtable.open(path)` to get an opaque
   `RwaveSession*` and drives metadata, hierarchy, and trace queries
   through the vtable. `vtable.close()` releases the file.

The vtable lives for the process lifetime; each `RwaveSession*` lives
between matching `open` / `close` calls.

## Platform support

Plugins load at runtime, so they do not affect rwave's own build matrix.
However, rwave only **attempts** plugin loading on platforms where a
plugin ecosystem is known to be viable:

| Platform        | Plugin loading attempted |
|-----------------|--------------------------|
| linux x86_64    | yes                      |
| windows x86_64  | yes                      |
| linux aarch64   | no                       |
| macos aarch64   | no                       |
| macos x86_64    | no                       |

On platforms where plugin loading is disabled, opening a non-built-in
format errors with `<format> extension is not supported on this
platform.` without trying to load anything.

The list is conservative. PRs adding platforms (with a matching plugin
ecosystem) are welcome.

### Linux release binary

The prebuilt `rwave-linux-amd64` is glibc-dynamic (manylinux2014
baseline) so it can `dlopen`. Source builds against
`x86_64-unknown-linux-musl` are static and cannot — `.wlf` opens fail
with `Dynamic loading not supported`. Built-in formats are unaffected.

## Discovery

When rwave needs a plugin for format `<f>`, it searches in this order:

1. Environment variable `RWAVE_PLUGIN_<F>` (uppercase format name),
   set to an absolute path to the plugin shared library. Example for
   format `foo`: `RWAVE_PLUGIN_FOO=/abs/path/to/librwave_foo_backend.so`.
2. `$VIRTUAL_ENV/lib/python3.*/site-packages/rwave_<f>/librwave_<f>_backend.{so,dll}`.
3. `~/.local/lib/python3.*/site-packages/rwave_<f>/...` (same filename pattern).

The Python-side paths exist because the canonical distribution channel
for plugins is a Python wheel (see Distribution). The env var is the
escape hatch for development, system-wide installs the scan misses,
and unusual install layouts.

If all of the above fail, rwave emits the "support not installed" error.

## ABI v1

The authoritative C header is `crates/rwave/include/rwave_backend.h`.
Shape:

```c
#define RWAVE_BACKEND_ABI_VERSION 1

typedef struct RwaveSession RwaveSession;  /* opaque to rwave */

typedef enum {
    RWAVE_FMT_UNKNOWN = 0,
    RWAVE_FMT_VCD     = 1,
    RWAVE_FMT_FST     = 2,
    RWAVE_FMT_GHW     = 3,
    /* Plugin formats report UNKNOWN — no per-format enum value for
     * external formats. */
} RwaveFileFormat;

typedef enum {
    RWAVE_VK_BITS  = 0,  /* 4-state MSB-first ASCII bit string */
    RWAVE_VK_REAL  = 1,  /* IEEE 754 double as decimal string */
    RWAVE_VK_STR   = 2,  /* opaque string payload */
    RWAVE_VK_EVENT = 3,  /* no payload */
} RwaveValueKind;

typedef struct {
    const char*    full_path;     /* hierarchical, dot-separated */
    const char*    scope_path;    /* enclosing scope only */
    uint32_t       width;         /* bits; 1 for scalar/real/string/event */
    const char*    type_str;      /* "wire", "reg", "real", "event", ... */
    RwaveValueKind kind;
    uint64_t       backend_sid;   /* opaque; aliases share the same value */
} RwaveVarDecl;

typedef void (*RwaveEmit)(
    void*       ctx,
    uint64_t    backend_sid,
    int64_t     time_tick,
    const char* value_buf,        /* NUL-terminated */
    uint32_t    value_len);

typedef struct {
    uint32_t        abi_version;          /* must equal RWAVE_BACKEND_ABI_VERSION */
    const char*     name;                 /* format token — same string
                                              rwave used to dlopen this
                                              cdylib (i.e. the file
                                              extension, lowercase) */
    const char*     version;              /* plugin's own version string */

    /* lifecycle */
    RwaveSession*   (*open)(const char* path, char** err_out);
    void            (*close)(RwaveSession*);
    void            (*free_err)(char* err);

    /* metadata */
    RwaveFileFormat (*file_format)(RwaveSession*);
    void            (*timescale)(RwaveSession*,
                                 double* secs_per_tick,
                                 const char** display);
    const char*     (*date)(RwaveSession*);
    const char*     (*version_str)(RwaveSession*);
    int             (*time_range)(RwaveSession*, int64_t* lo, int64_t* hi);
    size_t          (*time_step_count)(RwaveSession*);

    /* hierarchy: cap=0 returns total count; cap>0 fills buf up to cap items */
    size_t          (*var_decls)(RwaveSession*, RwaveVarDecl* buf, size_t cap);

    /* trace decode: stream events back via emit(ctx, ...) */
    int             (*load_traces)(
                        RwaveSession*,
                        const uint64_t* sids, size_t n_sids,
                        RwaveEmit emit, void* ctx);
} RwaveBackend;

/* The plugin's sole exported symbol. */
const RwaveBackend* rwave_backend(const char** err_out);
```

### Memory ownership

- Strings owned by the plugin (`full_path`, `type_str`, `name`,
  `version`, `date()`, etc.) must remain valid for the lifetime of the
  associated `RwaveSession*`. Vtable-level strings (`name`, `version`)
  must live for the process.
- Error strings returned via `char** err_out` are allocated by the
  plugin and freed by rwave calling `vtable.free_err()`. rwave never
  calls `free()` directly on plugin memory.
- `value_buf` passed to `RwaveEmit` is borrowed for the callback only.
  rwave copies what it keeps.
- The `RwaveVarDecl* buf` passed to `var_decls` is rwave's; the plugin
  fills it but does not retain the pointer.

### Threading

ABI v1 is single-threaded with respect to any one `RwaveSession*`.
rwave will not call concurrently into the same backend. Plugins may
use threads internally as long as their exported functions present a
synchronous interface.

### Errors

Every fallible function takes a `char** err_out`. On success leave it
NULL. On failure set it to a NUL-terminated, human-readable message.
rwave shows the message verbatim and then calls `vtable.free_err()`.

## Three versions, three semantics

This system carries three independent version numbers. Keeping them
distinct is what lets a single plugin keep working across many rwave
releases:

| Version | Owned by | Bumps when | Where it lives |
|---|---|---|---|
| **rwave version** | rwave | any rwave change (bug fix, command, perf) | `crates/rwave/Cargo.toml` |
| **plugin version** | the plugin author | any plugin change (vendor lib update, decoder fix, new field) | the plugin's own manifest, surfaced in the vtable's `version` field |
| **ABI version** | this protocol | breaking vtable changes only (field removed, signature changed, semantic change to an existing call) | `RWAVE_BACKEND_ABI_VERSION` constant in the header / vtable's `abi_version` field |

Rwave changing version does **not** force a plugin rebuild. A plugin
changing version does **not** force a rwave rebuild. The ABI version
is the only compatibility gate; it stays at the same value across
many rwave + plugin releases until a structural change forces a bump.

## Distribution: wheels

The canonical distribution channel for plugins is a Python wheel — not
because plugins are Python, but because wheels give us platform tags,
version pinning, and a universally installed installer (`pip`) without
requiring custom installers.

### Naming

```
rwave_<format>-<plugin_version>-py3-none-<platform>.whl
```

- `<format>` matches the vtable's `name` field, which in turn equals
  the file extension this plugin claims (e.g. a plugin for `.foo`
  files has `<format> = foo` everywhere — wheel filename, package
  directory, cdylib filename, vtable `name`).
- `<plugin_version>` is the plugin's own semver — independent of rwave's
  version. The runtime compatibility check is the vtable's `abi_version`
  field, not this string.
- `<platform>` is the PEP 425 platform tag:
  - Linux x86_64: `linux_x86_64`
  - Windows x86_64: `win_amd64`

### Layout

```
rwave_<format>/
├── __init__.py                       # empty; required for site-packages discovery
└── librwave_<format>_backend.<ext>   # .so on Linux, .dll on Windows
```

Supporting files (vendor libraries, license data) may live in
subdirectories of `rwave_<format>/` and be discovered relative to the
plugin's own `__file__`. rwave does not introspect those — they are
internal to the plugin.

## Errors rwave emits

| Scenario | Message |
|---|---|
| Plugin not installed | `Error: <format> support not installed. Install a rwave_<format> wheel for <platform>.` |
| Plugin found but load failed | `Error: <verbatim from dlopen / init err_out>` |
| ABI version mismatch | `Error: <format> backend ABI mismatch (plugin v<X>, rwave expects v<Y>). Reinstall a rwave_<format> wheel matching rwave's ABI version.` |
| Platform without plugin support | `Error: <format> extension is not supported on this platform.` |

The install hint is intentionally version-agnostic — rwave's version
is not encoded in the wheel name, so quoting one specific filename
would be misleading. The ABI-mismatch message is the only one that
names a version, because the version IS the problem there.

Plugin authors do not author these messages; rwave generates them. The
contract is that the plugin loads cleanly when present and reports a
useful `err_out` when it doesn't.

## Writing a plugin

Minimal Rust skeleton:

```rust
// Cargo.toml: crate-type = ["cdylib"]

use std::ffi::{c_char, c_void};

#[repr(C)]
pub struct RwaveBackend { /* … same shape as the C header … */ }

static VTABLE: RwaveBackend = RwaveBackend {
    abi_version: 1,
    name:    c"foo".as_ptr(),
    version: c"0.0.1".as_ptr(),
    open:    my_open,
    close:   my_close,
    // …
};

#[no_mangle]
pub extern "C" fn rwave_backend(_err: *mut *mut c_char)
    -> *const RwaveBackend
{
    &VTABLE
}
```

Compile as a `cdylib`, package into a wheel per the naming above, drop
it into site-packages or point `RWAVE_PLUGIN_FOO` at it, and rwave will
load it on the next matching open.

## Versioning policy

See the "Three versions, three semantics" section above for the
overview. Concretely:

- `RWAVE_BACKEND_ABI_VERSION` bumps **only** on breaking vtable changes
  (field removed, signature changed, semantic change to an existing
  call). Appending new fields at the end of the vtable does not bump
  it — older plugins continue to work; rwave consults only the fields
  they fill.
- The wheel's version string in the filename is the plugin's own
  semver. Plugin authors choose when to bump it (vendor dep refresh,
  decoder fix, new vtable field they decided to fill, etc.); rwave
  never reads it. The runtime compatibility check is `abi_version`.
- Rwave's own version string is independent of both of the above and
  never appears in any plugin-related filename or hint.

## Conformance checklist

A plugin is conformant if:

- [ ] Exports exactly one symbol: `rwave_backend`, C linkage,
      no name mangling.
- [ ] Returns a vtable whose `abi_version` equals
      `RWAVE_BACKEND_ABI_VERSION` at build time.
- [ ] All vtable function pointers are non-NULL.
- [ ] `name` is a stable, NUL-terminated string matching the format
      token rwave routes by.
- [ ] Survives at least one full open → query → close cycle without
      crashing or leaking.
- [ ] Reports failures via `err_out`, not by aborting.
- [ ] Single-threaded use of any one `RwaveSession*` is sufficient
      (rwave will not call concurrently into the same backend).

## Known plugins

None publicly registered yet. To register a plugin, send a PR adding a
row below.

| Format | Distribution | Maintainer | Status |
|--------|--------------|------------|--------|
| _yours here_ |        |            |        |
