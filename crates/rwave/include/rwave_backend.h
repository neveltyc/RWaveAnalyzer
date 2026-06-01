/*
 * rwave backend ABI v1.
 *
 * External backends implement this ABI in a cdylib, ship it as a wheel,
 * and rwave dlopens it at runtime. See docs/PLUGIN.md for the protocol
 * description, discovery rules, memory ownership and threading
 * semantics, distribution (wheel) layout, and a writing-a-backend
 * tutorial.
 *
 * This header is the source of truth for the binary contract. Backend
 * authors #include it (or vendor a copy pinned to the rwave version they
 * target).
 *
 * ABI versioning: the version sits in the vtable's `abi_version` field
 * rather than the symbol name, so a single `rwave_backend` entry point
 * suffices forever. Bumps to RWAVE_BACKEND_ABI_VERSION are reserved for
 * breaking changes (field removal, signature change, semantic change to
 * an existing call); appending fields at the end of the vtable does not
 * bump it — older rwave reads only the fields it knows about.
 *
 * Licensing: this header alone is released under MIT (same as rwave). It
 * carries no implementation; including it in your project does not affect
 * your project's license choice.
 */

#ifndef RWAVE_BACKEND_H
#define RWAVE_BACKEND_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bump only on breaking vtable changes (field removed, signature changed,
 * semantic change to an existing call). Appending new fields to the end
 * of the vtable does not bump this. Backends fill the `abi_version` field
 * of their vtable with this constant at build time; rwave validates the
 * value before calling any other vtable function. */
#define RWAVE_BACKEND_ABI_VERSION 1u

/* Opaque per-file handle owned by the backend. Each call to
 * RwaveBackend::open returns one; rwave passes it back to every
 * subsequent vtable call until close. */
typedef struct RwaveSession RwaveSession;

/* Format-identity values returned by file_format(). The built-in wellen
 * backend uses VCD/FST/GHW; every external-backend format reports
 * UNKNOWN here. rwave does not maintain per-format enum values for
 * plugin formats — there's no central registry to keep in sync. */
typedef enum {
    RWAVE_FMT_UNKNOWN = 0,
    RWAVE_FMT_VCD     = 1,
    RWAVE_FMT_FST     = 2,
    RWAVE_FMT_GHW     = 3
} RwaveFileFormat;

/* Value formatting class for a variable. */
typedef enum {
    RWAVE_VK_BITS  = 0,  /* 4-state MSB-first ASCII bit string */
    RWAVE_VK_REAL  = 1,  /* IEEE 754 double, rendered as decimal string */
    RWAVE_VK_STR   = 2,  /* opaque string payload */
    RWAVE_VK_EVENT = 3   /* no payload */
} RwaveValueKind;

/* One variable declaration, as yielded by var_decls(). All const char*
 * pointers are owned by the backend and must remain valid for the
 * lifetime of the parent RwaveSession*. */
typedef struct {
    const char     *full_path;   /* hierarchical, dot-separated */
    const char     *scope_path;  /* enclosing scope only */
    uint32_t        width;       /* bits; 1 for scalar/real/string/event */
    const char     *type_str;    /* "wire", "reg", "real", "event", ... */
    RwaveValueKind  kind;
    uint64_t        backend_sid; /* opaque to rwave; aliases share this */
} RwaveVarDecl;

/* Streaming-trace callback. The backend calls this once per change event
 * during load_traces(). value_buf is NUL-terminated; value_len is the
 * length excluding the NUL. rwave copies what it keeps; the buffer is
 * borrowed for the call only. */
typedef void (*RwaveEmit)(
    void           *ctx,
    uint64_t        backend_sid,
    int64_t         time_tick,
    const char     *value_buf,
    uint32_t        value_len
);

/* The backend vtable. Returned by rwave_backend(). Field order is
 * stable; future ABI v1 revisions may only append. */
typedef struct {
    uint32_t        abi_version;   /* must equal RWAVE_BACKEND_ABI_VERSION */
    const char     *name;          /* format token — equals the file
                                      extension this plugin claims, e.g.
                                      "foo" for a plugin handling .foo */
    const char     *version;       /* backend's own version string */

    /* lifecycle */
    RwaveSession   *(*open)(const char *path, char **err_out);
    void            (*close)(RwaveSession *);
    void            (*free_err)(char *err);

    /* metadata */
    RwaveFileFormat (*file_format)(RwaveSession *);
    void            (*timescale)(
                        RwaveSession *,
                        double      *secs_per_tick,
                        const char **display
                    );
    const char     *(*date)(RwaveSession *);
    const char     *(*version_str)(RwaveSession *);
    /* Returns nonzero on success and fills *lo / *hi; zero if the file
     * has no recorded time steps. */
    int             (*time_range)(
                        RwaveSession *,
                        int64_t     *lo,
                        int64_t     *hi
                    );
    size_t          (*time_step_count)(RwaveSession *);

    /* Hierarchy: cap=0 returns total count without touching buf; cap>0
     * fills buf up to cap items and returns the number written (capped
     * at the true total). */
    size_t          (*var_decls)(
                        RwaveSession *,
                        RwaveVarDecl *buf,
                        size_t        cap
                    );

    /* Trace decode: stream change events for the given backend signal
     * ids back via emit(ctx, ...). Returns 0 on success, nonzero on
     * failure. */
    int             (*load_traces)(
                        RwaveSession *,
                        const uint64_t *sids,
                        size_t          n_sids,
                        RwaveEmit       emit,
                        void           *ctx
                    );
} RwaveBackend;

/*
 * The backend's sole exported symbol.
 *
 * Called once per process when rwave first dlopens the backend. Returns
 * a const pointer to a vtable whose abi_version equals
 * RWAVE_BACKEND_ABI_VERSION at backend build time. The vtable lives for
 * the process lifetime.
 *
 * On failure, return NULL and set *err_out to a static, NUL-terminated
 * human-readable string. rwave displays it verbatim. Note: rwave cannot
 * call free_err on it — free_err is a vtable member, and a failed init
 * means the vtable is unavailable. So `*err_out` must be a static string
 * the backend does not intend to free.
 */
const RwaveBackend *rwave_backend(const char **err_out);

#ifdef __cplusplus
}
#endif

#endif /* RWAVE_BACKEND_H */
