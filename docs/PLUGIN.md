# rwave plugin protocol

rwave reads VCD, FST, and GHW natively via its built-in `wellen` backend.
Two more formats ship **compiled into** the linux-amd64 binary — WLF
(Mentor) and FSDB (Synopsys Verdi NPI) — and any other format is loaded
at runtime from an **external** plugin shared library. Built-in and
external backends speak the *same* stable C ABI described here; they
differ only in how rwave obtains the vtable (a compiled-in pointer vs.
`dlopen` + `dlsym`).

This document is the contract. An external plugin that follows it is
loaded by rwave without rebuilding the rwave binary.

## Lifecycle

1. User runs an rwave command on a file whose extension is not native
   (`vcd`/`fst`/`ghw`). rwave takes the extension as the format token,
   e.g. `foo` for `data.foo`.
2. rwave resolves a vtable for that token (see Discovery): an external
   plugin named by `$RWAVE_PLUGIN_<EXT>` wins; otherwise a compiled-in
   built-in (`wlf`/`fsdb`) is used.
3. For an external plugin, rwave `dlopen`s the `.so` and resolves the
   symbol `rwave_backend`, calling it once. For a built-in, rwave calls
   the equivalent compiled-in entry point. Either way it gets a const
   vtable.
4. rwave validates `abi_version` in the vtable. Mismatch is fatal.
5. For each file, rwave calls `vtable.open(path)` to get an opaque
   `RwaveSession*` and drives metadata, hierarchy, and trace queries
   through the vtable. `vtable.close()` releases the file.

The vtable lives for the process lifetime; each `RwaveSession*` lives
between matching `open` / `close` calls.

## Platform support

- **External plugins** (`$RWAVE_PLUGIN_<EXT>`) load via `dlopen` on any
  platform whose rwave build is dynamically linked.
- **Built-in WLF/FSDB** (experimental, amd64) compile in only for `x86_64`
  linux — the target with a runtime-loadable vendor library. On any other
  build, `.wlf`/`.fsdb` errors with `<format> support is only available in the
  linux-x86_64 build.` They sit behind default-on Cargo features (`wlf`,
  `fsdb`); `--no-default-features` drops them.

### Linux release binary

The prebuilt `rwave-linux-amd64` is glibc-dynamic (manylinux2014
baseline) so it can `dlopen`. Source builds against
`x86_64-unknown-linux-musl` are static and cannot — `.wlf`/`.fsdb` opens
and external plugins fail with `Dynamic loading not supported`. The
native VCD/FST/GHW core is unaffected.

## Discovery

When rwave needs a backend for a non-native format `<f>`, it resolves the
vtable in this order:

1. **External override** — environment variable `RWAVE_PLUGIN_<F>`
   (uppercase format token), an absolute path to a backend cdylib.
   Example: `RWAVE_PLUGIN_FSDB=/abs/path/to/librwave_fsdb_backend.so`.
   If set and the file exists, rwave `dlopen`s it.
2. **Built-in** — `wlf`/`fsdb` where compiled in (amd64; WLF also on
   windows), the compiled-in vtable.
3. Otherwise rwave emits the "no backend" error (below).

That is the whole rule: one env var per format, or a built-in. No search
path, no registry, no Python/site-packages involvement. The override
always wins, so an external `.fsdb` backend set via `RWAVE_PLUGIN_FSDB`
supersedes the built-in NPI one.

Each backend locates *its own* vendor library separately, at init: the
built-in WLF reads `$RWAVE_WLF_LIB` (→ `libwlf.so`/`.dll`), the built-in FSDB
reads `$RWAVE_FSDB_LIB` (→ `libNPI.so`). Those name the *vendor* `.so`;
`RWAVE_PLUGIN_<EXT>` names the *rwave backend* `.so` — distinct layers.

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

## Distribution

Two channels, by backend kind:

- **Built-in (`wlf`, `fsdb`).** Nothing to distribute — they ship inside
  `rwave-linux-amd64`. The user only supplies the *vendor* library at
  runtime via `$RWAVE_WLF_LIB` / `$RWAVE_FSDB_LIB`.
- **External plugin.** Ship a single backend cdylib
  (`librwave_<format>_backend.so`) however you like — a tarball, a
  release asset, an internal artifact store. The user points
  `$RWAVE_PLUGIN_<FORMAT>` at its absolute path. The plugin may keep its
  own vendor libraries / support files beside the `.so` and locate them
  relative to itself (e.g. via `dladdr`); rwave does not introspect them.
  No wheel, no `pip`, no required on-disk layout.

## Errors rwave emits

| Scenario | Message |
|---|---|
| No backend for the extension | `Error: no backend for .<ext> files. Set RWAVE_PLUGIN_<EXT> to a backend library path to handle this format (see docs/PLUGIN.md).` |
| Built-in requested where it isn't compiled | `Error: <format> support is only available in the linux-x86_64 build.` |
| Plugin / vendor lib found but load failed | `Error: <verbatim from dlopen, init err_out, or the backend's vendor-lib loader>` |
| ABI version mismatch (external) | `Error: <format> backend ABI mismatch (plugin v<X>, rwave expects v<Y>). Rebuild the backend against rwave's current ABI.` |

Plugin authors do not author these messages; rwave generates them. The
contract is that the backend loads cleanly when present and reports a
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

Compile as a `cdylib`, then point `RWAVE_PLUGIN_FOO` at the resulting
`.so`; rwave loads it on the next `.foo` open. (Built-in backends use
this same vtable shape, but their source is compiled into rwave and the
vtable comes from a direct call rather than `dlopen` — see
`crates/rwave/src/plugin/builtin/`.)

## Versioning policy

See the "Three versions, three semantics" section above for the
overview. Concretely:

- `RWAVE_BACKEND_ABI_VERSION` bumps **only** on breaking vtable changes
  (field removed, signature changed, semantic change to an existing
  call). Appending new fields at the end of the vtable does not bump
  it — older plugins continue to work; rwave consults only the fields
  they fill.
- The plugin's version string (the vtable's `version` field) is the
  plugin's own semver. Plugin authors choose when to bump it (vendor dep
  refresh, decoder fix, new vtable field they decided to fill, etc.);
  rwave never reads it for compatibility. The runtime gate is
  `abi_version`.
- Rwave's own version is independent of both of the above.

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

## Known backends

| Format | Kind | Vendor lib | Notes |
|--------|------|-----------|-------|
| `wlf`  | built-in | `libwlf.so` (`$RWAVE_WLF_LIB`) | Mentor/Questa; linux-amd64 |
| `fsdb` | built-in | `libNPI.so` (`$RWAVE_FSDB_LIB`) | Synopsys Verdi NPI; needs a Verdi-Ultra license; linux-amd64 |

To register an external plugin, send a PR adding a row.
