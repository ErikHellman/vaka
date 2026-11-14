// Crash capture. Everything reachable from signal_handler() must be async-signal-safe:
// no malloc, no locks, no Objective-C/Swift runtime, no stdio. Memory is read through
// vm_read_overwrite (Darwin) / process_vm_readv (Linux) so a bad frame pointer cannot fault.
#if defined(__linux__)
#define _GNU_SOURCE
#endif

#include "VakaNative.h"

#include <errno.h>
#include <pthread.h>
#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#if defined(__APPLE__)
#include <TargetConditionals.h>
#include <mach/mach.h>
#include <sys/ucontext.h>
#elif defined(__linux__)
#include <sys/ucontext.h>
#include <sys/uio.h>
#endif

#define MAX_FRAMES 128
#define PATH_CAP 1024

static const int k_signals[] = {SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGTRAP, SIGSYS};
#define NUM_SIGNALS ((int)(sizeof k_signals / sizeof k_signals[0]))

static struct sigaction g_previous[NUM_SIGNALS];
static char g_report_path[PATH_CAP];
static volatile int g_installed = 0;
static volatile int g_handling = 0;
static volatile int g_report_written = 0;
static void *g_alt_stack = NULL;

// ---- tiny async-signal-safe writer ----

typedef struct {
    int fd;
    char buf[2048];
    size_t len;
} writer_t;

static void w_flush(writer_t *w) {
    size_t off = 0;
    while (off < w->len) {
        ssize_t n = write(w->fd, w->buf + off, w->len - off);
        if (n < 0) { if (errno == EINTR) continue; break; }
        off += (size_t)n;
    }
    w->len = 0;
}

static void w_char(writer_t *w, char c) {
    if (w->len == sizeof w->buf) w_flush(w);
    w->buf[w->len++] = c;
}

static void w_str(writer_t *w, const char *s) {
    if (!s) return;
    while (*s) w_char(w, *s++);
}

// Writes a string with newlines/CRs replaced, so the line-based format stays intact.
static void w_line_str(writer_t *w, const char *s, size_t max) {
    if (!s) return;
    for (size_t i = 0; s[i] && i < max; i++) {
        char c = s[i];
        w_char(w, (c == '\n' || c == '\r') ? ' ' : c);
    }
}

static void w_hex(writer_t *w, uint64_t v) {
    char tmp[16];
    int n = 0;
    do { tmp[n++] = "0123456789abcdef"[v & 0xf]; v >>= 4; } while (v && n < 16);
    w_str(w, "0x");
    while (n) w_char(w, tmp[--n]);
}

static void w_dec(writer_t *w, int64_t v) {
    char tmp[24];
    int n = 0;
    uint64_t u = v < 0 ? (uint64_t)(-v) : (uint64_t)v;
    if (v < 0) w_char(w, '-');
    do { tmp[n++] = (char)('0' + u % 10); u /= 10; } while (u && n < 24);
    while (n) w_char(w, tmp[--n]);
}

static void w_bytes_hex(writer_t *w, const uint8_t *b, uint32_t len) {
    for (uint32_t i = 0; i < len; i++) {
        w_char(w, "0123456789abcdef"[b[i] >> 4]);
        w_char(w, "0123456789abcdef"[b[i] & 0xf]);
    }
}

// ---- safe memory access ----

static int safe_read(uintptr_t src, void *dst, size_t len) {
#if defined(__APPLE__)
    vm_size_t out = 0;
    kern_return_t kr = vm_read_overwrite(mach_task_self(), (vm_address_t)src, (vm_size_t)len, (vm_address_t)dst, &out);
    return (kr == KERN_SUCCESS && out == len) ? 0 : -1;
#elif defined(__linux__)
    struct iovec local = {dst, len};
    struct iovec remote = {(void *)src, len};
    ssize_t n = process_vm_readv(getpid(), &local, 1, &remote, 1, 0);
    return n == (ssize_t)len ? 0 : -1;
#else
    memcpy(dst, (const void *)src, len);
    return 0;
#endif
}

static uintptr_t strip_pac(uintptr_t addr) {
#if defined(__APPLE__) && defined(__arm64__)
    return addr & 0x0000000fffffffffULL;
#else
    return addr;
#endif
}

// ---- report ----

static const char *signal_name(int sig) {
    switch (sig) {
        case SIGABRT: return "SIGABRT";
        case SIGBUS: return "SIGBUS";
        case SIGFPE: return "SIGFPE";
        case SIGILL: return "SIGILL";
        case SIGSEGV: return "SIGSEGV";
        case SIGTRAP: return "SIGTRAP";
        case SIGSYS: return "SIGSYS";
        default: return "SIGNAL";
    }
}

static void write_images(writer_t *w, const uintptr_t *frames, int count) {
    // Only images that contain a frame: the full list on iOS is ~1000 system images.
    int seen[MAX_FRAMES + 1];
    int nseen = 0;
    for (int i = 0; i < count; i++) {
        int idx = vaka_image_find(frames[i]);
        if (idx < 0) continue;
        int dup = 0;
        for (int j = 0; j < nseen; j++) if (seen[j] == idx) { dup = 1; break; }
        if (dup || nseen > MAX_FRAMES) continue;
        seen[nseen++] = idx;
        vaka_image_t img;
        if (vaka_image_get(idx, &img) != 0) continue;
        w_str(w, "image ");
        w_hex(w, img.addr);
        w_char(w, ' ');
        w_hex(w, img.size);
        w_char(w, ' ');
        w_str(w, img.kind == VAKA_IMAGE_MACHO ? "macho" : "elf");
        w_char(w, ' ');
        if (img.id_len) w_bytes_hex(w, img.id, img.id_len); else w_char(w, '-');
        w_char(w, ' ');
        w_line_str(w, img.path, PATH_CAP);
        w_char(w, '\n');
    }
}

static int open_report(void) {
    if (!g_report_path[0]) return -1;
    return open(g_report_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
}

static void write_header(writer_t *w) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    w_str(w, "vaka-crash 1\ntime ");
    w_dec(w, (int64_t)ts.tv_sec);
    w_char(w, '.');
    int64_t ms = ts.tv_nsec / 1000000;
    if (ms < 100) w_char(w, '0');
    if (ms < 10) w_char(w, '0');
    w_dec(w, ms);
    w_char(w, '\n');
}

static void write_frames(writer_t *w, const uintptr_t *frames, int count) {
    for (int i = 0; i < count; i++) {
        w_str(w, "frame ");
        w_hex(w, frames[i]);
        w_char(w, '\n');
    }
}

static int walk_stack(uintptr_t pc, uintptr_t fp, uintptr_t lr, uintptr_t *frames, int cap) {
    int n = 0;
    if (pc && n < cap) frames[n++] = strip_pac(pc);
    uintptr_t first_ret = 0;
    uintptr_t cur = fp;
    uintptr_t walked[MAX_FRAMES];
    int nw = 0;
    while (cur && (cur & (sizeof(uintptr_t) - 1)) == 0 && nw < cap) {
        uintptr_t rec[2];
        if (safe_read(cur, rec, sizeof rec) != 0) break;
        uintptr_t ret = strip_pac(rec[1]);
        if (ret == 0) break;
        walked[nw++] = ret;
        if (rec[0] <= cur) break;
        cur = rec[0];
    }
    if (nw) first_ret = walked[0];
    // On arm64 a crash in a leaf function has not pushed a frame record yet, so the caller is
    // only in LR. Add it unless the frame walk already starts with it.
    lr = strip_pac(lr);
    if (lr && lr != first_ret && n < cap) frames[n++] = lr;
    for (int i = 0; i < nw && n < cap; i++) frames[n++] = walked[i];
    return n;
}

// Program counter, frame pointer and (arm64) link register of the interrupted thread.
static void context_regs(void *context, uintptr_t *pc, uintptr_t *fp, uintptr_t *lr) {
#if defined(__APPLE__) && defined(__arm64__)
    ucontext_t *uc = (ucontext_t *)context;
    if (uc && uc->uc_mcontext) {
        *pc = (uintptr_t)__darwin_arm_thread_state64_get_pc(uc->uc_mcontext->__ss);
        *fp = (uintptr_t)__darwin_arm_thread_state64_get_fp(uc->uc_mcontext->__ss);
        *lr = (uintptr_t)__darwin_arm_thread_state64_get_lr(uc->uc_mcontext->__ss);
    }
#elif defined(__APPLE__) && defined(__x86_64__)
    ucontext_t *uc = (ucontext_t *)context;
    if (uc && uc->uc_mcontext) {
        *pc = (uintptr_t)uc->uc_mcontext->__ss.__rip;
        *fp = (uintptr_t)uc->uc_mcontext->__ss.__rbp;
    }
#elif defined(__linux__) && defined(__x86_64__)
    ucontext_t *uc = (ucontext_t *)context;
    if (uc) {
        *pc = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
        *fp = (uintptr_t)uc->uc_mcontext.gregs[REG_RBP];
    }
#elif defined(__linux__) && defined(__aarch64__)
    ucontext_t *uc = (ucontext_t *)context;
    if (uc) {
        *pc = (uintptr_t)uc->uc_mcontext.pc;
        *fp = (uintptr_t)uc->uc_mcontext.regs[29];
        *lr = (uintptr_t)uc->uc_mcontext.regs[30];
    }
#else
    (void)context; (void)pc; (void)fp; (void)lr;
#endif
}

static void restore_handlers(void) {
    for (int i = 0; i < NUM_SIGNALS; i++) sigaction(k_signals[i], &g_previous[i], NULL);
}

static void signal_handler(int sig, siginfo_t *info, void *context) {
    if (__sync_bool_compare_and_swap(&g_handling, 0, 1)) {
        if (!g_report_written) {
            uintptr_t pc = 0, fp = 0, lr = 0;
            context_regs(context, &pc, &fp, &lr);
            uintptr_t frames[MAX_FRAMES];
            int count = walk_stack(pc, fp, lr, frames, MAX_FRAMES);
            int fd = open_report();
            if (fd >= 0) {
                writer_t w;
                w.fd = fd;
                w.len = 0;
                write_header(&w);
                w_str(&w, "signal ");
                w_dec(&w, sig);
                w_char(&w, ' ');
                w_str(&w, signal_name(sig));
                w_str(&w, "\ncode ");
                w_dec(&w, info ? info->si_code : 0);
                w_str(&w, "\nfault_addr ");
                w_hex(&w, info ? (uint64_t)(uintptr_t)info->si_addr : 0);
                w_char(&w, '\n');
                write_frames(&w, frames, count);
                write_images(&w, frames, count);
                w_str(&w, "end\n");
                w_flush(&w);
                fsync(fd);
                close(fd);
                g_report_written = 1;
            }
        }
    }
    // Hand the signal to whoever was installed before us (or the default action).
    restore_handlers();
    raise(sig);
}

int vaka_crash_install(const char *report_path) {
    if (!report_path || strlen(report_path) >= PATH_CAP) return -1;
    vaka_images_init();
    strncpy(g_report_path, report_path, PATH_CAP - 1);
    if (!__sync_bool_compare_and_swap(&g_installed, 0, 1)) return 0;

    // Alternate stack so stack overflows on this thread can still be reported.
    // (tvOS prohibits sigaltstack; there, stack overflows are not reported.)
#if !(defined(__APPLE__) && TARGET_OS_TV)
    size_t alt_size = 64 * 1024;
    if (!g_alt_stack) g_alt_stack = malloc(alt_size);
    if (g_alt_stack) {
        stack_t ss;
        memset(&ss, 0, sizeof ss);
        ss.ss_sp = g_alt_stack;
        ss.ss_size = alt_size;
        sigaltstack(&ss, NULL);
    }
#endif

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = signal_handler;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    for (int i = 0; i < NUM_SIGNALS; i++) {
        if (sigaction(k_signals[i], &sa, &g_previous[i]) != 0) memset(&g_previous[i], 0, sizeof g_previous[i]);
    }
    return 0;
}

void vaka_crash_uninstall(void) {
    if (!__sync_bool_compare_and_swap(&g_installed, 1, 0)) return;
    restore_handlers();
}

int vaka_crash_write_exception(const char *type, const char *reason, const uintptr_t *addrs, int count) {
    if (g_report_written) return 0;
    int fd = open_report();
    if (fd < 0) return -1;
    if (count > MAX_FRAMES) count = MAX_FRAMES;
    uintptr_t frames[MAX_FRAMES];
    for (int i = 0; i < count; i++) frames[i] = strip_pac(addrs[i]);
    writer_t w;
    w.fd = fd;
    w.len = 0;
    write_header(&w);
    w_str(&w, "exception_type ");
    w_line_str(&w, type ? type : "NSException", 512);
    w_str(&w, "\nexception_reason ");
    w_line_str(&w, reason ? reason : "", 4096);
    w_char(&w, '\n');
    write_frames(&w, frames, count);
    write_images(&w, frames, count);
    w_str(&w, "end\n");
    w_flush(&w);
    fsync(fd);
    close(fd);
    g_report_written = 1;
    return 0;
}

void vaka_test_crash_segv(void) {
    volatile int *p = (volatile int *)(uintptr_t)8;
    *p = 42;
}

__attribute__((noinline)) int vaka_capture_stack(uintptr_t *out, int cap, int skip) {
    uintptr_t fp = (uintptr_t)__builtin_frame_address(0);
    int n = 0, i = 0;
    while (fp && (fp & (sizeof(uintptr_t) - 1)) == 0 && n < cap && i < 512) {
        uintptr_t rec[2];
        if (safe_read(fp, rec, sizeof rec) != 0) break;
        uintptr_t ret = strip_pac(rec[1]);
        if (ret == 0) break;
        if (i++ >= skip) out[n++] = ret;
        if (rec[0] <= fp) break;
        fp = rec[0];
    }
    return n;
}

// ---- main-thread sampling (app hang detection) ----
// The watchdog thread sends SIGURG to the main thread; the handler walks the interrupted
// context's frame pointers into a static buffer. SIGURG is ignored by default and only used
// for out-of-band socket data, so other uses are chained to the previous handler.

#define SAMPLE_SIGNAL SIGURG

static pthread_t g_main_thread;
static volatile int g_sampler_installed = 0;
static struct sigaction g_previous_sample;
// 0 idle, 1 requested, 2 handler writing, 3 done.
static volatile int g_sample_state = 0;
static uintptr_t g_sample[MAX_FRAMES];
static volatile int g_sample_count = 0;

static void sample_handler(int sig, siginfo_t *info, void *context) {
    if (pthread_equal(pthread_self(), g_main_thread) && __sync_bool_compare_and_swap(&g_sample_state, 1, 2)) {
        uintptr_t pc = 0, fp = 0, lr = 0;
        context_regs(context, &pc, &fp, &lr);
        g_sample_count = walk_stack(pc, fp, lr, g_sample, MAX_FRAMES);
        __sync_synchronize();
        g_sample_state = 3;
        return;
    }
    struct sigaction *prev = &g_previous_sample;
    if (prev->sa_flags & SA_SIGINFO) {
        if (prev->sa_sigaction) prev->sa_sigaction(sig, info, context);
    } else if (prev->sa_handler != SIG_DFL && prev->sa_handler != SIG_IGN && prev->sa_handler) {
        prev->sa_handler(sig);
    }
}

int vaka_sampler_install_main_thread(void) {
    if (!__sync_bool_compare_and_swap(&g_sampler_installed, 0, 1)) return 0;
    g_main_thread = pthread_self();
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = sample_handler;
    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SAMPLE_SIGNAL, &sa, &g_previous_sample) != 0) {
        g_sampler_installed = 0;
        return -1;
    }
    return 0;
}

int vaka_sample_main_thread(uintptr_t *out, int cap, int timeout_ms) {
    if (!g_sampler_installed || !out || cap <= 0) return -1;
    if (!__sync_bool_compare_and_swap(&g_sample_state, 0, 1)) return -1;
    if (pthread_kill(g_main_thread, SAMPLE_SIGNAL) != 0) {
        g_sample_state = 0;
        return -1;
    }
    struct timespec tick = {0, 1000000};
    for (int waited = 0; g_sample_state != 3; waited++) {
        if (waited >= timeout_ms && __sync_bool_compare_and_swap(&g_sample_state, 1, 0)) return -1;
        nanosleep(&tick, NULL);
    }
    __sync_synchronize();
    int n = g_sample_count < cap ? g_sample_count : cap;
    for (int i = 0; i < n; i++) out[i] = g_sample[i];
    g_sample_state = 0;
    return n;
}

#include <sys/wait.h>

__attribute__((noinline)) static void test_crash_inner(int sig) {
    if (sig == SIGSEGV) vaka_test_crash_segv();
    else raise(sig);
}

int vaka_test_fork_crash(const char *report_path, int sig) {
#if defined(__APPLE__) && TARGET_OS_TV
    (void)report_path; (void)test_crash_inner; (void)sig;
    return -1; // fork is prohibited on tvOS
#else
    pid_t pid = fork();
    if (pid < 0) return -1;
    if (pid == 0) {
        vaka_crash_install(report_path);
        test_crash_inner(sig);
        _exit(0);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    return status;
#endif
}
