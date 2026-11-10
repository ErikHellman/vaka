#ifndef VAKA_NATIVE_H
#define VAKA_NATIVE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define VAKA_IMAGE_MACHO 1
#define VAKA_IMAGE_ELF 2

/// A loaded binary image (main executable, framework, dylib).
typedef struct {
    uint64_t addr;       ///< Runtime load address (Mach-O header / first ELF PT_LOAD).
    uint64_t size;       ///< Size of the text segment (Mach-O) or mapped range (ELF).
    uint8_t id[32];      ///< Mach-O LC_UUID (16 bytes) or ELF GNU build id.
    uint32_t id_len;     ///< 0 when the image has no id.
    int32_t kind;        ///< VAKA_IMAGE_MACHO or VAKA_IMAGE_ELF.
    const char *path;    ///< Image path (owned by the image table, never freed).
} vaka_image_t;

/// Start tracking loaded images. Safe to call more than once.
void vaka_images_init(void);
/// Number of images currently known.
int vaka_image_count(void);
/// Copy image `index` into `out`. Returns 0 on success.
int vaka_image_get(int index, vaka_image_t *out);
/// Index of the image containing `addr`, or -1.
int vaka_image_find(uint64_t addr);

/// Install POSIX signal handlers (SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGABRT, SIGFPE, SIGSYS)
/// that write a crash report to `report_path` with only async-signal-safe calls.
/// Returns 0 on success.
int vaka_crash_install(const char *report_path);
/// Restore the previous signal handlers.
void vaka_crash_uninstall(void);
/// Write a report for an uncaught (Objective-C) exception. Called outside signal context.
/// Later signals in this process (e.g. the SIGABRT that follows) do not overwrite it.
int vaka_crash_write_exception(const char *type, const char *reason, const uintptr_t *addrs, int count);

/// Gzip-compress `len` bytes into `out` (capacity `cap`). Returns the compressed size, or 0.
size_t vaka_gzip(const uint8_t *in, size_t len, uint8_t *out, size_t cap);
/// Upper bound for the gzip output of `len` bytes.
size_t vaka_gzip_bound(size_t len);

/// Walk the calling thread's frame pointers. out[0] is the return address into the caller of
/// this function; `skip` drops that many more frames. Returns the number of frames written.
int vaka_capture_stack(uintptr_t *out, int cap, int skip);
/// Nearest exported symbol for `addr` (dladdr), or NULL. The string is owned by the loader.
const char *vaka_symbol_name(uintptr_t addr, uintptr_t *symbol_addr);

/// Test helper: dereference an invalid pointer.
void vaka_test_crash_segv(void);
/// Test helper: fork a child that installs the handler with `report_path` and crashes with
/// `sig` (SIGSEGV via a bad pointer, anything else via raise). Returns the child's wait status.
int vaka_test_fork_crash(const char *report_path, int sig);

#ifdef __cplusplus
}
#endif

#endif
